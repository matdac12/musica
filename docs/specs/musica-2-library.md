# MUSICA-2 · Saved library

Save the track you are listening to, keep it on disk, play it with no network.
Designed with Mattia on 2026-10-20 (grill session); mattiamp's `f` was the
reference, not a compatibility target. Start fresh: nothing is imported.

Memory is the product (AGENTS.md). Every section states its RAM cost; the
implementation must measure them (see [Measuring](#measuring)).

## Decisions

| # | Decision |
| --- | --- |
| 1 | **Saved = downloaded.** One concept: a track in the library is a file on disk. No "favourite but streamed" state. |
| 2 | **Only finite tracks can be saved.** Live streams are refused ("Live streams can't be saved"). A pasted link can be saved once mpv reports a duration; before that, or if it is live, the same refusal. Playlist links save the one video (`--no-playlist`). No length cap. |
| 3 | **Save from the Player only:** a clickable heart next to the title, or **Ctrl+S**. Save-only, not a toggle: on a saved track it says "Already saved". |
| 4 | **Format: native best audio, no re-encode** (`-f bestaudio/best`, no `-x`, no ffmpeg). Opus `.webm` or AAC `.m4a`; a 2 h mix is ~115 MB (mattiamp's mp3 of the same mix: 202 MB). |
| 5 | **Location: `%LOCALAPPDATA%\Musica\`** (`log::dir()`): audio in `library\`, index in `library.json`. Debug builds with `MUSICA_INSTANCE=<name>` use `%LOCALAPPDATA%\Musica\<name>\` instead. |
| 6 | **Index loaded at startup and kept in memory.** The index is the source of truth, not a directory scan. |
| 7 | **No import from mattiamp.** |
| 8 | **Downloads run one at a time in a FIFO**, through yt-dlp started by musica, in the kill-on-close job. `.partial\` then move. No timeout. |
| 9 | **The library lives in the Search view's list.** It shows while the box is empty *and while you type*; only Enter (a YouTube search) replaces it. |
| 10 | **Typing filters the library live** (title or channel, case-insensitive substring). YouTube results stay until the next Enter or until the box is empty again. |
| 11 | **A saved track always plays from disk**, wherever it was started (library, results, queue). Missing file: fall back to the URL. |
| 12 | **Hearts** on the Player title, on YouTube result rows and on Up next rows. Library rows have no heart; they are marked only when the file is missing (a track still downloading is not in the library yet). |
| 13 | **Remove with Del** in the library list, no dialog; 5 s **Ctrl+Z** undo through `.trash\`. |
| 14 | **No cap, no automatic deletion.** Total size in the status line. Startup sweep of `.partial\` and `.trash\`. Orphans are logged, never deleted. A bad index is set aside, never overwritten. |
| 15 | **Offline: no network detection.** A failed search keeps showing the saved matches. Streams that fail to play stop as today. |
| 16 | **Enter plays the shown library list in order; Shift+Enter plays it shuffled**, selected track first. No loop. |
| 17 | **No tray-menu Save.** |
| — | MUSICA-3 (search history) is out of scope and may not happen; this spec reserves nothing for it. |

## Storage

```
%LOCALAPPDATA%\Musica\            (or ...\Musica\<MUSICA_INSTANCE>\ in debug builds)
  library.json                    the index
  library\<id>.webm | <id>.m4a    one file per saved track
  library\.partial\               downloads in flight
  library\.trash\                 removed files waiting for undo / mpv to let go
```

`musica.exe` and `musica.log` stay where they are; the test sub-folder only
holds library data. `install.ps1` never clears the folder, so the library
survives reinstalls.

`library.json` is a JSON array in save order (the UI shows it newest first):

```json
[
  {
    "id": "wdDhTr_M7kA",
    "title": "the playlist to actually enter flowstate.",
    "url": "https://www.youtube.com/watch?v=wdDhTr_M7kA",
    "channel": "Pure Grit Studio",
    "duration": 7381,
    "file": "wdDhTr_M7kA.webm",
    "bytes": 120795136,
    "saved_at": "2026-10-20T11:02:13+02:00"
  }
]
```

- `id` is yt-dlp's video id: the dedupe key and the `is_saved` lookup.
- `file` is relative to `library\`. `bytes` feeds the total size without
  stat-ing every file at startup.
- Writes are atomic: write `library.json.tmp`, then rename over `library.json`.
  The index is written only on save and on remove.

**RAM:** the whole index lives in `App`. Roughly 250 B per entry (a few
`String`s): 200 tracks ≈ 50 KB, 1,000 ≈ 250 KB, against ~2 MB idle today.

## Saving

### Who can be saved

`savable(track)` is true when the track has a known, finite duration:

- search result: `duration.is_some()` (no duration means live);
- pasted link (`from_search == false`): `mpv_duration` is `Some` (it arrives
  after load).

Otherwise the status line says "Live streams can't be saved". A saved track
says "Already saved". A track already queued or downloading is ignored.

### Trigger

- **Player view:** the heart (hit rect in `PlayerHits`, handled in
  `on_player_click`) or **Ctrl+S** (in `pretranslate`, `VK_S` with Ctrl
  down). Ctrl+S also produces `WM_CHAR 0x13`, below the `0x21` type-to-search
  cutoff, so it does not start a search.
- Nowhere else: not in the results list, not in the tray menu.

The heart (Segoe MDL2 Assets, the `icons` font already loaded) has three states:

| State | Glyph | Colour |
| --- | --- | --- |
| Not saved | `E006` outline heart | `DIM` |
| Queued or downloading | `E00B` filled heart | `DIM` |
| Saved | `E00B` filled heart | `ACCENT` |

The heart is drawn only when there is a current track. For a live stream it
stays an outline, and clicking it gives the refusal message.

### Download

A single download worker thread consumes a FIFO of jobs (`mpsc` channel).
It reports back to the UI only with `PostMessageW` and a boxed payload that
the wndproc frees (AGENTS.md rule), through a new `WM_LIBRARY` message.

Per job:

```
yt-dlp -q --no-warnings --no-playlist -f bestaudio/best --no-simulate
       -o "<library>\.partial\%(id)s.%(ext)s"
       --print "after_move:%(.{id,title,channel,uploader,duration,ext,filepath})j"
       <url>
```

- The `--print` line is yt-dlp's own metadata. It is the source for the index
  entry (`channel` falls back to `uploader`, as in `search::parse`). This gives
  pasted links a real title, channel and id.
- **Not** `--extractor-args youtube:player_client=android`: AGENTS.md bans it,
  and mattiamp's use of it is not carried over.
- No `-x`, no `--audio-format`, no ffmpeg.

On success: move `filepath` to `library\<id>.<ext>`, read its size, append the
entry, write the index atomically, post `Saved`. The entry exists only once the
file is complete.

On failure (non-zero exit, no print line, move failed): delete what this job
left in `.partial\` and post `Failed(last stderr line)`. The status line shows
"Couldn't save: …", the heart goes back to an outline, and the log gets the
full error. Pressing save again retries.

Process lifetime:

- yt-dlp joins the **same kill-on-close job object as mpv**, so it never
  outlives musica. Today the job is private to `Player` (`player.rs`,
  `make_job`). Move it to a process-wide one (for example `tools::job()`) that
  both use.
- Spawn with `tools::command` (no console window).
- No timeout: a 2 h mix on a slow line may take longer than mattiamp's 5 minutes.

**RAM:**

- musica holds the job FIFO, a handful of `Track`s.
- yt-dlp runs as a separate process only while a download runs: a PyInstaller
  Python, possibly plus `deno` for YouTube's JS challenge. Estimated
  30–60 MB private, not yet measured. It is gone when the download ends.
- The FIFO keeps it to one yt-dlp at a time.

## Playback from disk

`Track` gains `id: Option<String>`. `search::parse` already reads `e["id"]`;
keep it. A pasted link has `None` until it is saved, and the `Saved` handler
fills in the current track's id.

In `play_current`, when the track's id is in the index and
`library\<file>` exists, pass that path to `player.play` instead of the URL.
Otherwise use the URL; if the id is in the index but the file is gone, the
library row shows "File missing".

Local-file gotchas:

- Without embedded tags, mpv's `media-title` for a local file is the file name
  (`wdDhTr_M7kA.webm`). While playing a local file, ignore `Event::Title` and
  keep the index title.
- Duration comes from the index (`from_search`-style authoritative duration),
  so the progress bar works immediately.
- "Is this row the playing track?" compares `id` (falling back to `url`), not
  `url` alone, because the library and the results both point at the same
  video.

**RAM:** cheaper than streaming. There is no `ytdl_hook` → yt-dlp subprocess
inside mpv, and no network cache filling. The demuxer flags stay as they are.
`Track` grows by one `Option<String>` (11 B of payload).

## The library in the Search view

The listbox (`LBS_NODATA`, owner-drawn) shows one of two sources:

- **Library:** the index, newest first, filtered by the box text.
- **Results:** the last YouTube search.

| Box state | List shows |
| --- | --- |
| Empty | Whole library |
| Text typed, no Enter yet | Library filtered by the text |
| Enter pressed (search runs, then results arrive) | YouTube results |
| Text edited after results | Still the YouTube results, until the next Enter |
| Emptied again | Whole library |
| YouTube search failed | Library filtered by the query (see [Offline](#offline)) |

- **Filter:** case-insensitive substring over title and channel, rebuilt on
  every `EN_CHANGE` (`ID_EDIT`, code `0x300`) while the source is Library.
  The view is a `Vec<usize>` of index positions, and `LB_SETCOUNT` follows it.
- **Rows:** the same layout as results (title, then channel · length). There
  is no heart (all rows are saved). The meta line ends with "File missing"
  (`ERR`) when the file has gone. A track being downloaded is not in the index
  yet, so it shows in the library only once complete. Its progress is the dim
  heart on the Player and result rows.
- **Status line** (lowest priority, after error / searching / loading /
  playing / toasts):
  - empty box: "Library · 12 tracks · 1.4 GB"
  - filtering: "2 saved · Enter: search YouTube"
  - no match: "No saved match · Enter: search YouTube"
  - empty library: "Nothing saved yet · ♥ on the player saves a track"

### Keys in the library list

| Key | Action |
| --- | --- |
| ↓ from the box, ↑ at the top row | As for results today |
| Enter, double-click | Queue = the list as shown, starting at this row |
| Shift+Enter | Queue = the list as shown, shuffled, this row first |
| Del | Remove the selected track (with undo) |
| Ctrl+Z | Undo the last remove, while its toast is showing (anywhere in the Search view; otherwise it reaches the EDIT's own text undo as today) |
| Space | Pause / resume, as for results |

- At the end of the queue playback stops, as with results. There is no loop.
- Shuffle is Fisher–Yates over a tiny time-seeded xorshift. No `rand`
  dependency.

### Hearts elsewhere

- **YouTube result rows and Player Up next rows:** a small heart at the right
  end of the meta line. Filled `ACCENT` when saved, filled `DIM` while queued
  or downloading. Nothing when not saved.
- **Player title:** the heart from [Trigger](#trigger).

**RAM:**

- `LBS_NODATA` stores nothing per row. The filter is ~8 B per visible entry.
- Playing the library clones the shown list into `queue` (as `play_result`
  already clones results): ~250 B per track while it plays, ~50 KB for 200
  tracks. If measurement says it matters, the queue can hold index positions
  instead.
- The hearts reuse the loaded icon font.

## Removing

Del on a library row:

1. Remove the entry from the index and write it.
2. Move `library\<file>` to `library\.trash\<file>`.
3. Status toast for ~5 s: `Removed "<title>" · Ctrl+Z to undo`.

- **Ctrl+Z while the toast shows:** move the file back and re-insert the entry
  at its old position.
- **Expiry:** when the toast expires, or on the next remove, delete the trashed
  file.
- **Removing the playing track:** mpv holds the file open, so the move or
  delete may fail. Then the entry goes from the index anyway and the file is
  left in `.trash\` (or in place) for the startup sweep. Playback is not
  interrupted.
- **Not from the Player.** Ctrl+S stays save-only.

**RAM:** one undo slot (an entry plus its old position).

## Size and cleanup

- **No cap and no automatic deletion.** The library holds only what was chosen.
- **Total size:** the sum of `bytes`, shown in the status line.
- **Startup sweep** (on the UI thread before the window shows, or on a short
  background thread):
  - empty `.partial\` and `.trash\`;
  - files in `library\` that the index does not name are logged, not deleted;
  - index entries whose file is missing are kept and shown as "File missing".
- **Unreadable `library.json`:** rename it to `library.json.bad` (never
  overwrite an existing `.bad`; add a suffix), start with an empty index, log
  it. The audio files stay, so the library can be rebuilt by hand.
- **Disk full** is just a download failure ("Couldn't save: …").

## Offline

- Nothing pings or checks the network. Failures show where they happen.
- **Search fails:** instead of an empty list, the list shows the library
  filtered by the query. The status line says "YouTube search failed · showing
  saved matches", and the error goes to the log.
- **Playing a saved track** needs no network.
- **Playing a streamed track offline:** "Couldn't play: …" and playback stops,
  as today. No auto-skip to the next saved track (out of scope).
- **Saving offline:** fails with yt-dlp's error, and the heart goes back to an
  outline.

## Out of scope

- Import from mattiamp, or any other library.
- MUSICA-3 search history. Nothing is reserved for it.
- Save from the results list or the tray menu. Unsave from the Player.
- Download progress percentage.
- Tags embedded in the files, cover art.
- Auto-skip past unplayable tracks.
- Library sort options or loop.

## Measuring

Run a test resident next to the installed one (`MUSICA_INSTANCE=test`,
`MUSICA_TEST_AO=null`), so the library goes to `Musica\test\`. Record
`PrivateMemorySize64` before and after, in the PR:

| What | How |
| --- | --- |
| musica idle | Empty library vs ~50 saved entries (a synthetic `library.json` is fine) |
| mpv playing | The same finite track streamed vs from the library file (60 s in, sample `time-pos` over the pipe to confirm it advances) |
| yt-dlp download | Peak private bytes of yt-dlp (plus `deno` if it spawns) while saving a ~3 min track and a 1 h+ mix |
| musica playing the library | Queue of ~50 tracks |

Audibility, and whether the heart click feels right, are for Mattia to confirm.

## Tests

Unit tests (no network):

- Index: round-trip; missing file means empty; corrupt file means set aside as
  `.bad` and start empty; atomic write leaves no `.tmp` behind.
- The yt-dlp `--print` line becomes an entry (`channel` falls back to
  `uploader`; missing fields).
- The `savable` rule: search result with or without duration; pasted link
  before and after `mpv_duration`.
- Library filter: case, title vs channel, empty query means all, newest first.
- Shuffle: the result is a permutation with the selected track first.
- `search::parse` keeps `id`.

## Implementation steps

Each step builds, passes `cargo test` and `cargo clippy --all-targets`, and
ships something usable.

1. **Library core.** New `library.rs`: paths (with the debug `MUSICA_INSTANCE`
   sub-folder), entry type, load/save (atomic, `.bad`), startup sweep,
   `is_saved`, total bytes. `Track.id` from search. Unit tests. No UI change.
2. **Save from the Player.**
   - Process-wide kill-on-close job shared by mpv and yt-dlp.
   - Download worker and FIFO, the yt-dlp command, `--print` parsing, the
     move, `WM_LIBRARY`.
   - Heart plus Ctrl+S, the `savable` refusal, status messages.
   - Measure the yt-dlp RAM here.
3. **Play from disk.** Local path in `play_current`, URL fallback, ignore
   `Title` for local files, match the playing row by id. Hearts on result and
   Up next rows. Measure mpv streamed vs local.
4. **Library in the Search view.**
   - The two list sources.
   - Empty box and live filter on `EN_CHANGE`; results stay until the next
     Enter or an empty box.
   - Library rows ("File missing").
   - Enter, double-click, Shift+Enter shuffle.
   - Status line summaries; the offline search fallback.
   - Measure idle with ~50 entries.
5. **Remove and undo.** Del, `.trash\`, the toast, Ctrl+Z, the playing-file
   case.
6. **Docs.** README (keys, library folder, status) and AGENTS.md (the
   `library.rs` row, the yt-dlp download flags, the measurement numbers).

## Open decisions

None from the grill. Two choices are left to measurement during
implementation:

- **Library queue:** cloned `Vec<Track>`, or index positions? The default is to
  clone; switch only if step 4's numbers say so.
- **yt-dlp download RAM:** if it is far above the 30–60 MB estimate, revisit
  (for example, warn while a download runs). No design change is expected.
