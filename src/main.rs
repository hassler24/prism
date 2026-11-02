#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::time::Instant;

use prism::fuzzy::{case_flags, Query};
use prism::index::{self, Index};
use prism::refresh::{self, Outcome};
use prism::usn;
use prism::walk;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        prism::win32::ensure_cli_console();
    }

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Prism — instant file finder\n\n\
             prism                     open the search window\n\
             prism --build-index       rebuild the index for all fixed drives\n\
             prism --build-index DIR   rebuild the index for one directory\n\
             prism --refresh           update the index incrementally\n\
             prism --search QUERY      search the current index from the console\n\
             prism --stats             show index metadata"
        );
        return;
    }

    if let Some(pos) = args.iter().position(|a| a == "--bench") {
        let q = args.get(pos + 1).cloned().unwrap_or_else(|| "prism".into());
        let n = args.get(pos + 2).and_then(|s| s.parse().ok()).unwrap_or(20);
        bench_cli(&q, n);
        return;
    }

    if let Some(pos) = args.iter().position(|a| a == "--search" || a == "-s") {
        let q = args[pos + 1..].join(" ");
        if q.trim().is_empty() {
            eprintln!("missing query");
            std::process::exit(2);
        }
        search_cli(&q);
        return;
    }

    if args.iter().any(|a| a == "--stats" || a == "--info") {
        stats_cli();
        return;
    }

    if args.iter().any(|a| a == "--refresh") {
        refresh_cli();
        return;
    }

    if let Some(pos) = args
        .iter()
        .position(|a| a == "--build-index" || a == "--rebuild")
    {
        let root = args.get(pos + 1).filter(|a| !a.starts_with("--")).cloned();
        build_cli(root);
        return;
    }

    prism::ui::run();
}

fn build_cli(root: Option<String>) {
    let roots = match root {
        Some(r) => vec![walk::normalize_root(&r)],
        None => walk::default_roots(),
    };
    if roots.is_empty() {
        eprintln!("no fixed/removable drives found");
        std::process::exit(1);
    }
    let out = index::default_index_path();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    // Journal positions before the walk: whatever changes while we index is
    // replayed by the next refresh instead of being lost.
    let cursors = usn::snapshot(&roots);
    println!(
        "indexing {:?} with {threads} threads -> {}",
        roots,
        out.display()
    );
    let t = Instant::now();
    let stats = match index::build(&roots, &out, threads) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("index build failed: {e}");
            std::process::exit(1);
        }
    };
    usn::save_cursors(&cursors);
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!(
        "done: {} entries ({} files, {} dirs), {:.2}s walk, {:.1} MB index",
        stats.entries,
        stats.entries.saturating_sub(stats.dirs),
        stats.dirs,
        t.elapsed().as_secs_f64(),
        size as f64 / 1_048_576.0
    );
}

fn load_or_die() -> Index {
    let path = index::default_index_path();
    match Index::open(&path) {
        Some(idx) => idx,
        None => {
            eprintln!(
                "no usable index at {} — run `prism --build-index`",
                path.display()
            );
            std::process::exit(1);
        }
    }
}

fn refresh_cli() {
    let path = index::default_index_path();
    let roots = walk::default_roots();
    if roots.is_empty() {
        eprintln!("no fixed/removable drives found");
        std::process::exit(1);
    }
    match refresh::refresh_auto(&path, &path, &roots) {
        Ok(done) if !matches!(done.outcome, Outcome::NeedsFullRebuild(_)) => {
            // The index on disk is already the refreshed one (no scratch + swap
            // in the CLI path), so the journal position is safe to persist.
            done.commit_cursors();
            report(done.outcome);
        }
        Ok(done) => report(done.outcome),
        Err(e) => {
            eprintln!("refresh failed: {e}");
            std::process::exit(1);
        }
    }
}

fn report(outcome: Outcome) {
    match outcome {
        Outcome::Refreshed(s) => {
            println!(
                "refreshed: {} entries ({} files, {} dirs), {} directories checked, {:.2}s",
                s.entries,
                s.files,
                s.dirs,
                s.changed_dirs,
                s.ms as f64 / 1000.0
            );
        }
        Outcome::UpToDate(s) => {
            println!(
                "up to date: {} entries, nothing to rewrite ({:.3}s)",
                s.entries,
                s.ms as f64 / 1000.0
            );
        }
        Outcome::NeedsFullRebuild(reason) => {
            println!("incremental refresh not possible: {reason}; rebuilding");
            build_cli(None);
        }
    }
}

fn stats_cli() {
    let idx = load_or_die();
    let path = index::default_index_path();
    let built = idx.header.built_at;
    let age = walk::unix_now().saturating_sub(built);
    let dirs = idx
        .entries()
        .iter()
        .filter(|e| e.flags & index::ENTRY_DIR != 0)
        .count();
    println!("index:   {}", path.display());
    println!("entries: {}", idx.count());
    println!("files:   {}", idx.count().saturating_sub(dirs));
    println!("dirs:    {dirs}");
    println!("roots:   {}", idx.header.root_count);
    println!(
        "names:   {:.1} MB",
        idx.header.names_len as f64 / 1_048_576.0
    );
    println!("built:   {} ({}s ago)", built, age);
    println!("version: {}", idx.header.version);
    // How stale is it *right now*? One journal query per drive, no directory IO.
    let log = usn::scan(&walk::default_roots());
    if log.complete {
        println!(
            "pending: {} changed dirs ({} journal records)",
            log.dirs.len(),
            log.records
        );
    } else {
        println!("pending: unknown ({})", log.note);
    }
}

fn bench_cli(q: &str, n: usize) {
    let idx = load_or_die();
    let query = Query::parse(q);
    let flags = case_flags(q);
    let mut times = Vec::with_capacity(n);
    let mut hits = 0;
    for _ in 0..n {
        let t = Instant::now();
        hits += idx.search(&query, &flags, 20).len();
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "query={q:?} runs={n} hits/run={} min={:.2}ms median={:.2}ms avg={:.2}ms max={:.2}ms",
        hits / n,
        times[0],
        times[times.len() / 2],
        avg,
        times[times.len() - 1]
    );
}

fn search_cli(q: &str) {
    let idx = load_or_die();
    let query = Query::parse(q);
    let flags = case_flags(q);
    let t = Instant::now();
    let hits = idx.search(&query, &flags, 20);
    let elapsed = t.elapsed();
    println!(
        "{} hits in {:.2} ms\n",
        hits.len(),
        elapsed.as_secs_f64() * 1000.0
    );
    for (i, score, tier) in hits {
        let name = idx.name_str(i as usize);
        let parent = idx.parent_path(i as usize);
        let kind = if idx.is_dir(i as usize) {
            "dir "
        } else {
            "file"
        };
        println!("{kind} {score:>6} t{tier}  {name}\n          {parent}");
    }
}
