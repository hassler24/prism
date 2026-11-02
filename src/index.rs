//! On-disk index: one mmap'd file — fixed-size entry records, a name blob, and
//! structure-of-arrays prefilters. Opening it is a `CreateFileMapping` and nothing
//! else, so cold start does not depend on how many files the machine has.
//!
//! ```text
//! header(64) | Entry[16] * n | sig u64 * n | tri[2] u64 * n | names blob
//! ```
//!
//! `sig` packs the prefilter into one 64-bit word: high 32 = 26-bit letter mask,
//! low 32 = character bloom. One AND+CMP per entry rejects ~95% of 1.9M names
//! without touching a name byte; only survivors are scored.
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use crate::fuzzy::{score_name, Query};
use crate::walk::{self, Chunk, Stats};

pub const MAGIC: [u8; 4] = *b"PRSM";
/// Bump when the record layout changes — stale files are rebuilt, never misread.
pub const VERSION: u32 = 6;

pub const ENTRY_DIR: u8 = 1;
pub const ENTRY_HIDDEN: u8 = 2;
pub const ENTRY_OFFLINE: u8 = 4;

type Hits = Vec<(i32, u32, u8)>;

/// 20 bytes. `parent` is an entry index; directories are written before anything
/// inside them, so `parent < index` always holds (roots are their own parent).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Entry {
    pub name_off: u32,
    pub name_len: u16,
    pub flags: u8,
    pub depth: u8,
    pub parent: u32,
    pub mtime: i32,
    pub change: i32,
}

const _: () = assert!(std::mem::size_of::<Entry>() == 20);

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Header {
    pub magic: [u8; 4],
    pub version: u32,
    pub entry_count: u32,
    pub root_count: u32,
    pub built_at: u32,
    pub flags: u32,
    pub names_len: u64,
    pub volume_serial: u64,
    pub reserved: [u32; 6],
}

const _: () = assert!(std::mem::size_of::<Header>() == 64);

/// `%LOCALAPPDATA%\Prism\index.prsm` — outside the repo, so the binary stays portable.
pub fn default_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        })
        .join("Prism")
}

pub fn default_index_path() -> PathBuf {
    default_dir().join("index.prsm")
}

/// Read-only view over a built index.
pub struct Index {
    mmap: Mmap,
    pub header: Header,
    /// Byte offset where each array begins, resolved once at open.
    off_entries: usize,
    off_sigs: usize,
    off_tris: usize,
    off_names: usize,
}

// SAFETY: the only state is a read-only file mapping plus integer offsets; no
// interior mutability, so sharing across threads is sound.
unsafe impl Send for Index {}
unsafe impl Sync for Index {}

const HDR: usize = std::mem::size_of::<Header>();

/// Alignment of the u64 arrays, rounded up so a 20-byte `Entry` still lands them
/// on whole words. `from_raw_parts` requires it: an unaligned `&[u64]` is UB, even
/// on x86, where it mostly shows up as a panic inside `is_some_and` at search time.
pub const WORD: usize = 8;

#[inline]
fn pad(n: usize) -> usize {
    (n + WORD - 1) & !(WORD - 1)
}

/// Byte offsets of the SoA arrays. Both the writer and the reader derive the
/// layout from this one function so a format change cannot desync them.
#[inline]
pub fn offsets(n: usize) -> (usize, usize, usize, usize) {
    let off_entries = HDR;
    let off_sigs = pad(off_entries + n * std::mem::size_of::<Entry>());
    let off_tris = off_sigs + pad(n * 8);
    let off_names = off_tris + pad(n * 16);
    (off_entries, off_sigs, off_tris, off_names)
}

/// Pack `(bloom, mask)` into the single word the index stores.
#[inline]
pub fn pack_sig(bloom: u32, mask: u64) -> u64 {
    ((mask & 0x03FF_FFFF) << 32) | bloom as u64
}

