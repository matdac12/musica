# Build musica and install it to %LOCALAPPDATA%\Musica\musica.exe (the path
# Hyperkey's Caps+M runs). Quits a running resident first so the exe is free.
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$dest = Join-Path $env:LOCALAPPDATA 'Musica'
$exe = Join-Path $dest 'musica.exe'

cargo build --release --manifest-path (Join-Path $repo 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

if (Test-Path $exe) {
  & $exe quit
  Start-Sleep -Milliseconds 800
}
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item (Join-Path $repo 'target\release\musica.exe') $exe -Force
Write-Host "Installed $exe"

foreach ($tool in 'mpv.exe', 'yt-dlp.exe') {
  if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
    Write-Warning "$tool is not on PATH. musica also looks in winget's folders; install with: winget install mpv-player.mpv-CI.MSVC / yt-dlp.yt-dlp"
  }
}
