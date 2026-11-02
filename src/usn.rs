//! Non-admin NTFS change feed.
//!
//! `FSCTL_READ_USN_JOURNAL` needs administrator rights, but the unprivileged
//! variant (`FSCTL_READ_UNPRIVILEGED_USN_JOURNAL`) works from a normal desktop
//! process. It does not expose file names, so records are resolved through
//! `OpenFileById` + `FileNameInfo`.
//!
//! Two consumers:
//!
//! * [`start_tail`] — live tail, feeds the "created seconds ago" overlay while
//!   the window is open.
//! * [`scan`] — cold catch-up: turns the journal into the exact set of
//!   directories whose contents changed since the index was written. The parent
//!   id is enough to find them, and it survives the file being deleted, which is
//!   what makes deletions and renames catchable without statting the disk.
//!
//! Deliberately not a resident service: the thread lives only while the Prism
//! window exists, and [`scan`] only runs as part of a refresh.
use std::collections::HashSet;
use std::ffi::c_void;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::index;
use crate::interop::FILE_READ_ATTRIBUTES;
use crate::win32::*;

const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00f4;
const FSCTL_READ_UNPRIVILEGED_USN_JOURNAL: u32 = 0x0009_03ab;

// Reason bits (winioctl.h). These were previously mismatched, which meant live
// tails only ever saw file *creates*: renames were silently dropped.
const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
const USN_REASON_HARD_LINK_CHANGE: u32 = 0x0001_0000;

/// Reasons worth an overlay entry: a name appeared.
const LIVE_MASK: u32 = USN_REASON_FILE_CREATE | USN_REASON_RENAME_NEW_NAME;
/// Reasons that can change a directory's list of children.
const STRUCT_MASK: u32 = USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_RENAME_OLD_NAME
    | USN_REASON_RENAME_NEW_NAME
    | USN_REASON_HARD_LINK_CHANGE;

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const FILE_ID_FILE_ID: i32 = 0;
const FILE_NAME_INFO: u32 = 2;
/// Required to resolve a *directory* by id; harmless for files.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

const READ_BUFFER: usize = 256 * 1024;
const MAX_READ_PAGES: usize = 64;
const MAX_RAW_RECORDS: usize = 2048;
const MAX_CATCHUP_RESOLVE: usize = 512;
const MAX_LIVE_RESOLVE: usize = 128;
const LIVE_POLL_MS: u64 = 250;

