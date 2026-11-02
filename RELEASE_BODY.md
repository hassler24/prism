## Install — 30 seconds

**Download [`prism.exe`](https://github.com/hassler24/prism/releases/latest/download/prism.exe)** (820 KB, no installer), put it anywhere, double-click it.

- First launch indexes your drives (~2–3 min per million files).
- Every launch after that: **half a second** to a window with the caret ready.
- Type — results appear as you type. Space separates words. `Esc` closes.

### Make it instant to reach

Right-click `prism.exe` → **Pin to taskbar** (or **Pin to Start**). Then `Win`, type `pri`, `Enter` — under a second from any app.

### Verify the download

```powershell
# SHA256 of the attached zip
Get-FileHash .\prism-v0.2.0-x86_64-pc-windows-msvc.zip -Algorithm SHA256
```

## What's new in 0.2.0

- **Spaces work.** `prism agents` is two words; the space key was being trimmed out of the query.
- **Journal-scoped refresh** — the index stays current by reading the NTFS change journal, so a file created seconds ago is searchable ~1 s later; a launch where nothing changed writes nothing at all.
- **The NTFS change time is the freshness watermark** — `LastWriteTime` does not move on create, so the old fallback missed real changes.
- Fixed UB in the search filters (unaligned `u64` slices), a rename-swallowing USN reason constant, and CLI output through redirected pipes.

Full notes and measurements in the [README](https://github.com/hassler24/prism#readme).
