# Musica

Tray music player for Windows: YouTube search via `yt-dlp`, playback via a
windowless `mpv --idle` child over its JSON IPC named pipe. Part of mstack;
same shape as tasca (plain Win32 via `windows` 0.62, launcher in `launch.rs`).

## Layout

| File | Role |
| --- | --- |
| `launch.rs` | `toggle / quit / --hidden`: post to the running resident or become it; elevated starts relaunch through explorer.exe |
| `ui.rs` | The resident: one popup with two views (Search: EDIT + owner-drawn LBS_NODATA listbox showing the library or YouTube results; Player: fully painted, double-buffered), queue, tray icon, message loop |
| `library.rs` | The saved library: `library.json` index (atomic writes, `.bad` on corruption), `library\` audio folder, startup sweep, yt-dlp `--print` line to entry, the live `filter`, `shuffle_first` |
| `download.rs` | Save worker: one thread, FIFO of URLs, one yt-dlp at a time into `library\.partial\` (joins the job) |
| `job.rs` | The one process-wide kill-on-close job object; `job::join(child)` for every child that must not outlive musica (mpv, the download yt-dlp; search's short-lived yt-dlp does not) |
| `search.rs` | `yt-dlp --flat-playlist --dump-single-json ytsearchN:` and parsing |
| `player.rs` | mpv child (joins the shared job, it no longer owns one); one IO thread owns the pipe |
| `tools.rs` | Find `mpv.exe` / `yt-dlp.exe` (PATH, then winget Links and Packages) |

## Rules

- Memory is the product. Measure `PrivateMemorySize64` of musica and mpv before and after any change that touches playback or buffering.
- Keep the mpv flags that trim it (`--load-*=no`, `--osc=no`, small demuxer cache). Do not add the yt-dlp `player_client=android` extractor arg: it made mpv buffer ~46 MB up front.
- The pipe handle is synchronous, so a blocking read would stall writes: the IO thread polls with `PeekNamedPipe`. Don't replace that with a blocking reader on the same handle.
- UI state lives in a thread-local `RefCell`; `with()` returns `None` on re-entry. Release the borrow before calls that send messages to our own wndproc.
- Background threads talk to the UI only by `PostMessageW` with a boxed payload; the wndproc frees it.
- Saving runs `yt-dlp -q --no-warnings --no-playlist --playlist-items 1 -f bestaudio/best --no-simulate -o <library>\.partial\%(id)s.%(ext)s --print after_move:%(.{id,title,channel,uploader,duration,ext,filepath,webpage_url})j -- <url>`. No `-x`, no ffmpeg, no `player_client=android`. The worker only downloads; the UI thread moves the file, adds the entry and writes the index. A pasted YouTube link gets its id from the URL (`search::youtube_id`) so a saved track shows as saved at once; any other link gets yt-dlp's id from the print line. A duplicate id is dropped after download. `App::saving` holds url and id and is matched like `Track::is_same` (id when both have one), so a pasted link and a search result for one video never both download. `finalize` never overwrites a file already at the target: it renames it to `<id>.orphan[.N].<ext>` and logs it (spec decision 14).
- Saved tracks play from `library\<file>` (`library::local_file`), else the URL. While a local file plays, `Event::Title` is ignored (mpv reports the file name) and the length comes from the index. "Is this the playing row" is `Track::is_same` (id, else url). If mpv fails to open the saved file (deleted after the check), `Event::Error` replays the URL once (`start_current(true)`); `local` and `local_duration` are cleared on error and at the end of the queue.
- Library RAM measured (debug, `PrivateMemorySize64`): musica idle 1.97 MB on main, 1.98 MB with an empty library, 1.94 MB with 50 entries; the download's yt-dlp peaks near 52 MB (3 min track and 2 h mix alike) and is gone when it ends.
- The Search list has two sources (`Source::Library`, `Source::Results`). Library: `App::view_idx` (index positions, newest first, `library::filter` by the box text), rebuilt by `sync_list` on every `EN_CHANGE` and when the library changes. Results stay across edits; only Enter or an emptied box changes the source. A failed search falls back to Library with the failure status. Enter, double-click and Shift+Enter (`library::shuffle_first`, no `rand`) clone the shown list into `queue` (`play_shown`). The queue is a `Vec<Track>` clone, not index positions: removing a track (MUSICA-9) shifts the index under a queue that held positions, so a removal never touches a playing queue.
- Queue RAM measured (debug): musica playing a 50-track library queue 2.50–2.55 MB vs 2.55–2.56 MB for a one-track queue, mpv 42–43 MB either way. The clone is below what `PrivateMemorySize64` can resolve. Idle with 50 entries: 2.02 MB hidden / 2.48 MB with the popup shown, against 1.91–1.98 / 2.42 empty.
- Removing (MUSICA-9): Del on a Library row (`remove_selected`) drops the entry, writes the index, then `library::trash_file` moves the file to `library\.trash\` (rename works even while mpv plays it). `App::undo` is the one slot (entry, old index position, 5 s `until`); Ctrl+Z reinserts it only while `undo_pending()`, else the key falls through to the EDIT's own undo. The timer expires the slot (`expire_undo`) and the next removal does too. If the move fails because the file is in use, the file stays, a `.trash\<file>.del` marker is written, and `App::held` retries the delete on the 500 ms timer; `sweep` honours markers at startup (never deleting a file an entry names again). Del is ignored on Results rows and in the Player.
- Shift+Enter reads `GetAsyncKeyState`, so posted messages cannot set it. Test shuffle with a throwaway build that also accepts a flag in `lParam`; don't commit it.
- Ctrl+Z reads `GetAsyncKeyState` too: test the undo with a throwaway build that also accepts a flag in `lParam` (e.g. scan-code bits 0x5A; a fake `lParam` repeat count makes the listbox loop), and clear the box between steps, since a posted Z without Ctrl types a `z` into it.
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
