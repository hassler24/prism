//! Parallel directory enumeration over `FindFirstFileExW` / `FindNextFileW`.
//!
//! Why not `std::fs::read_dir`: it allocates a `PathBuf` + `String` per entry and
//! enumerates with the default small buffer. Raw `FindFirstFileExW` with
//! `FIND_FIRST_EX_LARGE_FETCH` + name-only basic info is several times faster.
//!
//! Why not the MFT / USN journal ("everything" style): `FSCTL_ENUM_USN_DATA` on
//! `\\.\C:` requires the Administrators group. This machine is non-admin, and a
//! background USN reader would violate the zero-resident-RAM rule. So: parallel
//! walk, plus per-directory mtime diffing for cheap refreshes.
//!
//! Ordering contract (the index depends on it): every directory is assigned an id
//! at discovery time, **ids are dense from 0, and every root id is lower than any
//! id discovered under it**. Because a dir's parent is always discovered first,
//! `id == entry index` and `parent < self` hold for free — so paths can be
//! reconstructed with no fixup pass, and entries need no sorting.
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::fuzzy::sigil;
use crate::index::{Entry, ENTRY_DIR, ENTRY_HIDDEN, ENTRY_OFFLINE};
use crate::interop::*;

/// Hard ceiling so a pathological junction loop can never hang the walk.
pub const MAX_DEPTH: u8 = 64;

/// Windows-owned trees that are pure noise and change constantly. Everything
/// else — including node_modules, target, AppData — is indexed: the whole point
/// is finding *every* file, not the subset another tool guesses he wants.
const SKIP_DIRS: &[&str] = &[
    "system volume information",
    "winsxs",
    "config.msi",
    "$recycle.bin",
    "installationinstance",
];

#[inline]
pub(crate) fn skip_dir(lower: &str) -> bool {
    if lower.starts_with('$') {
        return true; // $Recycle.Bin, $Extend, $Orphan...
    }
    SKIP_DIRS.contains(&lower) || lower.starts_with("installationinformation")
}

/// `%LOCALAPPDATA%\Prism` — the index and cursor files. Never indexed: it is
/// Prism's own cache, and every index write creates/removes a file there.
pub fn is_cache_dir(path: &str) -> bool {
    let needle = cache_dir();
    path.len() == needle.len() && path.eq_ignore_ascii_case(needle)
}

/// Would `parent\name` be [`is_cache_dir`]? Compares the leaf name first, so the
/// per-directory cost is a five-character compare and no allocation.
pub fn is_cache_child(parent: &str, name: &str) -> bool {
    let needle = cache_dir();
    let leaf = needle.rsplit('\\').next().unwrap_or(needle);
    if !name.eq_ignore_ascii_case(leaf) {
        return false;
    }
    let mut probe = String::with_capacity(parent.len() + name.len() + 1);
    probe.push_str(parent);
    if !parent.ends_with('\\') {
        probe.push('\\');
    }
    probe.push_str(name);
    probe.eq_ignore_ascii_case(needle)
}

fn cache_dir() -> &'static str {
    static NEEDLE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NEEDLE.get_or_init(|| crate::index::default_dir().to_string_lossy().into_owned())
}

/// One unit of work: enumerate this directory.
pub struct Work {
    /// Entry slot this directory will occupy (assigned at discovery).
    pub slot: u32,
    pub parent: u32,
    pub path: String,
    pub depth: u8,
}

/// A worker-local batch. Names live in `blob`; `RawItem` offsets point into it.
pub struct Chunk {
    pub blob: Vec<u8>,
    pub items: Vec<RawItem>,
}

/// Raw record before it is appended to the final entry arrays.
/// For a file, `link` = its parent dir's entry slot.
/// For a dir, `link` = the slot the dir itself was assigned.
pub struct RawItem {
    pub name_off: u32,
    pub name_len: u16,
    pub flags: u8,
    pub depth: u8,
    pub link: u32,
    pub parent: u32,
    /// Watermark: a directory's creation time, a file's last write.
    pub mtime: i32,
    /// NTFS change time (FileChangeTime): moves on create, write, rename, delete.
    pub change: i32,
    pub bloom: u32,
    pub mask: u64,
    pub tri: [u64; 2],
}