/// Ceiling for one [`scan`]. Past these the journal is no longer a cheaper
/// source of truth than the mtime pass, so the caller falls back to that.
const MAX_SCAN_RECORDS: usize = 2_000_000;
const MAX_SCAN_DIRS: usize = 200_000;
const MAX_SCAN_PASSES: usize = 512;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UsnJournalData {
    journal_id: u64,
    first_usn: i64,
    next_usn: i64,
    lowest_valid_usn: i64,
    max_usn: i64,
    maximum_size: u64,
    allocation_delta: u64,
    min_supported_major: u16,
    max_supported_major: u16,
    flags: u32,
    _tail: [u8; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ReadUsnJournalData {
    start_usn: i64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    journal_id: u64,
    min_major: u16,
    max_major: u16,
    _pad: u32,
}

#[repr(C)]
struct FileIdDescriptor {
    size: u32,
    kind: i32,
    file_id: i64,
    _pad: u64,
}

const _: () = assert!(std::mem::size_of::<UsnJournalData>() == 80);
const _: () = assert!(std::mem::size_of::<ReadUsnJournalData>() == 48);
const _: () = assert!(std::mem::size_of::<FileIdDescriptor>() == 24);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub journal_id: u64,
    pub next_usn: i64,
}

#[derive(Clone, Debug)]
pub struct Change {
    pub path: String,
    pub is_dir: bool,
}

#[derive(Clone, Copy)]
struct RawRecord {
    file_id: u64,
    parent_id: u64,
    is_dir: bool,
    reason: u32,
}

struct Volume {
    root: String,
    handle: HANDLE,
    journal_id: u64,
    first_usn: i64,
    next_usn: i64,
}

// The handle is owned by exactly one `Volume`; the thread that creates it is
// also the only thread that reads it.
unsafe impl Send for Volume {}

impl Drop for Volume {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

impl Volume {
    fn open(root: &str) -> Option<Volume> {
        let letter = root.chars().next()?.to_ascii_uppercase();
        let path = wide(&format!("\\\\.\\{letter}:"));
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                FILE_TRAVERSE | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut vol = Volume {
            root: format!("{letter}:\\"),
            handle,
            journal_id: 0,
            first_usn: 0,
            next_usn: 0,
        };
        if !vol.query() {
            return None;
        }
        Some(vol)
    }

    fn query(&mut self) -> bool {
        let mut data = UsnJournalData::default();
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                &mut data as *mut UsnJournalData as *mut c_void,
                std::mem::size_of::<UsnJournalData>() as u32,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || returned < 32 {
            return false;
        }
        self.journal_id = data.journal_id;
        self.first_usn = data.first_usn;
        self.next_usn = data.next_usn;
        true
    }

    fn valid_cursor(&self, cursor: Cursor) -> bool {
        cursor.journal_id == self.journal_id
            && cursor.next_usn >= self.first_usn
            && cursor.next_usn <= self.next_usn
    }

    /// Read journal records from `start_usn`, handing each one that matches
    /// `reason_mask` (and is newer than `min_timestamp`) to `sink`. `sink`
    /// returns `false` to stop early. Returns `(cursor_after_read, count)`.
    fn read_raw(
        &mut self,
        start_usn: i64,
        reason_mask: u32,
        min_timestamp: i64,
        mut sink: impl FnMut(RawRecord) -> bool,
    ) -> io::Result<(i64, usize)> {
        if start_usn < self.first_usn {
            return Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32));
        }
        let mut cursor = start_usn;
        let mut count = 0usize;
        let mut out = vec![0u8; READ_BUFFER];

        for _ in 0..MAX_READ_PAGES {
            let input = ReadUsnJournalData {
                start_usn: cursor,
                reason_mask,
                return_only_on_close: 0,
                timeout: 0,
                bytes_to_wait_for: 0,
                journal_id: self.journal_id,
                min_major: 2,
                max_major: 2,
                _pad: 0,
            };
            let mut returned = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    self.handle,
                    FSCTL_READ_UNPRIVILEGED_USN_JOURNAL,
                    &input as *const ReadUsnJournalData as *const c_void,
                    std::mem::size_of::<ReadUsnJournalData>() as u32,
                    out.as_mut_ptr() as *mut c_void,
                    out.len() as u32,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::from_raw_os_error(
                    unsafe { GetLastError() } as i32
                ));
            }
            if returned < 8 {
                break;
            }
            let next = i64::from_le_bytes(out[0..8].try_into().unwrap());
            let mut off = 8usize;
            while off + 60 <= returned as usize {
                let len = u32::from_le_bytes(out[off..off + 4].try_into().unwrap()) as usize;
                if len < 60 || off + len > returned as usize {
                    break;
                }
                let major = u16::from_le_bytes(out[off + 4..off + 6].try_into().unwrap());
                if major == 2 {
                    let file_id = u64::from_le_bytes(out[off + 8..off + 16].try_into().unwrap());
                    let parent_id = u64::from_le_bytes(out[off + 16..off + 24].try_into().unwrap());
                    let timestamp = i64::from_le_bytes(out[off + 32..off + 40].try_into().unwrap());
                    let reason = u32::from_le_bytes(out[off + 40..off + 44].try_into().unwrap());
                    let attrs = u32::from_le_bytes(out[off + 52..off + 56].try_into().unwrap());
                    if reason & reason_mask != 0 && timestamp >= min_timestamp {
                        count += 1;
                        let keep_going = sink(RawRecord {
                            file_id,
                            parent_id,
                            is_dir: attrs & FILE_ATTRIBUTE_DIRECTORY != 0,
                            reason,
                        });
                        if !keep_going {
                            return Ok((cursor, count));
                        }
                    }
                }
                off += len;
            }
            if next <= cursor {
                break;
            }
            cursor = next;
            self.next_usn = self.next_usn.max(next);
            // A full buffer may have more pages; a short read is the tail.
            if (returned as usize) < READ_BUFFER {
                break;
            }
        }
        Ok((cursor, count))
    }

    /// Resolve a file or directory id to a full path.
    fn resolve(&self, file_id: u64) -> Option<String> {
        let desc = FileIdDescriptor {
            size: std::mem::size_of::<FileIdDescriptor>() as u32,
            kind: FILE_ID_FILE_ID,
            file_id: file_id as i64,
            _pad: 0,
        };
        let handle = unsafe {
            OpenFileById(
                self.handle,
                &desc as *const FileIdDescriptor as *const c_void,
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS,
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut buf = vec![0u16; 32 * 1024];
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle,
                FILE_NAME_INFO,
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * std::mem::size_of::<u16>()) as u32,
            )
        };
        unsafe {
            CloseHandle(handle);
        }
        if ok == 0 {
            return None;
        }
        // FILE_NAME_INFO starts with a u32 byte length, then the UTF-16 name.
        let bytes = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const u32) } as usize;
        let chars = (bytes / 2).min(buf.len().saturating_sub(2));
        if chars == 0 {
            return None;
        }
        let rel = String::from_utf16_lossy(&buf[2..2 + chars]);
        if rel == "\\" {
            // The volume root itself has no leaf name but is still a directory
            // whose mtime changes when something lands directly in it.
            return Some(self.root.clone());
        }
        let path = if rel.starts_with('\\') {
            format!("{}{}", &self.root[..2], rel)
        } else {
            format!("{}\\{}", &self.root[..2], rel)
        };
        if path.is_empty() {
            return None;
        }
        Some(path)
    }
}

