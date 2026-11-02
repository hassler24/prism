//! Incremental refresh — two engines, one builder.
//!
//! * [`refresh_scoped`] is the fast one: the NTFS change journal already knows
//!   *exactly* which directories were touched, so only those are re-listed. No
//!   stat pass, no filesystem walk — a handful of syscalls instead of one stat
//!   per directory. Everything else is copied straight out of the old index.
//! * [`refresh`] is the safety net: stat every directory and re-list the ones
//!   whose `mtime` moved. Needed on volumes with no journal (FAT32 sticks) and
//!   after a journal reset, and it costs one stat per directory.
//!
//! [`refresh_auto`] picks between them, and returns [`Outcome::UpToDate`] —
//! writing nothing at all — when the journal proves the index already matches
//! the disk. That is the common case, so a normal launch does no index work.
//!
//! Both engines share one rewrite: unchanged subtrees are copied out of the old
//! index, changed directories are re-enumerated, renamed/moved subtrees are
//! rebuilt, deleted entries simply never get emitted.
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use crate::fuzzy::{sigil, trigram_sigil};
use crate::index::{self, Entry, Index, ENTRY_DIR, ENTRY_HIDDEN, ENTRY_OFFLINE};
use crate::interop::*;
use crate::usn;
use crate::walk::{self, MAX_DEPTH};

#[derive(Debug, Default, Clone, Copy)]
pub struct RefreshStats {
    pub entries: usize,
    pub dirs: usize,
    pub files: usize,
    pub changed_dirs: usize,
    pub ms: u128,
}

#[derive(Debug)]
pub enum Outcome {
    /// The index already matched the disk; nothing was written.
    UpToDate(RefreshStats),
    Refreshed(RefreshStats),
    NeedsFullRebuild(&'static str),
}

/// Which directories to re-list and how to find them.
enum Mode<'a> {
    /// Canonical lowercase directory keys (see [`usn::dir_key`]) from the journal.
    Journal(&'a HashSet<String>),
    /// Stat every directory, then compare mtimes.
    Mtime,
}

/// A refresh result plus the journal position that goes with it.
///
/// The cursors are *not* persisted here: a caller that writes to a scratch file
/// and swaps it in later (the UI) must save them only after the swap, or a crash
/// in between would advance the journal past changes the live index lacks.
pub struct Refresh {
    pub outcome: Outcome,
    pub cursors: Vec<(String, usn::Cursor)>,
}

impl Refresh {
    fn new(outcome: Outcome, cursors: Vec<(String, usn::Cursor)>) -> Refresh {
        Refresh { outcome, cursors }
    }

    /// Persist journal positions, but only for an index that is really in place.
    pub fn commit_cursors(&self) {
        if !matches!(self.outcome, Outcome::NeedsFullRebuild(_)) {
            usn::save_cursors(&self.cursors);
        }
    }
}

/// Journal first, mtime fallback. This is what a launch should call.
pub fn refresh_auto(current: &Path, dest: &Path, roots: &[String]) -> Result<Refresh, String> {
    // Debug knob: prove the stat-driven fallback still works without waiting for
    // the journal to roll over.
    if std::env::var_os("PRISM_FORCE_MTIME").is_some() {
        return refresh(current, dest, roots);
    }
    let t_scan = Instant::now();
    let log = usn::scan(roots);
    trace(&format!(
        "scan {:?} complete={} records={} dirs={} {}",
        t_scan.elapsed(),
        log.complete,
        log.records,
        log.dirs.len(),
        log.note
    ));
    if std::env::var_os("PRISM_TRACE").is_some() {
        let mut dirs: Vec<&String> = log.dirs.iter().collect();
        dirs.sort();
        for d in dirs {
            eprintln!("trace:   dirty {d}");
        }
    }
    if log.complete {
        let Some(idx) = Index::open(current) else {
            return Ok(Refresh::new(
                Outcome::NeedsFullRebuild("no usable index"),
                Vec::new(),
            ));
        };
        let entries = idx.count();
        drop(idx);
        if log.dirs.is_empty() {
            // Nothing was created, deleted, renamed or moved: the index on disk
            // is already correct, so keep the file (and its 130 MB) untouched.
            return Ok(Refresh::new(
                Outcome::UpToDate(RefreshStats {
                    entries,
                    ..Default::default()
                }),
                log.cursors,
            ));
        }
        match rebuild(current, dest, roots, Mode::Journal(&log.dirs)) {
            // The journal handed us a clean answer covering everything up to the
            // position we read to, so hand that position back to the caller.
            Ok(out) if !matches!(out, Outcome::NeedsFullRebuild(_)) => {
                return Ok(Refresh::new(out, log.cursors))
            }
            // Structural surprise (drive set changed, corrupt index): the mtime
            // engine is still far cheaper than a 3–4 minute full rebuild.
            _ => {}
        }
    }
    refresh(current, dest, roots)
}

/// Stat-driven refresh. Keeps the old index usable if it fails.
pub fn refresh(current: &Path, dest: &Path, roots: &[String]) -> Result<Refresh, String> {
    // Capture journal positions *before* the pass: anything that happens while
    // we walk is replayed next time instead of being silently lost.
    let cursors = usn::snapshot(roots);
    let out = rebuild(current, dest, roots, Mode::Mtime)?;
    Ok(Refresh::new(out, cursors))
}

/// Refresh only the directories named by the change journal.
pub fn refresh_scoped(
    current: &Path,
    dest: &Path,
    roots: &[String],
    dirty: &HashSet<String>,
) -> Result<Outcome, String> {
    rebuild(current, dest, roots, Mode::Journal(dirty))
}

#[inline]
fn trace(msg: &str) {
    if std::env::var_os("PRISM_TRACE").is_some() {
        eprintln!("trace: {msg}");
    }
}

fn threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
}

