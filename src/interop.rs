//! Minimal Win32 bindings for fast directory enumeration. No `windows` crate:
//! the handful of functions the indexer needs are declared here, which keeps the
//! binary small and the build offline-proof.
use std::ffi::c_void;

pub type HANDLE = *mut c_void;
pub type BOOL = i32;
pub type DWORD = u32;
pub type WCHAR = u16;

pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

/// Only basic info (attrs + times + size) — cheapest level.
pub const FINDEX_INFO_BASIC: DWORD = 1;
/// Name-only enumeration; never let the OS do content (index-service) searching.
pub const FINDEX_SEARCH_OPS_NAME_MATCH: DWORD = 0;
/// Big internal buffers inside the enumeration handle: far fewer round trips.
pub const FIND_FIRST_EX_LARGE_FETCH: DWORD = 0x02;
/// Skip cloud/offline placeholders and their hydration metadata.
pub const FIND_FIRST_EX_ON_DISK_ENTRIES_ONLY: DWORD = 0x04;

pub const FILE_ATTRIBUTE_DIRECTORY: DWORD = 0x0000_0010;
pub const FILE_ATTRIBUTE_HIDDEN: DWORD = 0x0000_0002;
pub const FILE_ATTRIBUTE_SYSTEM: DWORD = 0x0000_0004;
pub const FILE_ATTRIBUTE_REPARSE_POINT: DWORD = 0x0000_0400;
pub const FILE_ATTRIBUTE_TEMPORARY: DWORD = 0x0000_0100;
pub const FILE_ATTRIBUTE_COMPRESSED: DWORD = 0x0000_8000;
pub const FILE_ATTRIBUTE_OFFLINE: DWORD = 0x0000_1000;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FILETIME {
    pub dwLowDateTime: DWORD,
    pub dwHighDateTime: DWORD,
}

/// Memory layout per the working P/Invoke definitions.
#[repr(C)]
pub struct WIN32_FIND_DATAW {
    pub dwFileAttributes: DWORD,
    /// Birth time. For a directory this is the *last listing change*: create a
    /// child inherited from its parent, delete leaves the directory's own, and a
    /// rename stamps a new one. That is why it — and not `LastWriteTime`, which also
    /// moves on in-place writes to a child — is the refresh's watermark.
    pub ftCreationTime: FILETIME,
    pub ftLastAccessTime: FILETIME,
    pub ftLastWriteTime: FILETIME,
    pub nFileSizeHigh: DWORD,
    pub nFileSizeLow: DWORD,
    pub dwReserved0: DWORD,
    pub dwReserved1: DWORD,
    /// UTF-16, NUL-terminated, MAX_PATH (260) — long paths come back in 8.3 form
    /// unless the process manifest opts into long paths, which ours does not need
    /// because we cap the walk depth and skip the pathological trees anyway.
    pub cFileName: [WCHAR; 260],
    pub cAlternateFileName: [WCHAR; 14],
}

#[repr(C)]
#[derive(Default)]
pub struct WIN32_FILE_ATTRIBUTE_DATA {
    pub dwFileAttributes: DWORD,
    pub ftCreationTime: FILETIME,
    pub ftLastAccessTime: FILETIME,
    pub ftLastWriteTime: FILETIME,
    pub nFileSizeHigh: DWORD,
    pub nFileSizeLow: DWORD,
}

#[repr(C)]
#[derive(Default)]
pub struct FILE_BASIC_INFO_FILE {}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BY_HANDLE_FILE_INFORMATION {
    pub dwFileAttributes: DWORD,
    pub ftCreationTime: FILETIME,
    pub ftLastAccessTime: FILETIME,
    pub ftLastWriteTime: FILETIME,
    pub nVolumeSerialNumber: DWORD,
    pub nFileSizeHigh: DWORD,
    pub nFileSizeLow: DWORD,
    pub nNumberOfLinks: DWORD,
    pub nFileIndexHigh: DWORD,
    pub nFileIndexLow: DWORD,
}