impl Index {
    pub fn open(path: &Path) -> Option<Index> {
        let file = File::open(path).ok()?;
        // SAFETY: the file is mapped read-only and never written while mapped.
        let mmap = unsafe { Mmap::map(&file).ok()? };
        if mmap.len() < HDR {
            return None;
        }
        // SAFETY: length checked above; Header is POD and the mapping base is page-aligned.
        let header = unsafe { std::ptr::read_unaligned(mmap.as_ptr() as *const Header) };
        if header.magic != MAGIC || header.version != VERSION {
            return None;
        }
        let n = header.entry_count as usize;
        let (off_entries, off_sigs, off_tris, off_names) = offsets(n);
        let need = off_names + header.names_len as usize;
        if mmap.len() < need {
            return None; // truncated write — treat as absent, rebuild
        }
        // Cheap sanity checks against a half-written file: the header is written
        // last, so a zero-filled data region shows up as a bogus entry 0.
        if n == 0 || header.root_count == 0 || header.root_count > 64 || header.names_len == 0 {
            return None;
        }
        // SAFETY: index 0 is inside the entries array, which `need` proved exists.
        let first =
            unsafe { std::ptr::read_unaligned(mmap.as_ptr().add(off_entries) as *const Entry) };
        if first.flags & ENTRY_DIR == 0 || first.parent != 0 {
            return None;
        }
        Some(Index {
            mmap,
            header,
            off_entries,
            off_sigs,
            off_tris,
            off_names,
        })
    }

    /// Ask Windows to prefetch the filter arrays ahead of the first query.
    /// Limited to the arrays every search scans; this is only a hint, not a block.
    pub fn prefetch(&self) {
        const PAGE: usize = 4096;
        let start = self.off_sigs & !(PAGE - 1);
        let end = (self.off_names + PAGE - 1) & !(PAGE - 1);
        let len = (end - start).min(self.mmap.len() - start);
        let range = crate::win32::WIN32_MEMORY_RANGE_ENTRY {
            VirtualAddress: unsafe { self.mmap.as_ptr().add(start) as *mut std::ffi::c_void },
            NumberOfBytes: len,
        };
        unsafe {
            crate::win32::PrefetchVirtualMemory(crate::win32::GetCurrentProcess(), 1, &range, 0);
        }
    }

    #[inline]
    pub fn entries(&self) -> &[Entry] {
        unsafe {
            std::slice::from_raw_parts(
                self.mmap.as_ptr().add(self.off_entries) as *const Entry,
                self.header.entry_count as usize,
            )
        }
    }

    #[inline]
    pub fn sigs(&self) -> &[u64] {
        unsafe {
            std::slice::from_raw_parts(
                self.mmap.as_ptr().add(self.off_sigs) as *const u64,
                self.header.entry_count as usize,
            )
        }
    }

    #[inline]
    pub fn tris(&self) -> &[[u64; 2]] {
        unsafe {
            std::slice::from_raw_parts(
                self.mmap.as_ptr().add(self.off_tris) as *const [u64; 2],
                self.header.entry_count as usize,
            )
        }
    }

    #[inline]
    pub fn names(&self) -> &[u8] {
        &self.mmap[self.off_names..self.off_names + self.header.names_len as usize]
    }

    #[inline]
    pub fn name(&self, e: &Entry) -> &[u8] {
        let n = self.names();
        let a = e.name_off as usize;
        &n[a..a + e.name_len as usize]
    }

    #[inline]
    pub fn count(&self) -> usize {
        self.header.entry_count as usize
    }

    pub fn name_str(&self, i: usize) -> String {
        String::from_utf8_lossy(self.name(&self.entries()[i])).into_owned()
    }

    #[inline]
    pub fn is_dir(&self, i: usize) -> bool {
        self.entries()[i].flags & ENTRY_DIR != 0
    }