fn rebuild(
    current: &Path,
    dest: &Path,
    roots: &[String],
    mode: Mode<'_>,
) -> Result<Outcome, String> {
    let t0 = Instant::now();
    let Some(idx) = Index::open(current) else {
        return Ok(Outcome::NeedsFullRebuild("no usable index"));
    };
    let entries = idx.entries();
    let count = entries.len();
    let root_count = idx.header.root_count as usize;
    if root_count == 0 || root_count > count || roots.len() != root_count {
        return Ok(Outcome::NeedsFullRebuild("drive set changed"));
    }

    let mut indexed_roots = Vec::with_capacity(root_count);
    for (i, e) in entries.iter().take(root_count).enumerate() {
        if e.flags & ENTRY_DIR == 0 || e.parent != i as u32 {
            return Ok(Outcome::NeedsFullRebuild("root entries are malformed"));
        }
        indexed_roots.push(idx.name_str(i));
    }
    let wanted: Vec<String> = roots.iter().map(|r| walk::normalize_root(r)).collect();
    if indexed_roots != wanted {
        return Ok(Outcome::NeedsFullRebuild("drive set changed"));
    }
    for (i, e) in entries.iter().enumerate() {
        let parent = e.parent as usize;
        if parent >= count || (parent == i && i >= root_count) {
            return Ok(Outcome::NeedsFullRebuild("parent index is corrupt"));
        }
    }

    let t_stat = Instant::now();
    let stats = match mode {
        Mode::Mtime => Some(stat_dirs(&idx, entries, threads())),
        Mode::Journal(_) => None,
    };
    trace(&format!("stat {:?}", t_stat.elapsed()));
    let (dirty, stats_ref) = match &mode {
        Mode::Journal(d) => (Some(*d), None),
        Mode::Mtime => (None, stats.as_deref()),
    };
    let built_at = idx.header.built_at;

    let t_walk = Instant::now();
    let mut b = Builder::new(&idx, dirty, stats_ref);
    let index_init = t_walk.elapsed();
    if let Some(reason) = b.run(&wanted) {
        return Ok(Outcome::NeedsFullRebuild(reason));
    }
    let visit = t_walk.elapsed();
    let built = b.finish();
    trace(&format!(
        "walk {:?} entries={} changed_dirs={}",
        t_walk.elapsed(),
        built.entries.len(),
        built.changed_dirs
    ));
    trace(&format!(
        "  init {:?} visit {:?} finish {:?}",
        index_init,
        visit - index_init,
        t_walk.elapsed() - visit
    ));
    // Nothing to do when the journal named no relistable directory, or when every
    // directory it named came back identical: either way the live index stands.
    let unchanged =
        (built.changed_dirs == 0 || built.noop) && built.entries.len() == count && built_at > 2;
    if std::env::var_os("PRISM_TRACE").is_some() {
        eprintln!(
            "trace: compare unchanged={unchanged} changed_dirs={} noop={} entries {}=={}",
            built.changed_dirs,
            built.noop,
            built.entries.len(),
            count
        );
    }

    // Drop both the output vecs' source mapping before replacing the file.
    drop(idx);
    let mut stats = RefreshStats {
        entries: built.entries.len(),
        dirs: built.dirs,
        files: built.files,
        changed_dirs: built.changed_dirs,
        ms: 0,
    };
    let t_write = Instant::now();
    if std::env::var_os("PRISM_TRACE").is_some() {
        eprintln!(
            "trace: build entries={} names={} sigs={} tris={}",
            built.entries.len(),
            built.names.len(),
            built.sigs.len(),
            built.tris.len()
        );
    }
    // The arrays are assembled in full before this point (the engine is a
    // rebuild, not an in-place patch), so a clean journal says nothing about
    // whether they differ from what is already on disk. Compare instead of
    // assuming: an unchanged index must not cost 133 MB of rewrite.
    let written = if unchanged {
        false
    } else {
        index::write_if_changed(
            &built.entries,
            &built.sigs,
            &built.tris,
            &built.names,
            dest,
            root_count,
        )
        .map_err(|e| format!("could not write index: {e}"))?
    };
    trace(&format!(
        "write {:?} written={written} unchanged={unchanged}",
        t_write.elapsed()
    ));
    stats.ms = t0.elapsed().as_millis();
    Ok(if unchanged {
        Outcome::UpToDate(stats)
    } else {
        Outcome::Refreshed(stats)
    })
}

struct Built {
    names: Vec<u8>,
    entries: Vec<Entry>,
    sigs: Vec<u64>,
    tris: Vec<[u64; 2]>,
    dirs: usize,
    files: usize,
    changed_dirs: usize,
    /// The rebuild reproduced the stored index exactly, so there is nothing to write.
    noop: bool,
}