extern "system" {
    pub fn GetFileInformationByHandleEx(
        hFile: HANDLE,
        FileInformationClass: u32,
        lpFileInformation: *mut c_void,
        dwBufferSize: DWORD,
    ) -> BOOL;
    pub fn FindFirstFileExW(
        lpFileName: *const WCHAR,
        fInfoLevelId: DWORD,
        lpFindFileData: *mut WIN32_FIND_DATAW,
        fSearchOp: DWORD,
        lpSearchFilter: *const c_void,
        dwAdditionalFlags: DWORD,
    ) -> HANDLE;
    pub fn FindNextFileW(hFindFile: HANDLE, lpFindFileData: *mut WIN32_FIND_DATAW) -> BOOL;
    pub fn FindClose(hFindFile: HANDLE) -> BOOL;
    pub fn GetLastError() -> DWORD;
    pub fn CreateFileW(
        lpFileName: *const WCHAR,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: *const c_void,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    pub fn GetFileAttributesExW(
        lpFileName: *const WCHAR,
        fInfoLevelId: DWORD,
        lpFileInformation: *mut WIN32_FILE_ATTRIBUTE_DATA,
    ) -> BOOL;
    pub fn GetFileInformationByHandle(
        hFile: HANDLE,
        lpFileInformation: *mut BY_HANDLE_FILE_INFORMATION,
    ) -> BOOL;
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    pub fn GetDriveTypeW(lpRootPathName: *const WCHAR) -> DWORD;
    pub fn SetFileInformationByHandle(
        hFile: HANDLE,
        FileInformationClass: u32,
        lpFileInformation: *mut c_void,
        dwBufferSize: DWORD,
    ) -> BOOL;
    pub fn SetFileTime(
        hFile: HANDLE,
        lpCreationTime: *const FILETIME,
        lpLastAccessTime: *const FILETIME,
        lpLastWriteTime: *const FILETIME,
        lpChangeTime: *const FILETIME,
    ) -> BOOL;
}

pub const OPEN_EXISTING: DWORD = 3;
pub const FILE_FLAG_BACKUP_SEMANTICS: DWORD = 0x0200_0000;
/// `SetFileTime`: "do not change this field".
pub const FILETIME_LEAVE_ALONE: DWORD = 0xFFFF_FFFF;
/// `GetFileInformationByHandleEx`: one `FILE_BASIC_INFO`, `FileBasicInfo`.
pub const FILE_BASIC_INFO_CLASS: u32 = 0;
/// `FILE_BASIC_INFO` is four `i64` stamps and one `u32` attributes; the two reserved
/// words that pad it in the C header are kept, because the size is what the API checks.
pub const FILE_BASIC_INFO_SIZE: usize = 40;
/// Offsets of the stamps, in the order the struct declares them.
pub const CREATION: usize = 0;
pub const ACCESS: usize = 8;
pub const WRITE: usize = 16;
pub const CHANGE: usize = 24;
/// 100 ns ticks per second.
pub const TICKS_PER_SEC: i64 = 10_000_000;

pub const GET_FILEEX_INFO_LEVEL_STANDARD: DWORD = 0;
pub const FILE_SHARE_READ: DWORD = 0x0000_0001;
pub const FILE_SHARE_WRITE: DWORD = 0x0000_0002;
pub const FILE_SHARE_DELETE: DWORD = 0x0000_0004;

pub const DRIVE_NO_ROOT_DIR: DWORD = 1;
pub const DRIVE_REMOVABLE: DWORD = 2;
pub const DRIVE_FIXED: DWORD = 3;
pub const DRIVE_REMOTE: DWORD = 4;
pub const DRIVE_CDROM: DWORD = 5;
pub const DRIVE_RAMDISK: DWORD = 6;

/// Win32 error codes (`GetLastError`).
pub const ERROR_FILE_NOT_FOUND: DWORD = 2;
pub const ERROR_PATH_NOT_FOUND: DWORD = 3;
/// CreateFileW: read attributes — all a stat needs.
pub const FILE_READ_ATTRIBUTES: DWORD = 0x0080;
/// CreateFileW: change attributes only. Enough to bump the NTFS change time and
/// nothing else, so a file whose watermark already matches can safely be stamped.
pub const FILE_WRITE_ATTRIBUTES: DWORD = 0x0100;
pub const SYNCHRONIZE: DWORD = 0x0010_0000;
/// Seconds between the NTFS epoch (1601) and the unix epoch. `filetime_to_unix`
/// subtracts this and the directory-timestamp probe adds it back.
pub const FILETIME_UNIX_EPOCH: i64 = 11_644_473_600;

/// Unpack the little-endian `i64` the OS keeps at `off` in a `FILE_BASIC_INFO` copy.
#[inline]
fn info_i64(buf: &[u8; FILE_BASIC_INFO_SIZE], off: usize) -> i64 {
    i64::from_le_bytes(buf[off..off + 8].try_into().expect("8-byte field"))
}

/// NTFS **change** time of `path` as unix seconds, or `None` if the file is gone.
///
/// This one is read-only and cheap; it is what the indexer calls while it walks.
pub fn change_time_secs(path: &str) -> Option<i64> {
    let buf = basic_info(path, FILE_READ_ATTRIBUTES)?;
    let ticks = info_i64(&buf, CHANGE);
    (ticks > 0).then(|| ticks / TICKS_PER_SEC - FILETIME_UNIX_EPOCH)
}

/// Stored **watermark** — creation/access/write/change packed into `FILE_BASIC_INFO`
/// order, little-endian. `change_time_secs` reads its fourth slot; the walker writes
/// the whole thing into one entry.
pub fn watermark_bytes(path: &str) -> Option<[u8; FILE_BASIC_INFO_SIZE]> {
    basic_info(path, FILE_READ_ATTRIBUTES)
}

fn basic_info(path: &str, access: DWORD) -> Option<[u8; FILE_BASIC_INFO_SIZE]> {
    unsafe {
        let h = CreateFileW(
            utf16z(path).as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut buf = [0u8; FILE_BASIC_INFO_SIZE];
        let ok = GetFileInformationByHandleEx(
            h,
            FILE_BASIC_INFO_CLASS,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
        );
        CloseHandle(h);
        (ok != 0).then_some(buf)
    }
}

/// Bump the NTFS **change** time of `path` (a file or a directory) without moving a
/// single visible timestamp: creation, access and write are read back and written
/// unchanged, only the change time moves.
///
/// This is how the index accepts a file after a walk. A build pauses before it lists
/// a directory, so a file that lands mid-walk wears either a change time older than
/// the stamp (it was there) or one newer (it never was). Accepting it marks it with the
/// walk's own stamp, and moving the *parent* directory's stamp with it means a repeat
/// walk sees that stamp already current and adds nothing — so for a file we accepted, past
/// later builds need neither a re-list nor a hash to prove it.
///
/// One write per accepted file and one per touched directory, at build or refresh
/// time; no handle is held once it returns and no state survives the call.
pub fn accept_file(path: &str) -> bool {
    unsafe {
        let h = CreateFileW(
            utf16z(path).as_ptr(),
            FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut buf = [0u8; 40];
        let got = GetFileInformationByHandleEx(
            h,
            FILE_BASIC_INFO_CLASS,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
        );
        if got == 0 {
            CloseHandle(h);
            return false;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let ticks = (now + FILETIME_UNIX_EPOCH) * 10_000_000;
        buf[24..32].copy_from_slice(&ticks.to_le_bytes());
        let ok = SetFileInformationByHandle(
            h,
            FILE_BASIC_INFO_CLASS,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
        );
        CloseHandle(h);
        ok != 0
    }
}

pub fn volume_serial(path: &str) -> Option<u64> {
    unsafe {
        let p = utf16z(&format!("\\\\?\\{}", path));
        let h = CreateFileW(
            p.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let ok = GetFileInformationByHandle(h, &mut info);
        CloseHandle(h);
        if ok == 0 {
            None
        } else {
            Some(info.nVolumeSerialNumber as u64)
        }
    }
}

/// Present drives, filtered to the ones worth indexing.
pub fn indexable_drives() -> Vec<String> {
    let mut out = Vec::new();
    for b in b'A'..=b'Z' {
        let root = format!("{}:\\", b as char);
        let t = unsafe { GetDriveTypeW(utf16z(&root).as_ptr()) };
        if t == DRIVE_FIXED || t == DRIVE_REMOVABLE {
            out.push(root);
        }
    }
    out
}

impl WIN32_FIND_DATAW {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    #[inline]
    pub fn is_hidden(&self) -> bool {
        self.dwFileAttributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
    }
    /// Change time (`LastWriteTime` on the find-data side, FileChangeTime in
    /// `FILE_FULL_DIR_INFO`): moves on creation, in-place writes, renames and
    /// deletes, including the delete of a child. Does not move on mere attribute
    /// changes, which is what kept the `CreationTime` watermark honest.
    #[inline]
    pub fn change_time(&self) -> i64 {
        filetime_to_unix(&self.ftLastWriteTime)
    }
    #[inline]
    pub fn is_reparse(&self) -> bool {
        self.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[inline]
    pub fn is_offline(&self) -> bool {
        self.dwFileAttributes & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_COMPRESSED) != 0
    }
    #[inline]
    pub fn size(&self) -> u64 {
        ((self.nFileSizeHigh as u64) << 32) | self.nFileSizeLow as u64
    }
    /// Mtime stored in the index — the freshness watermark for this entry.
    ///
    /// For a directory that is the **creation** time, not the last-write time:
    /// Windows stamps a *directory* `LastWriteTime` on Unix-style writes that
    /// change nothing in its list (`touch`, an in-place file write), so it flaps
    /// constantly and misleads the mtime refresh into re-listing
    /// directories that never changed. The creation mtime only moves on
    /// add/remove/rename — the event we care about, and the one the change journal
    /// reports exactly. A file keeps its last-write time.
    #[inline]
    pub fn mtime(&self) -> i64 {
        if self.is_dir() {
            filetime_to_unix(&self.ftCreationTime)
        } else {
            filetime_to_unix(&self.ftLastWriteTime)
        }
    }
}

/// UTF-8 → UTF-16 with trailing NUL (Win32 wide-string convention).
pub fn utf16z(s: &str) -> Vec<WCHAR> {
    let mut v: Vec<WCHAR> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// Fixed UTF-16 buffer, NUL-terminated → String (lossy).
pub fn from_wide(w: &[WCHAR]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// NTFS FILETIME (100 ns ticks since 1601) → unix seconds.
pub fn filetime_to_unix(ft: &FILETIME) -> i64 {
    let raw = ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64;
    (raw / 10_000_000) - FILETIME_UNIX_EPOCH
}