    /// `C:\Users\Hassler\Documents\file.md` for entry `i`.
    pub fn full_path(&self, i: usize) -> String {
        let entries = self.entries();
        let mut parts: Vec<&[u8]> = Vec::with_capacity(8);
        let mut cur = i;
        for _ in 0..64 {
            let e = &entries[cur];
            parts.push(self.name(e));
            if e.parent == cur as u32 {
                break;
            }
            cur = e.parent as usize;
        }
        let mut out = String::with_capacity(64);
        for (k, p) in parts.iter().rev().enumerate() {
            let s = String::from_utf8_lossy(p);
            if k > 0 && !out.ends_with('\\') {
                out.push('\\');
            }
            out.push_str(&s);
        }
        out
    }

    /// Directory containing entry `i` (shown as the quiet second line in the UI).
    pub fn parent_path(&self, i: usize) -> String {
        let e = self.entries()[i];
        if e.parent == i as u32 {
            String::new()
        } else {
            self.full_path(e.parent as usize)
        }
    }

    /// Top `cap` hits, best first: `(entry, score, tokens matched)`.
    ///
    /// Tier 2 = every token matched. Tier 1 = at least one. Tier 1 only backfills
    /// unused slots, which is what kills the "exactly zero results" failure mode
    /// of Windows Search without letting garbage outrank a real hit.
    pub fn search(&self, q: &Query, qcase: &[bool], cap: usize) -> Vec<(u32, i32, u8)> {
        self.search_with_total(q, qcase, cap).0
    }

    /// Like [`search`](Self::search), plus the number of matching entries across
    /// the whole index, not just the returned page.
    pub fn search_with_total(
        &self,
        q: &Query,
        qcase: &[bool],
        cap: usize,
    ) -> (Vec<(u32, i32, u8)>, usize) {
        if cap == 0 {
            return (Vec::new(), 0);
        }
        let entries = self.entries();

        // Fast tier: literal trigrams are present, so every hit is a real
        // substring (or near-substring). This is the per-keystroke path.
        if let Some(tq) = q.tri {
            let (mut best, _, total) = self.collect_parallel(q, qcase, cap, Some(tq));
            rank_hits(entries, &mut best);
            if best.len() >= cap && best.first().map(|h| h.0 >= 220).unwrap_or(false) {
                return (
                    best.into_iter()
                        .take(cap)
                        .map(|(s, i, t)| (i, s, t))
                        .collect(),
                    total,
                );
            }
        }

        // Fuzzy tier: subsequence matching, still filtered by the cheap
        // character signature. Only reached for typos / short queries.
        let (mut best, mut partial, total) = self.collect_parallel(q, qcase, cap, None);
        rank_hits(entries, &mut best);
        rank_hits(entries, &mut partial);

        let mut out: Vec<(u32, i32, u8)> = best
            .into_iter()
            .take(cap)
            .map(|(s, i, t)| (i, s, t))
            .collect();
        if out.len() < cap {
            for (s, i, t) in partial {
                if out.len() >= cap {
                    break;
                }
                out.push((i, s, t));
            }
        }
        (out, total)
    }

    fn collect_parallel(
        &self,
        q: &Query,
        qcase: &[bool],
        cap: usize,
        tri: Option<[u64; 2]>,
    ) -> (Hits, Hits, usize) {
        let n = self.sigs().len();
        let threads = search_threads(n);
        if threads <= 1 {
            let mut best = Vec::with_capacity(256);
            let mut partial = Vec::with_capacity(cap * 4);
            let total = self.collect_range(0..n, q, qcase, cap, tri, (&mut best, &mut partial));
            return (best, partial, total);
        }

        let chunk = n.div_ceil(threads);
        let ranges: Vec<_> = (0..threads)
            .map(|t| {
                let start = t * chunk;
                let end = ((t + 1) * chunk).min(n);
                start..end
            })
            .filter(|r| !r.is_empty())
            .collect();

        std::thread::scope(|scope| {
            let handles: Vec<_> = ranges
                .into_iter()
                .map(|range| {
                    scope.spawn(move || {
                        let mut best = Vec::with_capacity(256);
                        let mut partial = Vec::with_capacity(cap * 4);
                        let total = self.collect_range(
                            range,
                            q,
                            qcase,
                            cap,
                            tri,
                            (&mut best, &mut partial),
                        );
                        (best, partial, total)
                    })
                })
                .collect();

            let mut best = Vec::new();
            let mut partial = Vec::new();
            let mut total = 0;
            for handle in handles {
                let (b, p, t) = handle
                    .join()
                    .unwrap_or_else(|_| (Vec::new(), Vec::new(), 0));
                best.extend(b);
                partial.extend(p);
                total += t;
            }
            (best, partial, total)
        })
    }