struct Builder<'a> {
    idx: &'a Index,
    entries: &'a [Entry],
    /// `first[id]` = first child of entry `id`, `next[c]` = its next sibling.
    /// Replaces a `HashMap<u32, Vec<u32>>` child map: two flat arrays, no hashing,
    /// no 268k `Vec` allocations.
    first: Vec<u32>,
    next: Vec<u32>,
    out: Out,
    /// Mtime mode only.
    stats: Option<&'a [Stat]>,
    /// Journal mode only.
    dirty: Option<&'a HashSet<String>>,
    built_at: u32,
    changed_dirs: usize,
    key: String,
}

impl<'a> Builder<'a> {
    fn new(
        idx: &'a Index,
        dirty: Option<&'a HashSet<String>>,
        stats: Option<&'a [Stat]>,
    ) -> Builder<'a> {
        let entries = idx.entries();
        let n = entries.len();
        let mut first = vec![u32::MAX; n];
        let mut next = vec![u32::MAX; n];
        for (i, e) in entries.iter().enumerate() {
            let parent = e.parent as usize;
            if parent == i || parent >= n {
                continue;
            }
            next[i] = first[parent];
            first[parent] = i as u32;
        }
        Builder {
            idx,
            entries,
            first,
            next,
            out: Out::with_capacity(n, idx.names().len()),
            stats,
            dirty,
            built_at: idx.header.built_at,
            changed_dirs: 0,
            key: String::with_capacity(256),
        }
    }

    fn run(&mut self, roots: &[String]) -> Option<&'static str> {
        for (i, name) in roots.iter().enumerate() {
            let e = self.entries[i];
            let (change, flags) = match self.stats {
                Some(stats) => match stats[i] {
                    Stat::Dir { change, attrs, .. } => (change, entry_flags(attrs, true)),
                    Stat::Denied => (e.change, e.flags),
                    Stat::File(_) | Stat::Missing => {
                        return Some("indexed root is unavailable");
                    }
                },
                None => (e.change, e.flags),
            };
            let new_id = self
                .out
                .copy_stamped(self.idx, i as u32, 0, flags, e.mtime, change);
            self.out.entries[new_id as usize].parent = new_id;
            if let Some(reason) = self.visit(i as u32, new_id, name) {
                return Some(reason);
            }
        }
        None
    }

    fn finish(self) -> Built {
        Built {
            names: self.out.names,
            entries: self.out.entries,
            sigs: self.out.sigs,
            tris: self.out.tris,
            dirs: self.out.dirs,
            files: self.out.files,
            changed_dirs: self.changed_dirs,
            noop: self.out.noop,
        }
    }

    #[inline]
    fn kids(&self, id: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cur = self.first[id as usize];
        while cur != u32::MAX {
            out.push(cur);
            cur = self.next[cur as usize];
        }
        out
    }

    /// Does this directory need to be enumerated again?
    ///
    /// Only the NTFS **change** time answers this. It moves when and only when the
    /// directory's content changes — a child created, removed or renamed — and not
    /// when a file inside it is merely written. The other timestamp is a trap: Windows
    /// keeps it tick-for-tick in step with a Unix-style creation or deletion, so it
    /// flaps constantly inside anything that makes temp files, and each flap means a
    /// re-list of a directory that cannot have changed, plus a rewrite of the whole
    /// index to store that non-change. Entry 0 stamps are also never stable from one
    /// build to the next, so comparing them says "changed" most of the time.
    #[inline]
    fn relist(&mut self, old_id: u32, path: &str) -> bool {
        if let Some(dirty) = self.dirty {
            push_key(&mut self.key, path);
            return dirty.contains(self.key.as_str());
        }
        match self.stats.map(|s| s[old_id as usize]) {
            // No change time captured for this entry: we cannot vouch for it, so
            // re-list it. A wrong "unchanged" loses a file; a wrong "changed" only
            // costs a listing.
            Some(Stat::Dir { change, .. }) => {
                change == 0 || self.entries[old_id as usize].change != change
            }
            // A directory that became something else, or vanished, is changed.
            Some(Stat::Denied) => false,
            _ => true,
        }
    }

    /// Re-list `path` if it changed, otherwise copy its children verbatim.
    fn visit(&mut self, old_id: u32, new_id: u32, path: &str) -> Option<&'static str> {
        if self.relist(old_id, path) {
            self.changed_dirs += 1;
            if std::env::var_os("PRISM_TRACE").is_some() {
                eprintln!("trace: RELIST {path}");
            }
            self.relist_children(old_id, new_id, path)
        } else {
            self.copy_children(old_id, new_id, path)
        }
    }

    /// Copy a directory whose change time matches the index: its children are the
    /// stored ones, so each is re-emitted verbatim, with its own change time deciding
    /// only whether to descend into it.
    fn copy_children(&mut self, old_id: u32, new_id: u32, path: &str) -> Option<&'static str> {
        for child in self.kids(old_id) {
            let e = self.entries[child as usize];
            if e.flags & ENTRY_DIR == 0 {
                self.out.copy(self.idx, child, new_id, e.flags, e.mtime);
                continue;
            }
            // Directory: take fresh attrs/mtime when we have them, then decide
            // whether its own contents are still valid.
            let (mtime, change, flags, reparse) = match self.stats {
                Some(stats) => match stats[child as usize] {
                    Stat::Dir {
                        mtime,
                        change,
                        attrs,
                    } => (
                        mtime,
                        change,
                        entry_flags(attrs, true),
                        attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0,
                    ),
                    _ => (e.mtime, e.change, e.flags, false),
                },
                None => (e.mtime, e.change, e.flags, false),
            };
            let new_child = self
                .out
                .copy_stamped(self.idx, child, new_id, flags, mtime, change);
            if reparse {
                continue;
            }
            let child_path = join(path, &self.idx.name_str(child as usize));
            if let Some(reason) = self.visit(child, new_child, &child_path) {
                return Some(reason);
            }
        }
        None
    }

    /// Enumerate `path` again and reconcile it with the stored children.
    fn relist_children(&mut self, old_id: u32, new_id: u32, path: &str) -> Option<&'static str> {
        let depth = self.out.entries[new_id as usize].depth;
        let actual = match list_dir(path) {
            Listing::Entries(v) => v,
            // Still cannot read it: keep what we had rather than losing entries.
            Listing::Denied => {
                self.copy_children(old_id, new_id, path);
                return None;
            }
            // Deleted between the journal record and now: emit it empty. The
            // parent's own re-list (or the next journal record) removes it.
            Listing::Missing => return None,
        };

        // The stored listing, to tell a real change from a journal entry that only
        // reported a write into one of these children (editor swap files, log
        // appends, `touch`) and left the names exactly as they were.
        let kids = self.kids(old_id);
        // The stored names, for the change test below. Keyed by name because the walk
        // order and this listing order are both arbitrary.
        let before: HashSet<String> = kids
            .iter()
            .map(|&child| self.idx.name_str(child as usize).to_lowercase())
            .collect();
        let pushed_before = self.out.entries.len();
        let changed_before_count = self.changed_dirs;
        let mut old_children: HashMap<String, u32> = HashMap::with_capacity(kids.len());
        for &child in &kids {
            old_children.insert(self.idx.name_str(child as usize).to_lowercase(), child);
        }

        for item in actual {
            if item.is_dir
                && (depth + 1 > MAX_DEPTH
                    || walk::skip_dir(&item.lower)
                    || walk::is_cache_child(path, &item.name))
            {
                continue;
            }
            let child_path = join(path, &item.name);
            let Some(child) = old_children.remove(&item.lower) else {
                self.add_new(&item, &child_path, new_id, depth + 1);
                continue;
            };
            let e = self.entries[child as usize];
            if (e.flags & ENTRY_DIR != 0) != item.is_dir {
                // File became a directory or vice versa: rebuild it from scratch.
                self.add_new(&item, &child_path, new_id, depth + 1);
                continue;
            }
            let (sig, tri) = if self.idx.name(&e) == item.name.as_bytes() {
                (
                    self.idx.sigs()[child as usize],
                    self.idx.tris()[child as usize],
                )
            } else {
                sigils(&item.name)
            };
            let new_child = self.out.push(
                item.name.as_bytes(),
                item.flags,
                e.depth,
                new_id,
                item.mtime,
                e.change,
                sig,
                tri,
            );
            if !item.is_dir || item.reparse {
                continue;
            }
            if let Some(reason) = self.visit(child, new_child, &child_path) {
                return Some(reason);
            }
        }

        // Journal entries fire on writes too — an editor's swap file, a log append,
        // `touch` — so most re-lists reproduce what was already stored.
        // If no child was added or removed (the survivors plus the appended set account
        // for every stored child, and nothing is left over to delete) and no nested
        // directory changed, the index on disk still holds this directory, and a
        // rewrite would only re-emit it.
        //
        // Names are enough to be safe. Re-list order is whatever the directory handle
        // returns, never sorted, so a set rather than a sequence is the only valid
        // comparison; and a file that landed before the old walk may carry a stale name
        // from here, but it then wears a *different* one too, so the set differs and we
        // write. A stale pair wearing the same name can only be a replacement file, which
        // sets the watermark.
        let appended = self.out.entries.len() - pushed_before;
        if self.dirty.is_some()
            && old_children.is_empty()
            && appended == before.len()
            && self.changed_dirs == changed_before_count
        {
            let names = &self.out.names;
            let emitted: HashSet<String> = self.out.entries[pushed_before..]
                .iter()
                .map(|e| {
                    let lo = e.name_off as usize;
                    let hi = lo + e.name_len as usize;
                    String::from_utf8_lossy(&names[lo..hi]).to_lowercase()
                })
                .collect();
            if emitted == before {
                self.out.noop = true;
            }
        }
        None
    }

    fn add_new(&mut self, item: &Actual, path: &str, parent: u32, depth: u8) {
        let (sig, tri) = sigils(&item.name);
        let new_id = self.out.push(
            item.name.as_bytes(),
            item.flags,
            depth,
            parent,
            item.mtime,
            stamps(item, path).1,
            sig,
            tri,
        );
        if item.is_dir && !item.reparse {
            self.build_new(path, new_id, depth);
        }
    }

    /// Walk a subtree that is new to the index in full: it has no stored copy.
    fn build_new(&mut self, path: &str, parent: u32, depth: u8) {
        let Listing::Entries(items) = list_dir(path) else {
            return;
        };
        for item in items {
            if item.is_dir
                && (depth + 1 > MAX_DEPTH
                    || walk::skip_dir(&item.lower)
                    || walk::is_cache_child(path, &item.name))
            {
                continue;
            }
            let child_path = join(path, &item.name);
            let (sig, tri) = sigils(&item.name);
            let new_id = self.out.push(
                item.name.as_bytes(),
                item.flags,
                depth + 1,
                parent,
                item.mtime,
                stamps(&item, &child_path).1,
                sig,
                tri,
            );
            if item.is_dir && !item.reparse {
                self.build_new(&child_path, new_id, depth + 1);
            }
        }
    }

    fn copy_subtree(&mut self, old_id: u32, new_id: u32) {
        for child in self.kids(old_id) {
            let e = self.entries[child as usize];
            let new_child = self.out.copy(self.idx, child, new_id, e.flags, e.mtime);
            if e.flags & ENTRY_DIR != 0 {
                self.copy_subtree(child, new_child);
            }
        }
    }
}

