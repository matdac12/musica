# Musica

Background music from YouTube, without a browser tab. A tiny Windows tray app:
press Caps+M (via Hyperkey), type a search, press Enter, pick a result, and it
plays on the default sound device. No ads, no window to keep open.

Lean Rust on plain Win32 (a ~330 KB exe, about 2 MB of memory idle). Playback is
a windowless `mpv` child that resolves YouTube through `yt-dlp`; it uses about
40 MB while playing and exits with Musica. Part of [mstack](https://github.com/matdac12/mstack).

## Status

Prototype. Search, play, pause, stop. Live streams (lofi radios) work. No queue,
library or volume control yet.

## Usage

```
musica.exe [toggle | quit | --hidden]
```

One resident per user. `toggle` (Caps+M) shows or hides the popup, starting the
resident if none is running; `quit` ends it and its player; `--hidden` starts it
in the tray only.

| Key | In the popup |
| --- | --- |
| Enter | Search (or play, if you pasted a YouTube link) |
| ↓ / ↑ | Into the results / back to the search box |
| Enter, double-click | Play the selected result |
| Space | Pause / resume (in the results) |
| Esc, click away | Hide |

The tray icon: left-click toggles the popup; right-click has Pause, Stop and Quit.
The log is `%LOCALAPPDATA%\Musica\musica.log`.

## Build and install

Windows, Rust stable (MSVC toolchain), plus the two players it drives:

```powershell
winget install mpv-player.mpv-CI.MSVC yt-dlp.yt-dlp
.\scripts\install.ps1        # cargo build --release, copy to %LOCALAPPDATA%\Musica
```

Musica finds `mpv.exe` and `yt-dlp.exe` on PATH or in winget's folders.

Hyperkey binding (hand-written, in `hyper.ahk`):

```ahk
CapsLock & m:: {
    Run '"' EnvGet("LOCALAPPDATA") '\Musica\musica.exe" toggle'
    KeyWait "CapsLock", "T0.4"
}
```

```powershell
cargo test
cargo clippy --all-targets
```