pub struct Stats {
    pub entries: usize,
    pub dirs: usize,
    pub ms: u128,
}

struct Pool {
    /// LIFO work stack (deep-first keeps one tree hot in the file cache).
    stack: Mutex<Vec<Work>>,
    /// Directories enumerated but whose children are not yet pushed.
    outstanding: AtomicUsize,
    next_slot: AtomicU32,
    found: AtomicUsize,
}

/// Walk `roots` with `threads` workers, pushing finished chunks into `tx`.
/// Slot ids 0..roots.len() belong to the roots, in the given order.
pub fn walk(roots: &[String], threads: usize, tx: &std::sync::mpsc::Sender<Chunk>) -> Stats {
    let t0 = Instant::now();
    let roots: Vec<String> = roots.iter().map(|r| normalize_root(r)).collect();
    let pool = Arc::new(Pool {
        stack: Mutex::new(Vec::with_capacity(4096)),
        outstanding: AtomicUsize::new(roots.len()),
        next_slot: AtomicU32::new(roots.len() as u32),
        found: AtomicUsize::new(0),
    });

    // Roots are real entries too (slot == id, parent == self) and must be sent
    // before their children so `parent < self` holds for every entry.
    {
        let mut chunk = Chunk {
            blob: Vec::new(),
            items: Vec::with_capacity(roots.len()),
        };
        let mut st = pool.stack.lock().unwrap();
        for (i, r) in roots.iter().enumerate() {
            let (bloom, mask) = sigil(r.as_bytes());
            let times = stat_times(r);
            chunk.items.push(RawItem {
                name_off: chunk.blob.len() as u32,
                name_len: r.len().min(u16::MAX as usize) as u16,
                flags: ENTRY_DIR,
                depth: 0,
                link: i as u32,
                parent: i as u32, // a root is its own parent
                mtime: times.0,
                change: times.1,
                bloom,
                mask,
                tri: crate::fuzzy::trigram_sigil(r.as_bytes()),
            });
            chunk.blob.extend_from_slice(r.as_bytes());
            st.push(Work {
                slot: i as u32,
                parent: i as u32,
                path: r.clone(),
                depth: 0,
            });
        }
        if !chunk.items.is_empty() {
            let _ = tx.send(chunk);
        }
    }

    let n = threads.clamp(2, 32);
    std::thread::scope(|s| {
        for _ in 0..n {
            let pool = Arc::clone(&pool);
            let tx = tx.clone();
            s.spawn(move || worker(pool, tx));
        }
    });

    Stats {
        entries: pool.found.load(Ordering::Relaxed),
        dirs: pool.next_slot.load(Ordering::Relaxed) as usize,
        ms: t0.elapsed().as_millis(),
    }
}

fn worker(pool: Arc<Pool>, tx: std::sync::mpsc::Sender<Chunk>) {
    let mut chunk = Chunk {
        blob: Vec::with_capacity(128 * 1024),
        items: Vec::with_capacity(4096),
    };
    // Local LIFO first: keeps a subtree cache-hot and cuts lock traffic ~10:1.
    let mut local: Vec<Work> = Vec::with_capacity(512);
    let mut ffd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let mut idle_spins = 0u32;

    loop {
        let job = match local.pop() {
            Some(w) => Some(w),
            None => pool.stack.lock().unwrap().pop(),
        };
        let Some(job) = job else {
            if pool.outstanding.load(Ordering::SeqCst) == 0 {
                break;
            }
            idle_spins += 1;
            if idle_spins > 8 {
                std::thread::sleep(std::time::Duration::from_micros(200));
            } else {
                std::thread::yield_now();
            }
            continue;
        };
        idle_spins = 0;

        let dirs = unsafe { scan_dir(&pool, &job, &mut ffd, &mut chunk) };
        pool.outstanding.fetch_sub(1, Ordering::SeqCst);

        // Keep the subtree local (cache-hot, one lock-free pop per dir); only
        // spill to the shared stack when a directory is very wide.
        if local.len() + dirs.len() <= 4096 {
            local.extend(dirs);
        } else {
            let mut st = pool.stack.lock().unwrap();
            st.extend(dirs);
        }

        if chunk.items.len() >= 8192 {
            let fresh = Chunk {
                blob: Vec::with_capacity(128 * 1024),
                items: Vec::with_capacity(4096),
            };
            if tx.send(std::mem::replace(&mut chunk, fresh)).is_err() {
                return; // collector gone
            }
        }
    }

    if !chunk.items.is_empty() {
        let _ = tx.send(chunk);
    }
}