struct Out {
    names: Vec<u8>,
    entries: Vec<Entry>,
    sigs: Vec<u64>,
    tris: Vec<[u64; 2]>,
    dirs: usize,
    files: usize,
    /// A journal-directed re-list came back identical to what was stored: the
    /// arrays are assembled, but nothing in the index on disk needs changing.
    noop: bool,
}

impl Out {
    fn with_capacity(entries: usize, names: usize) -> Out {
        Out {
            names: Vec::with_capacity(names),
            entries: Vec::with_capacity(entries),
            sigs: Vec::with_capacity(entries),
            tris: Vec::with_capacity(entries),
            dirs: 0,
            files: 0,
            noop: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        name: &[u8],
        flags: u8,
        depth: u8,
        parent: u32,
        mtime: i32,
        change: i32,
        sig: u64,
        tri: [u64; 2],
    ) -> u32 {
        let len = name.len().min(u16::MAX as usize);
        let id = self.entries.len() as u32;
        let off = self.names.len() as u32;
        self.names.extend_from_slice(&name[..len]);
        self.entries.push(Entry {
            name_off: off,
            name_len: len as u16,
            flags,
            depth,
            parent,
            mtime,
            change,
        });
        self.sigs.push(sig);
        self.tris.push(tri);
        if flags & ENTRY_DIR != 0 {
            self.dirs += 1;
        } else {
            self.files += 1;
        }
        id
    }

    /// Copy entry `old` out of the old index under a new parent.
    fn copy(&mut self, idx: &Index, old: u32, parent: u32, flags: u8, mtime: i32) -> u32 {
        let e = idx.entries()[old as usize];
        let o = old as usize;
        self.push(
            idx.name(&e),
            flags,
            e.depth,
            parent,
            mtime,
            e.change,
            idx.sigs()[o],
            idx.tris()[o],
        )
    }

    /// Copy with both watermarks refreshed, and report whatever was written back
    /// (the stamp may be lost if the volume refuses attribute writes).
    fn copy_stamped(
        &mut self,
        idx: &Index,
        old: u32,
        parent: u32,
        flags: u8,
        mtime: i32,
        change: i32,
    ) -> u32 {
        let e = idx.entries()[old as usize];
        let o = old as usize;
        self.push(
            idx.name(&e),
            flags,
            e.depth,
            parent,
            mtime,
            change,
            idx.sigs()[o],
            idx.tris()[o],
        )
    }
}

/// [`usn::dir_key`] into a reusable buffer, so the scan loop allocates nothing.
#[inline]
fn push_key(dst: &mut String, path: &str) {
    dst.clear();
    dst.push_str(path.trim_end_matches('\\'));
    dst.make_ascii_lowercase();
}

#[inline]
fn dir_changed(stored: i32, actual: i32, built_at: u32) -> bool {
    stored == 0 || stored != actual || (built_at > 2 && actual as u32 >= built_at.saturating_sub(2))
}

#[inline]
fn sigils(name: &str) -> (u64, [u64; 2]) {
    let (bloom, mask) = sigil(name.as_bytes());
    (index::pack_sig(bloom, mask), trigram_sigil(name.as_bytes()))
}

#[inline]
fn join(parent: &str, name: &str) -> String {
    let mut s = String::with_capacity(parent.len() + name.len() + 1);
    s.push_str(parent);
    if !parent.ends_with('\\') {
        s.push('\\');
    }
    s.push_str(name);
    s
}

#[inline]
fn entry_flags(attrs: u32, is_dir: bool) -> u8 {
    let mut flags = 0u8;
    if is_dir {
        flags |= ENTRY_DIR;
    }
    if attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
        flags |= ENTRY_HIDDEN;
    }
    if attrs & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_COMPRESSED) != 0 {
        flags |= ENTRY_OFFLINE;
    }
    flags
}

