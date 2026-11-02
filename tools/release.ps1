# Build a release binary the way CI does and print where it landed.
# Run from the repo root:  powershell -File tools\release.ps1
$ErrorActionPreference = "Stop"
cargo build --release
$exe = Join-Path $PSScriptRoot "..\target\release\prism.exe"
if (-not (Test-Path $exe)) { throw "build did not produce $exe" }
Write-Output ""
Write-Output ("binary: {0}  ({1:N0} KB)" -f $exe, ((Get-Item $exe).Length/1KB))
Write-Output "attach it to a GitHub release with:  gh release create v0.2.0 `"$exe`" --title v0.2.0 --notes `"see README`""