/// Enumerate `job.path`, appending records to `chunk` and child dirs to `local`.
unsafe fn scan_dir(
    pool: &Pool,
    job: &Work,
    ffd: &mut WIN32_FIND_DATAW,
    chunk: &mut Chunk,
    // NOTE: children are pushed onto pool.stack by the caller loop below; we return
    // them so the worker can keep them local.
) -> Vec<Work> {
    let mut out_children: Vec<Work> = Vec::new();

    let mut pattern = String::with_capacity(job.path.len() + 2);
    pattern.push_str(&job.path);
    if !job.path.ends_with('\\') {
        pattern.push('\\');
    }
    pattern.push('*');

    // A directory's listing bumps its creation time *and* its change time, so both
    // come from one probe of the directory before its own entries are read over the
    // buffer.
    // Only directories the walker descends into need the extra call that yields their
    // change time; file entries are scanned by their parent and need none.
    let dir_change = crate::interop::change_time_secs(&job.path)
        .and_then(|s| i32::try_from(s).ok())
        .unwrap_or(0);
    let h = FindFirstFileExW(
        utf16z(&pattern).as_ptr(),
        FINDEX_INFO_BASIC,
        ffd,
        FINDEX_SEARCH_OPS_NAME_MATCH,
        std::ptr::null(),
        FIND_FIRST_EX_LARGE_FETCH | FIND_FIRST_EX_ON_DISK_ENTRIES_ONLY,
    );
    if h == INVALID_HANDLE_VALUE {
        return out_children; // access denied, gone, or path too long
    }

    loop {
        let name = from_wide(&ffd.cFileName);
        if name == "." || name == ".." {
            if FindNextFileW(h, ffd) == 0 {
                break;
            }
            continue;
        }

        let is_dir = ffd.is_dir();
        if is_dir && (job.depth + 1 > MAX_DEPTH || skip_dir(&name.to_ascii_lowercase())) {
            if FindNextFileW(h, ffd) == 0 {
                break;
            }
            continue;
        }
        // Prism's own cache is not user content, and indexing it would make every
        // index write mark its own directory changed on the next launch.
        if is_dir && is_cache_child(&job.path, &name) {
            if FindNextFileW(h, ffd) == 0 {
                break;
            }
            continue;
        }

        let (bloom, mask) = sigil(name.as_bytes());
        let tri = crate::fuzzy::trigram_sigil(name.as_bytes());
        let off = chunk.blob.len() as u32;
        let len = name.len().min(u16::MAX as usize) as u16;
        chunk
            .blob
            .extend_from_slice(&name.as_bytes()[..len as usize]);

        let mut flags = 0u8;
        if is_dir {
            flags |= ENTRY_DIR;
        }
        if ffd.is_hidden() {
            flags |= ENTRY_HIDDEN;
        }
        if ffd.is_offline() {
            flags |= ENTRY_OFFLINE;
        }

        if is_dir {
            let slot = pool.next_slot.fetch_add(1, Ordering::Relaxed);
            chunk.items.push(RawItem {
                name_off: off,
                name_len: len,
                flags,
                depth: job.depth + 1,
                link: slot,
                parent: job.slot,
                mtime: ffd.mtime() as i32,
                change: dir_change,
                bloom,
                mask,
                tri,
            });
            if !ffd.is_reparse() {
                let mut p = String::with_capacity(job.path.len() + name.len() + 1);
                p.push_str(&job.path);
                if !job.path.ends_with('\\') {
                    p.push('\\');
                }
                p.push_str(&name);
                out_children.push(Work {
                    slot,
                    parent: job.slot,
                    path: p,
                    depth: job.depth + 1,
                });
                pool.outstanding.fetch_add(1, Ordering::SeqCst);
            }
        } else {
            pool.found.fetch_add(1, Ordering::Relaxed);
            chunk.items.push(RawItem {
                name_off: off,
                name_len: len,
                flags,
                depth: job.depth + 1,
                link: job.slot,
                parent: job.slot,
                // A file's watermark is its last write; it has no change-time probe.
                mtime: ffd.mtime() as i32,
                change: 0,
                bloom,
                mask,
                tri,
            });
        }

        if FindNextFileW(h, ffd) == 0 {
            break;
        }
    }
    FindClose(h);
    out_children
}

