# Musica

Background music from YouTube, without a browser tab. A tiny Windows tray app:
press Caps+M (via Hyperkey), type a search, press Enter, pick a result, and it
plays on the default sound device. No ads, no window to keep open.

Lean Rust on plain Win32 (a ~330 KB exe, about 2 MB of memory idle). Playback is
a windowless `mpv` child that resolves YouTube through `yt-dlp`; it uses about
40 MB while playing and exits with Musica. Part of [mstack](https://github.com/matdac12/mstack).

## Status

Prototype. Search, play, a player view with the queue, pause, next/previous,
volume. Live streams (lofi radios) work. Save the playing track to a local
library (below); saved tracks play from disk, offline. The Search view lists
the library, so you can browse and filter it. Removing tracks and search
history come later.

## Usage

```
musica.exe [toggle | quit | --hidden]
```

One resident per user. `toggle` (Caps+M) shows or hides the popup, starting the
resident if none is running; `quit` ends it and its player; `--hidden` starts it
in the tray only.

Caps+M opens the **player** while something is playing, the **search** otherwise.
The results you play from become the queue: at the end of a track the next one
plays.

| Key | Player |
| --- | --- |
| Space | Pause / resume |
| → / ← | Next / previous (← restarts the track if you're past 3 s) |
| ↑ / ↓ | Volume (max 100%) |
| Ctrl+S, click the heart | Save the track (see Saved tracks) |
| Any letter | Start a new search |
| Tab | Search view |
| Esc, click away | Hide |

| Key | Search |
| --- | --- |
| Typing | Filters the saved library live, by title or channel (no network) |
| Enter (in the box) | Search YouTube (or play, if you pasted a YouTube link) |
| ↓ / ↑ | Into the list / back to the search box |
| Enter, double-click (in the list) | Play the list as shown, from the selected row, and open the player |
| Shift+Enter (library list) | Play the library list shuffled, the selected track first |
| Ctrl+↑ / Ctrl+↓ | Volume (Ctrl+Shift for ±10) |
| Space | Pause / resume (in the list) |
| Esc, Tab | Back to the player (Esc hides when nothing plays) |

## Saved tracks

On the player, click the heart next to the title or press **Ctrl+S** to save
the playing track: Musica downloads it with `yt-dlp` as the native best audio
(`.webm` or `.m4a`, no re-encode, no ffmpeg) into `%LOCALAPPDATA%\Musica\library\`
and lists it in `library.json` next to it. The heart is an outline when not
saved, dim while queued or downloading, orange once saved.

- A saved track plays from its file wherever you start it (search result,
  queue, pasted link), so it works with no network. It shows its saved title and
  length at once. If the file was deleted by hand, it streams from YouTube
  instead. Saved tracks show an orange heart in the results and in Up next; a
  track being downloaded shows a dim one.
- Only finite tracks can be saved. A live stream, or a pasted link before its
  length is known, says "Live streams can't be saved". A saved track says
  "Already saved". Save never toggles, and there is no unsave yet.
- Downloads run one at a time and stop when Musica quits. A failed download
  leaves nothing behind and shows "Couldn't save: ...".
- The library survives reinstalls. A damaged `library.json` is kept as
  `library.json.bad` and Musica starts with an empty library; the audio files stay.

### Browsing the library

With the search box empty, the Search view lists everything saved, newest
first; the status line says how many tracks and how much disk ("Library · 12
tracks · 1.4 GB").

- **Typing filters the library** as you type (title or channel, any case). The
  library stays until you press Enter: only then do YouTube results replace it.
  The results stay while you edit the box; emptying the box brings the library
  back.
- **Enter or double-click** on a library row plays the list as shown (all of it,
  or the filtered part) from that row; at the end playback stops, there is no
  loop. **Shift+Enter** plays it shuffled with the selected track first.
- A row whose file has gone says "File missing" in red, and still plays from its
  URL.
- **Offline:** if the YouTube search fails, the list shows the saved tracks that
  match the query, with "YouTube search failed · showing saved matches".

The tray icon: left-click toggles the popup; right-click has Pause, Next, Stop and Quit.
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
