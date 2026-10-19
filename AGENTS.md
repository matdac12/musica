# Musica

Tray music player for Windows: YouTube search via `yt-dlp`, playback via a
windowless `mpv --idle` child over its JSON IPC named pipe. Part of mstack;
same shape as tasca (plain Win32 via `windows` 0.62, launcher in `launch.rs`).

## Layout

| File | Role |
| --- | --- |
| `launch.rs` | `toggle / quit / --hidden`: post to the running resident or become it; elevated starts relaunch through explorer.exe |
| `ui.rs` | The resident: one popup with two views (Search: EDIT + owner-drawn LBS_NODATA listbox; Player: fully painted, double-buffered), queue, tray icon, message loop |
| `search.rs` | `yt-dlp --flat-playlist --dump-single-json ytsearchN:` and parsing |
| `player.rs` | mpv child in a kill-on-close job object; one IO thread owns the pipe |
| `tools.rs` | Find `mpv.exe` / `yt-dlp.exe` (PATH, then winget Links and Packages) |

## Rules

- Memory is the product. Measure `PrivateMemorySize64` of musica and mpv before and after any change that touches playback or buffering.
- Keep the mpv flags that trim it (`--load-*=no`, `--osc=no`, small demuxer cache). Do not add the yt-dlp `player_client=android` extractor arg: it made mpv buffer ~46 MB up front.
- The pipe handle is synchronous, so a blocking read would stall writes: the IO thread polls with `PeekNamedPipe`. Don't replace that with a blocking reader on the same handle.
- UI state lives in a thread-local `RefCell`; `with()` returns `None` on re-entry. Release the borrow before calls that send messages to our own wndproc.
- Background threads talk to the UI only by `PostMessageW` with a boxed payload; the wndproc frees it.
- You cannot confirm the Caps+M hotkey fires or that audio is audible. Say so and ask the owner. You can confirm mpv's `time-pos` advances by connecting a second client to `\\.\pipe\musica-mpv-<pid>`.

## Testing next to the installed copy

Debug builds read two switches (release builds never do): `MUSICA_INSTANCE=<name>` suffixes the window class and mutex so a test resident runs beside the installed one, and `MUSICA_TEST_AO=null` makes mpv play silently. Don't stop the owner's music to test.

```powershell
$env:MUSICA_INSTANCE='test'; $env:MUSICA_TEST_AO='null'; .\target\debug\musica.exe
```

## Commands

```powershell
cargo test
cargo clippy --all-targets
.\scripts\install.ps1
```