pub fn normalize_root(r: &str) -> String {
    let t = r.trim_end_matches(['/', '\\']);
    if t.len() == 2 && t.as_bytes()[1] == b':' {
        format!("{}\\", &t[..2])
    } else {
        t.to_string()
    }
}

/// Fixed + removable drives that are present.
pub fn default_roots() -> Vec<String> {
    indexable_drives()
        .into_iter()
        .map(|d| normalize_root(&d))
        .collect()
}

/// Assemble chunks into final arrays.
///
/// Because dir ids are dense from 0 and assigned parent-before-child, the entry
/// index of a dir IS its id: files can be appended in any order and `parent`
/// always points at an already-written entry.
pub fn assemble(
    chunks: &[Chunk],
    root_count: usize,
) -> (Vec<u8>, Vec<Entry>, Vec<u64>, Vec<[u64; 2]>) {
    let mut names: Vec<u8> = Vec::with_capacity(20 * 1024 * 1024);
    let mut entries: Vec<Entry> = Vec::with_capacity(900_000);
    let mut sigs: Vec<u64> = Vec::with_capacity(900_000);
    let mut tris: Vec<[u64; 2]> = Vec::with_capacity(900_000);

    // dir id -> byte offset of its name in the output blob.
    let mut dir_name_off: Vec<u32> = vec![u32::MAX; root_count.max(1)];
    let mut dir_entry: Vec<u32> = vec![u32::MAX; root_count.max(1)];

    // Pass 1: dirs, ordered by id.
    let mut dirs: Vec<(u32, &RawItem, &[u8])> = Vec::with_capacity(60_000);
    for c in chunks {
        for it in &c.items {
            if it.flags & ENTRY_DIR != 0 {
                dirs.push((
                    it.link,
                    it,
                    &c.blob[it.name_off as usize..(it.name_off + it.name_len as u32) as usize],
                ));
            }
        }
    }
    dirs.sort_unstable_by_key(|(id, _, _)| *id);
    if let Some(last) = dirs.last() {
        dir_name_off.resize(last.0 as usize + 1, u32::MAX);
        dir_entry.resize(last.0 as usize + 1, u32::MAX);
    }
    for (id, it, name) in &dirs {
        let entry_idx = entries.len() as u32;
        debug_assert_eq!(*id as usize, entry_idx as usize, "dir ids must be dense");
        let off = names.len() as u32;
        names.extend_from_slice(name);
        entries.push(Entry {
            name_off: off,
            name_len: it.name_len,
            flags: it.flags,
            depth: it.depth,
            parent: it.parent,
            mtime: it.mtime,
            change: it.change,
        });
        sigs.push(crate::index::pack_sig(it.bloom, it.mask));
        tris.push(it.tri);
        dir_name_off[*id as usize] = off;
        dir_entry[*id as usize] = entry_idx;
    }

    // Pass 2: files, parent resolved through the dir table.
    for c in chunks {
        for it in &c.items {
            if it.flags & ENTRY_DIR != 0 {
                continue;
            }
            let parent = match dir_entry.get(it.link as usize) {
                Some(&p) if p != u32::MAX => p,
                _ => continue, // parent dir was skipped (depth/skip-list): drop the orphan
            };
            let off = names.len() as u32;
            names.extend_from_slice(
                &c.blob[it.name_off as usize..(it.name_off + it.name_len as u32) as usize],
            );
            entries.push(Entry {
                name_off: off,
                name_len: it.name_len,
                flags: it.flags,
                depth: it.depth,
                parent,
                mtime: it.mtime,
                change: it.change,
            });
            sigs.push(crate::index::pack_sig(it.bloom, it.mask));
            tris.push(it.tri);
        }
    }

    (names, entries, sigs, tris)
}