    fn collect_range(
        &self,
        range: std::ops::Range<usize>,
        q: &Query,
        qcase: &[bool],
        cap: usize,
        tri: Option<[u64; 2]>,
        out: (&mut Hits, &mut Hits),
    ) -> usize {
        let (best, partial) = out;
        let entries = self.entries();
        let sigs = self.sigs();
        let tris = self.tris();
        let names = self.names();

        let req = pack_sig(q.bloom_and, q.mask_and);
        let any = (q.mask_or & 0x03FF_FFFF) << 32;
        let want = q.tokens.len();
        let partial_wanted = want > 1;
        let cap_partial = cap * 4;
        let strong = tri.is_some();
        let mut pass = 0usize;
        let mut total = 0usize;

        for i in range {
            if let Some(t) = tri {
                let s = tris[i];
                if (s[0] & t[0]) != t[0] || (s[1] & t[1]) != t[1] {
                    continue;
                }
            }
            let s = sigs[i];
            let and_ok = (s & req) == req;
            if !and_ok && (strong || !(partial_wanted && (s & any) != 0)) {
                continue;
            }
            pass += 1;
            let e = &entries[i];
            let a = e.name_off as usize;
            let name = &names[a..a + e.name_len as usize];
            let (matched, sc) = score_name(name, q, qcase);
            if matched == 0 {
                continue;
            }
            total += 1;
            if and_ok && matched as usize == want {
                best.push((sc, i as u32, 2));
                // Bound the winner set: flushes are rare, and keeping 8x the
                // requested cap preserves the true top `cap` even on "a".
                if best.len() >= 8192 {
                    rank_hits(entries, best);
                    best.truncate((cap * 8).max(128));
                }
            } else if !strong && partial.len() < cap_partial {
                partial.push((sc, i as u32, matched));
            }
        }

        if std::env::var_os("PRISM_TRACE").is_some() {
            eprintln!(
                "trace: strong={strong} entries={} pass={} best={} partial={}",
                sigs.len(),
                pass,
                best.len(),
                partial.len()
            );
        }
        total
    }
}

/// Touch the arrays every query scans so the first search does not page-fault
/// through 95 MB. Safe to call on a throwaway thread: the pages land in the
/// file cache, which the window's own mapping then hits warm.
pub fn warm(path: &Path) {
    let Some(idx) = Index::open(path) else {
        return;
    };
    let start = idx.off_sigs;
    let end = (idx.off_tris + idx.header.entry_count as usize * 16).min(idx.mmap.len());
    let base = idx.mmap.as_ptr();
    let mut off = start;
    while off < end {
        // SAFETY: `off` stays inside the mapping; one volatile byte per page is
        // enough to fault it in.
        unsafe {
            std::hint::black_box(std::ptr::read_volatile(base.add(off)));
        }
        off += 4096;
    }
}

fn search_threads(n: usize) -> usize {
    if n < 100_000 {
        return 1;
    }
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1)
        .clamp(1, 8)
}

fn rank_hits(entries: &[Entry], v: &mut [(i32, u32, u8)]) {
    v.sort_unstable_by(|a, b| {
        b.0.cmp(&a.0)
            // files before dirs at equal score — he usually wants a file
            .then_with(|| {
                let da = (entries[a.1 as usize].flags & ENTRY_DIR != 0) as u8;
                let db = (entries[b.1 as usize].flags & ENTRY_DIR != 0) as u8;
                da.cmp(&db)
            })
    });
}

