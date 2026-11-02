<div align="center">

# ◆ Prism

**Instant file search for Windows.** Press a key, type two letters, the file is
already on screen.

[![Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D6)](https://github.com/hassler24/prism/releases)
[![Rust](https://img.shields.io/badge/made_with-Rust-DEA584)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![Release](https://img.shields.io/github/v/release/hassler24/prism)](https://github.com/hassler24/prism/releases)
[![CI](https://github.com/hassler24/prism/actions/workflows/ci.yml/badge.svg)](https://github.com/hassler24/prism/actions/workflows/ci.yml)

One `.exe`, **820 KB**, two dependencies, no installer, no service, no tray icon,
**0 processes when closed**. It indexes every drive on the machine — all
2,000,000+ files — and answers in single-digit milliseconds from a memory-mapped
index. Nothing runs in the background, ever.

</div>

---

## Why

Windows Search is slow, skips files, and needs a service that never sleeps.
Everything was built the other way round:

| | Windows Search | Prism |
|---|---|---|
| Indexes | a filtered subset | **every file, every drive** |
| Memory when closed | a service, always | **0 processes** |
| A new file is findable in | minutes | **~1 second** |
| Cold first query | seconds | **104 ms** (then 31 ms) |
| Anything to install | a service + index | one `.exe` you can delete |

The whole engine is two crates (`memmap2`, `ab_glyph`) and ~6,500 lines of Rust
with hand-rolled Win32 — no GUI framework, no WebView, no GPU process.

## 60-second start

1. **Download** `prism.exe` from the
   [latest release](https://github.com/hassler24/prism/releases) and drop it
   anywhere. No installer, no dependencies.
2. **Double-click it.** The first launch indexes your drives (~2–3 min for a
   million files). Every launch after that starts in **half a second**.
3. **Type.** Results appear as you type — no Enter, no waiting.

That is the whole setup. Nothing else to configure.

### The shortcut

Pin `prism.exe` to the taskbar, or keep the shortcut on your desktop — **Win**,
first letters of its name, **Enter**. From a cold start that is under a second
to a window with the caret already in the field.

A global hotkey was deliberately **not** added. Windows will not give a
permanent key chord to a program that is allowed to be closed, and the only way
to get one is a low-level keyboard hook — a process that runs from boot and
sees every keystroke on the machine. That is the exact shape of a keylogger,
and it would break the one rule this app is built on: **nothing runs when Prism
is closed.** (If you want one anyway, AutoHotkey can wrap it in two lines.)

## Using it

| Key | Does |
|---|---|
| letters / **space** | search — space splits words, all of them must match |
| `↑` `↓` | move through results |
| `PgUp` `PgDn` | jump a page |
| `Enter` | open the file |
| `Ctrl+Enter` | reveal it in Explorer |
| `Ctrl+C` | copy the selected file's path |
| `Esc` | clear the query; again to close |
| click outside | close |

The query is **fuzzy**: `agnts` still finds `AGENTS.md`, `pr ag` finds
`prism-agent.ts`. Misspelt words degrade to partial matches instead of "no
results" — the failure mode that makes Windows Search useless.

Paste works too, including multi-line text (line breaks collapse to spaces).

## How fast, measured

On a live machine: **1,990,000+ files indexed, 135 MB index, Ryzen-class CPU,
NVMe.** Every number below is from that machine, not a benchmark rig.

| | |
|---|---|
| Search, warm | **8–23 ms** (`agents` 8.2, `prism` 19.7, `a` 22.7) |
| Search, cold process | 104 ms first, 31 ms after |
| Launch to usable window | **~0.5 s** |
| Full index build | **125–150 s** for 1.99 M entries |
| Refresh after real changes | **0.25–1.6 s** |
| Refresh when nothing changed | **nothing is written at all** |
| Memory while the window is open | 11 MB private / 26–77 MB working set |
| Memory when closed | **0** — the process is gone |

## How it stays current

Prism does not re-scan the disk to learn what changed. It asks NTFS, which
already knows:

- **While the window is open**, a thread tails the NTFS change journal (the
  unprivileged variant — no admin needed) and pushes brand-new files straight
  into the result list, so a file created *a second ago* is already searchable.
- **On every launch**, a freshness check reads that journal from where the last
  index left off and reduces it to the exact set of directories whose contents
  changed. Those get re-listed; the other 268,000 directories are copied out of
  the old index untouched. If the journal proves nothing changed, **nothing is
  written** — the 135 MB file is not even opened for writing.
- The watermark is the NTFS **change time** (`$STANDARD_INFORMATION.ChangeTime`
  via `FILE_BASIC_INFO`), which moves when and only when a directory's listing
  changes. `LastWriteTime` — what everything else uses — is a trap: it moves on
  in-place writes and does not move on creates. This was found by measuring, and
  it is the difference between a refresh that is right and one that is fast.
- If the journal is unavailable (a FAT32 stick) or has rolled over, it falls
  back to statting every directory in parallel, and only a broken index triggers
  a full rebuild.

## How it is small

- **820 KB** — the whole program. The icon and version info are compiled in.
- **Two dependencies**, both tiny and boring: `memmap2` (memory-mapped files)
  and `ab_glyph` (font rasterisation).
- **No GUI framework.** A layered Win32 window, a software compositor, and a
  DIB. The blurred glass backdrop is captured, downscaled and blurred in plain
  Rust — no DWM composition, no GPU, no WebView2 runtime.
- **0 processes when closed.** There is no service, no tray, no autostart, no
  updater. Delete the `.exe` and the machine is exactly as it was (the index is
  a plain file in `%LOCALAPPDATA%\Prism`, deleted with the folder).

## From source

```bat
git clone https://github.com/hassler24/prism
cd prism
cargo build --release
target\release\prism.exe
```

Needs the MSVC toolchain and Windows 10/11. Tests:

```bat
cargo test --workspace
```

### Command line

```text
prism                     open the search window
prism --search "query"    search from a terminal, top 20 hits
prism --stats             index size, age, and what the journal says changed
prism --refresh           incremental refresh, from the journal
prism --build-index       force a full rebuild
prism --bench "q" 20      search timing, min/median/avg/max
```

## Known limits

- **The index is per-user** — another Windows account sees nothing.
- **Removable drives** are indexed while plugged in and stay in the results
  until the next rebuild after they are unplugged.
- **A small change still rewrites the whole index file** (135 MB, ~0.25 s). It
  is fast, not incremental; an append-only delta file is the next step.
- **Long paths** over 260 characters are skipped unless the volume has long-path
  support enabled.
- **Windows only.** It is Win32 through and through; there is no macOS or Linux
  port and no plan for one.

## License

[MIT](LICENSE) © 2026 Hassler