#[derive(Clone, Copy)]
enum Stat {
    Dir { mtime: i32, change: i32, attrs: u32 },
    File(i32),
    Missing,
    Denied,
}

/// One stat per directory, in parallel. Paths are built in a single forward pass
/// (a parent's index is always lower than its children's), which is what makes
/// this pass worth running at all.
fn stat_dirs(idx: &Index, entries: &[Entry], threads: usize) -> Vec<Stat> {
    let mut stats = vec![Stat::Missing; entries.len()];
    let mut slot: Vec<u32> = vec![u32::MAX; entries.len()];
    let mut paths: Vec<(u32, String)> = Vec::with_capacity(events_hint(entries.len()));
    let mut scratch = String::with_capacity(300);
    for (i, e) in entries.iter().enumerate() {
        if e.flags & ENTRY_DIR == 0 {
            continue;
        }
        let parent = e.parent as usize;
        scratch.clear();
        if parent != i {
            scratch.push_str(&paths[slot[parent] as usize].1);
            if !scratch.ends_with('\\') {
                scratch.push('\\');
            }
        }
        scratch.push_str(&String::from_utf8_lossy(idx.name(e)));
        slot[i] = paths.len() as u32;
        paths.push((i as u32, scratch.clone()));
    }
    if paths.is_empty() {
        return stats;
    }

    let workers = threads.max(1).min(paths.len());
    let chunk = paths.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for part in paths.chunks(chunk) {
            handles.push(scope.spawn(move || {
                let mut buf: Vec<u16> = Vec::with_capacity(320);
                part.iter()
                    .map(|(id, path)| (*id, stat_path(path, &mut buf)))
                    .collect::<Vec<_>>()
            }));
        }
        for handle in handles {
            for (id, stat) in handle.join().unwrap_or_default() {
                stats[id as usize] = stat;
            }
        }
    });
    stats
}