fn cursor_path(root: &str) -> PathBuf {
    let letter = root.chars().next().unwrap_or('?').to_ascii_lowercase();
    index::default_dir().join(format!("usn-{letter}.cursor"))
}

fn load_cursor(root: &str) -> Option<Cursor> {
    let bytes = std::fs::read(cursor_path(root)).ok()?;
    if bytes.len() < 16 {
        return None;
    }
    Some(Cursor {
        journal_id: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
        next_usn: i64::from_le_bytes(bytes[8..16].try_into().ok()?),
    })
}

fn save_cursor(root: &str, cursor: Cursor) -> io::Result<()> {
    let path = cursor_path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut bytes = [0u8; 16];
    bytes[0..8].copy_from_slice(&cursor.journal_id.to_le_bytes());
    bytes[8..16].copy_from_slice(&cursor.next_usn.to_le_bytes());
    // Written in place, not via a temp file + rename: creating a new file in the
    // index directory is itself a journal event, which would mark Prism's own
    // directory dirty on every save and force an index rewrite on every launch.
    // 16 bytes in one sector is atomic enough — `load_cursor` rejects anything
    // that fails `valid_cursor`.
    std::fs::write(path, bytes)
}

/// Capture the journal positions *before* an index build. Saving these only
/// after a successful swap means an interrupted refresh is replayed next time.
pub fn snapshot(roots: &[String]) -> Vec<(String, Cursor)> {
    let mut out = Vec::with_capacity(roots.len());
    for root in roots {
        let Some(volume) = Volume::open(root) else {
            continue;
        };
        let cursor = load_cursor(root)
            .filter(|c| volume.valid_cursor(*c))
            .unwrap_or(Cursor {
                journal_id: volume.journal_id,
                next_usn: volume.next_usn,
            });
        out.push((root.clone(), cursor));
    }
    out
}

pub fn save_cursors(cursors: &[(String, Cursor)]) {
    for (root, cursor) in cursors {
        let _ = save_cursor(root, *cursor);
    }
}

/// Canonical directory key used to tell the refresh engine what to re-list:
/// no trailing separator, ASCII lowercase. `C:\` → `c:`,
/// `C:\Users\Hassler\` → `c:\users\hassler`.
pub fn dir_key(path: &str) -> String {
    path.trim_end_matches('\\').to_ascii_lowercase()
}

/// The result of one journal catch-up.
pub struct Changelog {
    /// Directories whose contents changed, as [`dir_key`] strings.
    pub dirs: HashSet<String>,
    /// Journal positions to persist *after* the index reflects this changelog.
    pub cursors: Vec<(String, Cursor)>,
    /// `false` means `dirs` cannot be trusted to be complete — the caller must
    /// fall back to statting directories.
    pub complete: bool,
    /// Why this is incomplete, for logs and `--stats`.
    pub note: &'static str,
    pub records: usize,
}