/// Walk `roots`, assemble, write atomically. Blocking; run it off the UI thread.
pub fn build(roots: &[String], out: &Path, threads: usize) -> std::io::Result<Stats> {
    let t0 = std::time::Instant::now();
    let (tx, rx) = std::sync::mpsc::channel::<Chunk>();

    // Collector on its own thread so the walkers never stall on disk writes.
    let collector = std::thread::spawn(move || {
        let mut chunks: Vec<Chunk> = Vec::with_capacity(64);
        while let Ok(c) = rx.recv() {
            chunks.push(c);
        }
        chunks
    });

    let stats = walk::walk(roots, threads, &tx);
    drop(tx);
    let chunks = collector.join().unwrap_or_default();

    let (names, entries, sigs, tris) = walk::assemble(&chunks, roots.len());
    write_index(&entries, &sigs, &tris, &names, out, roots.len())?;

    Ok(Stats {
        entries: entries.len(),
        dirs: stats.dirs,
        ms: t0.elapsed().as_millis(),
    })
}

fn trace(what: &str, t: std::time::Instant) {
    if std::env::var_os("PRISM_TRACE").is_some() {
        eprintln!("trace: {what} {:?}", t.elapsed());
    }
}

fn write_pod<T>(w: &mut impl Write, v: &[T]) -> std::io::Result<()> {
    let n = std::mem::size_of_val(v);
    if n == 0 {
        return Ok(());
    }
    // SAFETY: callers only pass POD slices (Entry/u64/[u64; 2]).
    w.write_all(unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, n) })
}

/// `Entry` is a plain record of integers, so a byte compare is a value compare.
/// Slicing it means we never allocate a second 32 MB copy just to look.
fn same_entries(a: &[Entry], b: &[Entry]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // SAFETY: Entry and u8 are both plain old data, and the lengths line up.
    let (a, b) = unsafe {
        (
            std::slice::from_raw_parts(a.as_ptr() as *const u8, std::mem::size_of_val(a)),
            std::slice::from_raw_parts(b.as_ptr() as *const u8, std::mem::size_of_val(b)),
        )
    };
    a == b
}

/// `true` if the index at `out` already holds exactly these arrays, compared
/// without copying them: the arrays are borrowed back out of the mapping and the
/// name blob is compared one 1 MiB chunk at a time.
fn already_matches(
    out: &Path,
    entries: &[Entry],
    sigs: &[u64],
    tris: &[[u64; 2]],
    names: &[u8],
    root_count: usize,
) -> bool {
    let Some(idx) = Index::open(out) else {
        return false;
    };
    if idx.count() != entries.len()
        || idx.header.names_len as usize != names.len()
        || idx.header.root_count as usize != root_count
        || !same_entries(idx.entries(), entries)
        || idx.sigs() != sigs
        || idx.tris() != tris
    {
        return false;
    }
    // Name blob: compare in 1 MiB chunks so no second full copy is resident.
    let lhs = idx.names();
    const CHUNK: usize = 1 << 20;
    let mut off = 0usize;
    while off < lhs.len() {
        let end = (off + CHUNK).min(lhs.len());
        if lhs[off..end] != names[off..end] {
            return false;
        }
        off = end;
    }
    true
}

/// Write only if the on-disk index differs from these arrays. Returns `true` when
/// a write happened.
#[allow(clippy::too_many_arguments)]
pub fn write_if_changed(
    entries: &[Entry],
    sigs: &[u64],
    tris: &[[u64; 2]],
    names: &[u8],
    out: &Path,
    root_count: usize,
) -> std::io::Result<bool> {
    if already_matches(out, entries, sigs, tris, names, root_count) {
        return Ok(false);
    }
    write_index(entries, sigs, tris, names, out, root_count)?;
    Ok(true)
}