/// Rough number of directories to expect, for one allocation.
fn events_hint(entries: usize) -> usize {
    entries / 7 + 16
}

fn stat_path(path: &str, buf: &mut Vec<u16>) -> Stat {
    buf.clear();
    buf.extend(path.encode_utf16());
    buf.push(0);
    let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
    let ok =
        unsafe { GetFileAttributesExW(buf.as_ptr(), GET_FILEEX_INFO_LEVEL_STANDARD, &mut data) };
    if ok == 0 {
        return match unsafe { GetLastError() } {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Stat::Missing,
            _ => Stat::Denied,
        };
    }
    if data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Stat::Dir {
            mtime: 0,
            change: change_time(path),
            attrs: data.dwFileAttributes,
        }
    } else {
        Stat::File(0)
    }
}

/// Change time as the index stores it: unix seconds, 0 when the OS will not say.
///
/// `change == 0` is read by [`Builder::relist`] as "unknown, re-list it", so a platform
/// or a volume that cannot report the field costs correctness nothing and speed
/// everything.
#[inline]
fn change_time(path: &str) -> i32 {
    crate::interop::change_time_secs(path)
        .and_then(|s| i32::try_from(s).ok())
        .unwrap_or(0)
}

enum Listing {
    Entries(Vec<Actual>),
    Missing,
    Denied,
}

struct Actual {
    name: String,
    lower: String,
    flags: u8,
    is_dir: bool,
    reparse: bool,
    mtime: i32,
    /// NTFS change time, for directories only (files keep 0).
    change: i32,
}

/// Both watermarks of one listing entry, as stored in the index.
#[inline]
fn stamps(item: &Actual, path: &str) -> (i32, i32) {
    if item.is_dir {
        (item.mtime, change_time(path))
    } else {
        (item.mtime, 0)
    }
}