/// Watermark + change time for a path the indexer is about to store.
///
/// A directory's listing is what its **creation** time and its **change** time both
/// track (a file's creation time never moves again, so a file's watermark is its last
/// write). Windows exposes no call that reads the change time alone — only the
/// undocumented `NtQueryInformationFile` `FileFullDirectoryInformation` would — so,
/// like Microsoft's own `fsutil usn`, we ask it the other way round: `SetFileTime` with
/// creation/access/write left `-1` ("unchanged") and change "now" stamps the change time
/// to the present without moving a single visible timestamp, and reading it straight
/// back tells us what it was. One call per directory, at build time.
fn stat_times(path: &str) -> (i32, i32) {
    let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
    let ok = unsafe {
        GetFileAttributesExW(
            utf16z(path).as_ptr(),
            GET_FILEEX_INFO_LEVEL_STANDARD,
            &mut data,
        )
    };
    if ok == 0 {
        return (0, 0);
    }
    if data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        let change = crate::interop::change_time_secs(path)
            .and_then(|s| i32::try_from(s).ok())
            .unwrap_or(0);
        (filetime_to_unix(&data.ftCreationTime) as i32, change)
    } else {
        (filetime_to_unix(&data.ftLastWriteTime) as i32, 0)
    }
}

pub fn unix_now() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_of(entries: &[Entry], names: &[u8]) -> Vec<String> {
        entries
            .iter()
            .map(|e| {
                let a = e.name_off as usize;
                String::from_utf8_lossy(&names[a..a + e.name_len as usize]).into_owned()
            })
            .collect()
    }

    #[test]
    fn walks_recursively_and_keeps_every_level() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "prism-walk-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let deep = root.join("alpha").join("beta").join("gamma");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(root.join("top.txt"), b"x").unwrap();
        std::fs::write(root.join("alpha").join("mid.md"), b"x").unwrap();
        std::fs::write(deep.join("bottom.bin"), b"x").unwrap();

        let roots = vec![normalize_root(&root.to_string_lossy())];
        let (tx, rx) = std::sync::mpsc::channel();
        let stats = walk(&roots, 4, &tx);
        drop(tx);
        let chunks: Vec<Chunk> = rx.iter().collect();
        let (names, entries, sigs, tris) = assemble(&chunks, roots.len());

        assert_eq!(sigs.len(), entries.len());
        assert_eq!(tris.len(), entries.len());
        assert!(stats.ms < 10_000);
        let got = names_of(&entries, &names);
        for want in ["top.txt", "alpha", "beta", "gamma", "mid.md", "bottom.bin"] {
            assert!(got.iter().any(|n| n == want), "missing {want} in {got:?}");
        }
        let bottom = entries
            .iter()
            .position(|e| {
                let a = e.name_off as usize;
                &names[a..a + e.name_len as usize] == b"bottom.bin"
            })
            .unwrap();
        let gamma = entries
            .iter()
            .position(|e| {
                let a = e.name_off as usize;
                &names[a..a + e.name_len as usize] == b"gamma"
            })
            .unwrap();
        assert_eq!(entries[bottom].parent, gamma as u32);
        let _ = std::fs::remove_dir_all(&root);
    }
}