/// Write header + arrays + blob to a temp file, then rename over the live one.
///
/// The header carries the magic tag, so an interrupted write fails `Index::open`
/// and is rebuilt rather than opened as a half-empty index.
///
/// Deliberately **not** `fsync`'d: the index is a cache, and one interrupted build
/// costs a rebuild — not data. Waiting on 133 MB of writeback instead stalled the
/// process for 7–8 s and hogged the volume's flush queue while it did.
#[allow(clippy::too_many_arguments)]
pub fn write_index(
    entries: &[Entry],
    sigs: &[u64],
    tris: &[[u64; 2]],
    names: &[u8],
    out: &Path,
    root_count: usize,
) -> std::io::Result<()> {
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // write_index moves the live index aside as `.bak`; when the live path *is* a
    // backup (index.prsm <- index.new.prsm), that stale `.prsm.bak` must not be left
    // behind to keep the previous 135 MB on disk.
    let _ = std::fs::remove_file(out.with_extension("prsm.bak"));
    let tmp = out.with_extension("prsm.tmp");
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp)?;

    let header = Header {
        magic: MAGIC,
        version: VERSION,
        entry_count: entries.len() as u32,
        root_count: root_count as u32,
        built_at: walk::unix_now(),
        flags: 0,
        names_len: names.len() as u64,
        volume_serial: crate::interop::volume_serial("C:\\").unwrap_or(0),
        reserved: [0; 6],
    };
    // SAFETY: Header is POD.
    f.write_all(unsafe {
        std::slice::from_raw_parts(
            &header as *const Header as *const u8,
            std::mem::size_of::<Header>(),
        )
    })?;
    let t_buf = std::time::Instant::now();
    {
        let mut buf = BufWriter::with_capacity(4 << 20, &mut f);
        write_pod(&mut buf, entries)?;
        // 20-byte entries leave the u64 arrays off a word boundary; pad to where
        // `offsets` expects them so the mapping can hand out aligned `&[u64]`.
        let gap = offsets(entries.len()).1 - (HDR + std::mem::size_of_val(entries));
        buf.write_all(&[0u8; WORD][..gap])?;
        write_pod(&mut buf, sigs)?;
        write_pod(&mut buf, tris)?;
        buf.write_all(names)?;
        buf.flush()?;
    }
    trace("buffered write", t_buf);
    let t_close = std::time::Instant::now();
    drop(f);
    trace("close", t_close);

    // Keep the previous index one build long, then swap. Unlinking first means a
    // failure here leaves the *previous* index in place and the new one in `.tmp`.
    let bak = out.with_extension("prsm.bak");
    let _ = std::fs::remove_file(&bak);
    if out.exists() && std::fs::rename(out, &bak).is_err() {
        // Something still holds the live index: leave it alone and keep the new
        // copy aside rather than replacing a file we know is in use.
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "live index is in use",
        ));
    }
    std::fs::rename(&tmp, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuzzy::{case_flags, Query};

    fn fixture() -> (Vec<Entry>, Vec<u8>) {
        let mut names = Vec::new();
        let mut entries: Vec<Entry> = Vec::new();
        let add =
            |name: &str, flags: u8, parent: u32, entries: &mut Vec<Entry>, names: &mut Vec<u8>| {
                entries.push(Entry {
                    name_off: names.len() as u32,
                    name_len: name.len() as u16,
                    flags,
                    depth: 0,
                    parent,
                    mtime: 0,
                    change: 0,
                });
                names.extend_from_slice(name.as_bytes());
            };
        add("C:\\", ENTRY_DIR, 0, &mut entries, &mut names); // 0 root
        add("Users", ENTRY_DIR, 0, &mut entries, &mut names); // 1
        add("Hassler", ENTRY_DIR, 1, &mut entries, &mut names); // 2
        add("Documents", ENTRY_DIR, 2, &mut entries, &mut names); // 3
        add("AGENTS.md", 0, 3, &mut entries, &mut names); // 4
        add("trailer_final_v3.mp4", 0, 3, &mut entries, &mut names); // 5
        add("prism.exe", 0, 1, &mut entries, &mut names); // 6
        (entries, names)
    }

    fn build_index(entries: &[Entry], names: &[u8]) -> Index {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let sig = |i: usize| {
            crate::fuzzy::sigil(
                &names[entries[i].name_off as usize
                    ..(entries[i].name_off + entries[i].name_len as u32) as usize],
            )
        };
        let sigs: Vec<u64> = (0..entries.len())
            .map(|i| {
                let (bloom, mask) = sig(i);
                pack_sig(bloom, mask)
            })
            .collect();
        let tris: Vec<[u64; 2]> = (0..entries.len())
            .map(|i| {
                let a = entries[i].name_off as usize;
                let b = a + entries[i].name_len as usize;
                crate::fuzzy::trigram_sigil(&names[a..b])
            })
            .collect();
        let dir = std::env::temp_dir().join(format!(
            "prism-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.prsm");
        write_index(entries, &sigs, &tris, names, &p, 1).unwrap();
        Index::open(&p).expect("open index")
    }

    fn hits(idx: &Index, q: &str, cap: usize) -> Vec<(String, i32, u8)> {
        let qq = Query::parse(q);
        let cs = case_flags(q);
        idx.search(&qq, &cs, cap)
            .into_iter()
            .map(|(i, s, t)| (idx.name_str(i as usize), s, t))
            .collect()
    }

    #[test]
    fn roundtrip_and_paths() {
        let (e, n) = fixture();
        let idx = build_index(&e, &n);
        assert_eq!(idx.count(), 7);
        assert_eq!(idx.full_path(4), "C:\\Users\\Hassler\\Documents\\AGENTS.md");
        assert_eq!(idx.parent_path(4), "C:\\Users\\Hassler\\Documents");
        assert!(idx.is_dir(1));
        assert!(!idx.is_dir(4));
        assert_eq!(idx.full_path(6), "C:\\Users\\prism.exe");
    }

    #[test]
    fn ranks_expected_files_first() {
        let (e, n) = fixture();
        let idx = build_index(&e, &n);
        assert_eq!(hits(&idx, "agents", 10)[0].0, "AGENTS.md");
        assert_eq!(hits(&idx, "trailer", 10)[0].0, "trailer_final_v3.mp4");
        assert_eq!(hits(&idx, "prism exe", 10)[0].0, "prism.exe");
    }

    #[test]
    fn never_hard_zero_when_something_fits() {
        let (e, n) = fixture();
        let idx = build_index(&e, &n);
        // One impossible token out of two: the partial tier still surfaces a hit.
        let h = hits(&idx, "documents zzzqq", 10);
        assert!(!h.is_empty(), "partial tier should rescue this");
        assert_eq!(h[0].2, 1, "must be flagged as a partial match");
    }

    #[test]
    fn impossible_query_returns_nothing() {
        let (e, n) = fixture();
        let idx = build_index(&e, &n);
        assert!(hits(&idx, "qqqzzzwww", 10).is_empty());
    }

    #[test]
    fn reports_total_beyond_the_requested_page() {
        let (e, n) = fixture();
        let idx = build_index(&e, &n);
        let q = Query::parse("a");
        let cs = case_flags("a");
        let (page, total) = idx.search_with_total(&q, &cs, 1);
        assert_eq!(page.len(), 1);
        assert!(total > page.len(), "total must count beyond the page");
        assert_eq!(total, idx.search(&q, &cs, 100).len());
    }

    #[test]
    fn rejects_stale_or_truncated_files() {
        let dir = std::env::temp_dir().join("prism-stale-test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("stale.prsm");
        std::fs::write(&p, b"PRSM\x01\x00\x00\x00 and garbage").unwrap();
        assert!(Index::open(&p).is_none(), "wrong version must not load");
        std::fs::write(&p, b"NOPE").unwrap();
        assert!(Index::open(&p).is_none(), "non-index must not load");
    }
}