fn list_dir(path: &str) -> Listing {
    let mut pattern = String::with_capacity(path.len() + 2);
    pattern.push_str(path);
    if !path.ends_with('\\') {
        pattern.push('\\');
    }
    pattern.push('*');

    let mut ffd: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let h = unsafe {
        FindFirstFileExW(
            utf16z(&pattern).as_ptr(),
            FINDEX_INFO_BASIC,
            &mut ffd,
            FINDEX_SEARCH_OPS_NAME_MATCH,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH | FIND_FIRST_EX_ON_DISK_ENTRIES_ONLY,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return match unsafe { GetLastError() } {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Listing::Missing,
            _ => Listing::Denied,
        };
    }

    let mut items = Vec::new();
    loop {
        let name = from_wide(&ffd.cFileName);
        if name != "." && name != ".." {
            let is_dir = ffd.is_dir();
            items.push(Actual {
                lower: name.to_lowercase(),
                name,
                flags: entry_flags(ffd.dwFileAttributes, is_dir),
                is_dir,
                reparse: ffd.is_reparse(),
                mtime: ffd.mtime() as i32,
                change: 0,
            });
        }
        if unsafe { FindNextFileW(h, &mut ffd) } == 0 {
            break;
        }
    }
    unsafe { FindClose(h) };
    Listing::Entries(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuzzy::{case_flags, Query};
    use std::fs::{self, OpenOptions};
    use std::io::{Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn fixture(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "prism-refresh-{}-{}-{}",
            std::process::id(),
            tag,
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("tree");
        fs::create_dir_all(&root).unwrap();
        (base, root)
    }

    fn root_string(root: &Path) -> String {
        walk::normalize_root(&root.to_string_lossy())
    }

    fn build(base: &Path, root: &Path) -> (String, PathBuf) {
        let root = root_string(root);
        let idx = base.join("index.prsm");
        index::build(std::slice::from_ref(&root), &idx, 4).unwrap();
        (root, idx)
    }

    fn zero_built_at(path: &Path) {
        let mut f = OpenOptions::new().write(true).open(path).unwrap();
        f.seek(SeekFrom::Start(16)).unwrap();
        f.write_all(&0u32.to_le_bytes()).unwrap();
        f.flush().unwrap();
    }

    fn run(current: &Path, out: &Path, root: &str) -> RefreshStats {
        match refresh(current, out, &[root.to_string()]).unwrap().outcome {
            Outcome::Refreshed(s) | Outcome::UpToDate(s) => s,
            Outcome::NeedsFullRebuild(reason) => panic!("unexpected full rebuild: {reason}"),
        }
    }

    fn keys(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| usn::dir_key(p)).collect()
    }

    fn scope(current: &Path, out: &Path, root: &str, dirty: &HashSet<String>) -> RefreshStats {
        match refresh_scoped(current, out, &[root.to_string()], dirty).unwrap() {
            Outcome::Refreshed(s) | Outcome::UpToDate(s) => s,
            Outcome::NeedsFullRebuild(reason) => panic!("unexpected full rebuild: {reason}"),
        }
    }

    fn paths(path: &Path, q: &str) -> Vec<String> {
        let idx = Index::open(path).expect("open index");
        let query = Query::parse(q);
        let flags = case_flags(q);
        idx.search(&query, &flags, 20)
            .into_iter()
            .map(|(i, _, _)| idx.full_path(i as usize))
            .collect()
    }

    fn has(path: &Path, q: &str, suffix: &str) -> bool {
        paths(path, q).iter().any(|p| p.ends_with(suffix))
    }

    fn tick() {
        std::thread::sleep(Duration::from_millis(1100));
    }

    #[test]
    fn unchanged_tree_copies_without_relisting() {
        let (base, root) = fixture("same");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"a").unwrap();
        fs::write(root.join("sub").join("b.txt"), b"b").unwrap();
        let (root_s, idx) = build(&base, &root);
        zero_built_at(&idx);

        let out = base.join("out.prsm");
        let stats = run(&idx, &out, &root_s);
        assert_eq!(stats.changed_dirs, 0, "nothing changed");
        assert_eq!(stats.entries, Index::open(&idx).unwrap().count());
        assert!(has(&out, "b", "sub\\b.txt"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn adds_and_removes_files_and_subtrees() {
        let (base, root) = fixture("changes");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("keep.txt"), b"k").unwrap();
        fs::write(root.join("sub").join("old.txt"), b"o").unwrap();
        let (root_s, idx) = build(&base, &root);
        tick();

        fs::write(root.join("new.txt"), b"n").unwrap();
        fs::remove_file(root.join("sub").join("old.txt")).unwrap();
        fs::create_dir_all(root.join("sub").join("deep")).unwrap();
        fs::write(root.join("sub").join("deep").join("leaf.md"), b"l").unwrap();

        let out = base.join("out.prsm");
        let stats = run(&idx, &out, &root_s);
        assert!(stats.changed_dirs >= 2, "root and sub changed");
        assert!(has(&out, "new", "new.txt"));
        assert!(has(&out, "leaf", "sub\\deep\\leaf.md"));
        assert!(!has(&out, "old", "old.txt"), "deleted file must disappear");

        let leaves: Vec<String> = paths(&out, "leaf")
            .into_iter()
            .filter(|p| p.ends_with("leaf.md"))
            .collect();
        assert_eq!(leaves.len(), 1);
        assert!(leaves[0].ends_with("sub\\deep\\leaf.md"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn removes_a_directory_subtree() {
        let (base, root) = fixture("rmdir");
        fs::create_dir_all(root.join("gone").join("inside")).unwrap();
        fs::write(root.join("gone").join("inside").join("x.bin"), b"x").unwrap();
        let (root_s, idx) = build(&base, &root);
        tick();

        fs::remove_dir_all(root.join("gone")).unwrap();
        let out = base.join("out.prsm");
        run(&idx, &out, &root_s);
        assert!(paths(&out, "x.bin").is_empty());
        assert!(paths(&out, "gone").is_empty());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rename_moves_paths() {
        let (base, root) = fixture("rename");
        fs::create_dir_all(root.join("old")).unwrap();
        fs::write(root.join("old").join("file.txt"), b"f").unwrap();
        let (root_s, idx) = build(&base, &root);
        tick();

        fs::rename(root.join("old"), root.join("new")).unwrap();
        let out = base.join("out.prsm");
        run(&idx, &out, &root_s);
        assert!(has(&out, "file.txt", "new\\file.txt"));
        assert!(!has(&out, "file.txt", "old\\file.txt"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn type_change_file_to_directory() {
        let (base, root) = fixture("type");
        fs::write(root.join("node"), b"was a file").unwrap();
        let (root_s, idx) = build(&base, &root);
        tick();

        fs::remove_file(root.join("node")).unwrap();
        fs::create_dir_all(root.join("node")).unwrap();
        fs::write(root.join("node").join("child.md"), b"c").unwrap();

        let out = base.join("out.prsm");
        run(&idx, &out, &root_s);
        assert!(has(&out, "child", "node\\child.md"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn root_set_change_requests_full_rebuild() {
        let (base, root) = fixture("roots");
        fs::write(root.join("a.txt"), b"a").unwrap();
        let (_root_s, idx) = build(&base, &root);
        let out = base.join("out.prsm");
        let outcome = refresh(&idx, &out, &["D:\\".to_string()]).unwrap().outcome;
        assert!(matches!(outcome, Outcome::NeedsFullRebuild(_)));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_index_requests_full_rebuild() {
        let (base, root) = fixture("missing");
        let (root_s, _idx) = build(&base, &root);
        let out = base.join("out.prsm");
        let outcome = refresh(&base.join("nope.prsm"), &out, &[root_s])
            .unwrap()
            .outcome;
        assert!(matches!(outcome, Outcome::NeedsFullRebuild(_)));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scoped_refresh_only_touches_the_named_directories() {
        let (base, root) = fixture("scope");
        fs::create_dir_all(root.join("sub").join("deep")).unwrap();
        fs::create_dir_all(root.join("other")).unwrap();
        fs::write(root.join("sub").join("deep").join("keep.md"), b"k").unwrap();
        fs::write(root.join("other").join("stay.txt"), b"s").unwrap();
        let (root_s, idx) = build(&base, &root);
        tick();

        // New note in sub\deep; the index never learns about it any other way.
        fs::write(root.join("sub").join("deep").join("new-note.md"), b"n").unwrap();
        fs::write(root.join("other").join("stay.txt"), b"rewritten").unwrap();

        let out = base.join("out.prsm");
        let dirty = keys(&[&format!(r"{root_s}\sub\deep")]);
        let stats = scope(&idx, &out, &root_s, &dirty);

        assert_eq!(stats.changed_dirs, 1, "only the named directory re-lists");
        assert!(has(&out, "new-note", "sub\\deep\\new-note.md"));
        assert!(has(&out, "keep", "sub\\deep\\keep.md"));
        assert!(has(&out, "stay", "other\\stay.txt"));
        let before = Index::open(&idx).unwrap().count();
        assert_eq!(stats.entries, before + 1, "exactly one new entry");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scoped_refresh_removes_a_deleted_file() {
        let (base, root) = fixture("scope-del");
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs").join("gone.md"), b"g").unwrap();
        fs::write(root.join("docs").join("kept.md"), b"k").unwrap();
        let (root_s, idx) = build(&base, &root);
        tick();

        fs::remove_file(root.join("docs").join("gone.md")).unwrap();
        let out = base.join("out.prsm");
        let dirty = keys(&[&format!(r"{root_s}\docs")]);
        scope(&idx, &out, &root_s, &dirty);

        assert!(!has(&out, "gone", "gone.md"), "deleted file must disappear");
        assert!(has(&out, "kept", "kept.md"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scoped_refresh_finds_a_new_subtree_under_a_clean_parent() {
        let (base, root) = fixture("scope-new");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a").join("old.txt"), b"o").unwrap();
        let (root_s, idx) = build(&base, &root);
        tick();

        // A whole subtree appears at once (e.g. `git clone`): the journal names
        // only the last directories to change, but the parent is named too.
        fs::create_dir_all(root.join("a").join("fresh").join("inner")).unwrap();
        fs::write(
            root.join("a").join("fresh").join("inner").join("z.md"),
            b"z",
        )
        .unwrap();

        let out = base.join("out.prsm");
        let dirty = keys(&[&format!(r"{root_s}\a")]);
        scope(&idx, &out, &root_s, &dirty);

        assert!(has(&out, "z.md", "a\\fresh\\inner\\z.md"));
        let idx2 = Index::open(&out).unwrap();
        let z = idx2
            .search(&Query::parse("z.md"), &case_flags("z.md"), 5)
            .into_iter()
            .map(|(i, _, _)| idx2.full_path(i as usize))
            .collect::<Vec<_>>();
        assert_eq!(z.len(), 1, "new file indexed exactly once: {z:?}");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scoped_refresh_leaves_unknown_keys_alone() {
        let (base, root) = fixture("scope-none");
        fs::write(root.join("a.txt"), b"a").unwrap();
        let (root_s, idx) = build(&base, &root);
        tick();

        let out = base.join("out.prsm");
        let dirty = keys(&["C:\\definitely\\not\\here"]);
        // Nothing to do: report up-to-date and write nothing at all.
        match refresh_scoped(&idx, &out, std::slice::from_ref(&root_s), &dirty).unwrap() {
            Outcome::UpToDate(s) => assert_eq!(s.changed_dirs, 0),
            Outcome::Refreshed(s) => panic!("should not rewrite: {} dirs", s.changed_dirs),
            Outcome::NeedsFullRebuild(r) => panic!("unexpected full rebuild: {r}"),
        }
        assert!(!out.exists(), "a no-op refresh must not write an index");
        assert!(has(&idx, "a.txt", "a.txt"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn dir_keys_are_canonical() {
        assert_eq!(usn::dir_key("C:\\"), "c:");
        assert_eq!(usn::dir_key("C:\\Users\\Hassler"), "c:\\users\\hassler");
        assert_eq!(usn::dir_key("C:\\Users\\Hassler\\"), "c:\\users\\hassler");
    }
}