/// Read every structural change since the saved cursors and reduce it to the set
/// of directories that need re-listing. Deliberately does no directory listing
/// and no statting: a handful of `DeviceIoControl` + `OpenFileById` calls.
pub fn scan(roots: &[String]) -> Changelog {
    let mut log = Changelog {
        dirs: HashSet::new(),
        cursors: Vec::with_capacity(roots.len()),
        complete: true,
        note: "",
        records: 0,
    };
    for root in roots {
        let Some(mut volume) = Volume::open(root) else {
            log.complete = false;
            log.note = "no change journal";
            return log;
        };
        let mut cursor = match load_cursor(root).filter(|c| volume.valid_cursor(*c)) {
            Some(c) => c.next_usn,
            // No usable cursor: replay whatever the journal still holds. If that
            // is not enough, the caps below catch it and mark us incomplete.
            None => volume.first_usn,
        };
        if cursor < volume.first_usn {
            // The journal was reset or rolled past our position.
            log.complete = false;
            log.note = "journal reset";
            return log;
        }
        let mut parents: HashSet<u64> = HashSet::new();
        let mut passes = 0usize;
        loop {
            passes += 1;
            if passes > MAX_SCAN_PASSES || log.records > MAX_SCAN_RECORDS {
                log.complete = false;
                log.note = "change backlog too large";
                return log;
            }
            match volume.read_raw(cursor, STRUCT_MASK, 0, |record| {
                parents.insert(record.parent_id);
                log.records += 1;
                true
            }) {
                Ok((next, _)) => {
                    let moved = next > cursor;
                    cursor = next;
                    // No cursor movement means we are on the tail: catch-up done.
                    if !moved {
                        break;
                    }
                }
                Err(_) => {
                    log.complete = false;
                    log.note = "journal read failed";
                    return log;
                }
            }
        }
        log.cursors.push((
            root.clone(),
            Cursor {
                journal_id: volume.journal_id,
                next_usn: cursor,
            },
        ));
        for parent in parents {
            if log.dirs.len() >= MAX_SCAN_DIRS {
                log.complete = false;
                log.note = "too many changed directories";
                return log;
            }
            // A deleted parent cannot be resolved; its own parent carries the
            // delete record, so re-listing that one removes the subtree.
            let Some(path) = volume.resolve(parent) else {
                continue;
            };
            if is_never_indexed(&path) || crate::walk::is_cache_dir(&path) {
                continue;
            }
            log.dirs.insert(dir_key(&path));
        }
    }
    log
}

/// Trees the indexer skips, so a change inside them can never matter.
fn is_never_indexed(path: &str) -> bool {
    let lower = path.replace('/', "\\").to_ascii_lowercase();
    const SKIPPED: &[&str] = &[
        "\\$recycle.bin\\",
        "\\system volume information\\",
        "\\windows\\winsxs\\",
    ];
    SKIPPED.iter().any(|n| lower.contains(n))
}

fn is_noise(path: &str) -> bool {
    let lower = path.replace('/', "\\").to_ascii_lowercase();
    const NOISE: &[&str] = &[
        "\\$recycle.bin\\",
        "\\system volume information\\",
        "\\windows\\winsxs\\",
        "\\windows\\temp\\",
        "\\appdata\\local\\temp\\",
        "\\inetcache\\",
    ];
    NOISE.iter().any(|n| lower.contains(n))
}

fn emit(volume: &Volume, records: &[RawRecord], limit: usize, tx: &Sender<Change>) -> bool {
    let mut seen = HashSet::new();
    for record in records.iter().rev().take(limit).rev() {
        if record.reason & LIVE_MASK == 0 {
            continue;
        }
        let Some(path) = volume.resolve(record.file_id) else {
            continue;
        };
        if is_noise(&path) {
            continue;
        }
        let key = path.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let change = Change {
            path,
            is_dir: record.is_dir,
        };
        if tx.send(change).is_err() {
            return false;
        }
    }
    true
}

/// Start the catch-up + live tail. The thread exits when Prism exits or when
/// the UI drops its receiver.
pub fn start_tail(roots: Vec<String>, index_built_at: u32, tx: Sender<Change>) {
    std::thread::spawn(move || {
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }

        let min_timestamp = index_built_at as i64 - 300;
        let mut tails = Vec::new();
        for root in roots {
            let Some(mut volume) = Volume::open(&root) else {
                continue;
            };
            let saved = load_cursor(&root).filter(|c| volume.valid_cursor(*c));
            let mut cursor = if index_built_at == 0 {
                volume.next_usn
            } else {
                saved.map(|c| c.next_usn).unwrap_or(volume.first_usn)
            };
            if cursor < volume.next_usn && index_built_at != 0 {
                let mut caught_up = true;
                for _ in 0..8 {
                    let mut records: Vec<RawRecord> = Vec::new();
                    match volume.read_raw(cursor, LIVE_MASK, min_timestamp, |r| {
                        if records.len() == MAX_RAW_RECORDS {
                            records.remove(0);
                        }
                        records.push(r);
                        true
                    }) {
                        Ok((next, _)) => {
                            let done = next >= volume.next_usn || records.is_empty();
                            cursor = next;
                            if !emit(&volume, &records, MAX_CATCHUP_RESOLVE, &tx) {
                                return;
                            }
                            if done {
                                break;
                            }
                        }
                        Err(_) => {
                            caught_up = false;
                            break;
                        }
                    }
                }
                if !caught_up {
                    cursor = volume.next_usn;
                }
            }
            tails.push((volume, cursor));
        }

        loop {
            let mut progressed = false;
            for (volume, cursor) in &mut tails {
                if *cursor < volume.first_usn || *cursor > volume.next_usn {
                    if volume.query() {
                        *cursor = volume.next_usn;
                    }
                    continue;
                }
                let mut records: Vec<RawRecord> = Vec::new();
                match volume.read_raw(*cursor, LIVE_MASK, 0, |r| {
                    if records.len() == MAX_RAW_RECORDS {
                        records.remove(0);
                    }
                    records.push(r);
                    true
                }) {
                    Ok((next, _)) => {
                        if next != *cursor {
                            *cursor = next;
                            progressed = true;
                        }
                        if !records.is_empty() {
                            progressed = true;
                            if !emit(volume, &records, MAX_LIVE_RESOLVE, &tx) {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        if e.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32)
                            && volume.query()
                        {
                            *cursor = volume.next_usn;
                        }
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(if progressed {
                25
            } else {
                LIVE_POLL_MS
            }));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_codec_roundtrip() {
        let cursor = Cursor {
            journal_id: 0x1234_5678_9abc_def0,
            next_usn: 42_000_000,
        };
        let mut bytes = [0u8; 16];
        bytes[0..8].copy_from_slice(&cursor.journal_id.to_le_bytes());
        bytes[8..16].copy_from_slice(&cursor.next_usn.to_le_bytes());
        let decoded = Cursor {
            journal_id: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
            next_usn: i64::from_le_bytes(bytes[8..16].try_into().unwrap()),
        };
        assert_eq!(cursor, decoded);
    }

    #[test]
    fn noise_filter_is_narrow() {
        assert!(is_noise(r"C:\$Recycle.Bin\file"));
        assert!(is_noise(r"C:\Windows\WinSxS\file"));
        assert!(is_noise(r"C:\Users\Hassler\AppData\Local\Temp\file"));
        assert!(!is_noise(r"C:\Users\Hassler\Documents\file"));
        assert!(!is_noise(r"C:\Users\Hassler\dev\Prism\src\main.rs"));
    }

    #[test]
    fn never_indexed_trees_are_skipped() {
        assert!(is_never_indexed(r"C:\$Recycle.Bin\S-1-5-21"));
        assert!(is_never_indexed(
            r"C:\System Volume Information\tracking.log"
        ));
        assert!(is_never_indexed(r"C:\Windows\WinSxS\x86_foo"));
        assert!(!is_never_indexed(r"C:\Users\Hassler\AppData\Local\Temp"));
        assert!(!is_never_indexed(r"C:\Users\Hassler\Documents\Life"));
    }

    #[test]
    fn dir_keys_are_canonical() {
        assert_eq!(dir_key("C:\\"), "c:");
        assert_eq!(dir_key("C:\\Users\\Hassler"), "c:\\users\\hassler");
        assert_eq!(dir_key("C:\\Users\\Hassler\\"), "c:\\users\\hassler");
        assert_eq!(dir_key("D:\\"), dir_key("d:\\"));
    }

    #[test]
    fn reason_bits_match_winioctl() {
        // Regression guard: 0x200 is FILE_DELETE, not RENAME_NEW_NAME. Getting
        // this wrong silently hides every rename from the live overlay.
        assert_eq!(USN_REASON_FILE_CREATE, 0x100);
        assert_eq!(USN_REASON_FILE_DELETE, 0x200);
        assert_eq!(USN_REASON_RENAME_OLD_NAME, 0x1000);
        assert_eq!(USN_REASON_RENAME_NEW_NAME, 0x2000);
        assert_eq!(LIVE_MASK, 0x2100);
        assert_eq!(STRUCT_MASK & USN_REASON_RENAME_OLD_NAME, 0x1000);
    }

    #[test]
    fn read_header_sizes_are_stable() {
        assert_eq!(std::mem::size_of::<ReadUsnJournalData>(), 48);
        assert_eq!(std::mem::size_of::<UsnJournalData>(), 80);
        assert_eq!(std::mem::size_of::<FileIdDescriptor>(), 24);
    }
}
