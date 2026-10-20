//! The saved library: `library.json` (the index, loaded once and kept in
//! memory) and the audio files it names, under `%LOCALAPPDATA%\Musica\`.
//! Debug builds with `MUSICA_INSTANCE=<name>` use `...\Musica\<name>\`.
//!
//! ```text
//! library.json                 the index, a JSON array in save order
//! library\<id>.webm | .m4a     one file per saved track
//! library\.partial\            downloads in flight
//! library\.trash\              removed files waiting for undo
//! ```
//!
//! Saved means downloaded: an entry exists only once its file is complete.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::log::log;
use crate::search::Track;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// yt-dlp's video id: the dedupe key.
    pub id: String,
    pub title: String,
    pub url: String,
    pub channel: String,
    pub duration: u64,
    /// Relative to `library\`.
    pub file: String,
    pub bytes: u64,
    /// ISO 8601 with the local UTC offset.
    pub saved_at: String,
}

/// The base folder: the log's, plus the instance name in debug test builds.
pub fn root() -> PathBuf {
    match crate::settings::instance_suffix() {
        Some(instance) => crate::log::dir().join(instance),
        None => crate::log::dir(),
    }
}

pub fn index_path(root: &Path) -> PathBuf {
    root.join("library.json")
}

pub fn audio_dir(root: &Path) -> PathBuf {
    root.join("library")
}

pub fn partial_dir(root: &Path) -> PathBuf {
    audio_dir(root).join(".partial")
}

pub fn trash_dir(root: &Path) -> PathBuf {
    audio_dir(root).join(".trash")
}

/// The index. A missing file is an empty library. An unreadable one is set
/// aside as `library.json.bad` (never overwriting an earlier `.bad`), logged,
/// and the library starts empty: the audio files stay for a rebuild by hand.
pub fn load(root: &Path) -> Vec<Entry> {
    let path = index_path(root);
    let Ok(text) = std::fs::read_to_string(&path) else {
        if path.exists() {
            // Present but not valid UTF-8 / not readable as text.
            set_aside(&path);
        }
        return Vec::new();
    };
    match serde_json::from_str::<Vec<Entry>>(&text) {
        Ok(entries) => entries,
        Err(e) => {
            log(format!("library.json unreadable: {e}"));
            set_aside(&path);
            Vec::new()
        }
    }
}

fn set_aside(path: &Path) {
    let mut bad = PathBuf::from(format!("{}.bad", path.display()));
    let mut n = 1;
    while bad.exists() {
        bad = PathBuf::from(format!("{}.bad.{n}", path.display()));
        n += 1;
    }
    match std::fs::rename(path, &bad) {
        Ok(()) => log(format!("bad index kept as {}", bad.display())),
        Err(e) => log(format!("could not set the bad index aside: {e}")),
    }
}

/// Write through `library.json.tmp`, then rename over the index.
pub fn save(root: &Path, entries: &[Entry]) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(entries).map_err(|e| e.to_string())?;
    let path = index_path(root);
    let tmp = root.join("library.json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Where a removed entry's file went.
#[derive(Debug, PartialEq, Eq)]
pub enum Trashed {
    /// Moved to `.trash\`: undo moves it back.
    Moved,
    /// Not on disk (the row said "File missing"): nothing to hold.
    Missing,
    /// Still in `library\`, in use (mpv has it open). A `.del` marker in
    /// `.trash\` tells the next startup to delete it.
    Held,
}

/// A file name with no folder part: the only kind that is ever moved or deleted.
fn plain(file: &str) -> bool {
    !file.is_empty() && Path::new(file).file_name().is_some_and(|n| n == file)
}

fn marker(root: &Path, file: &str) -> PathBuf {
    trash_dir(root).join(format!("{file}.del"))
}

/// Move an entry's file to `.trash\` (replacing an older file of that name).
pub fn trash_file(root: &Path, file: &str) -> Trashed {
    if !plain(file) {
        return Trashed::Missing;
    }
    let from = audio_dir(root).join(file);
    if !from.is_file() {
        return Trashed::Missing;
    }
    let to = trash_dir(root).join(file);
    let moved = std::fs::create_dir_all(trash_dir(root))
        .and_then(|()| std::fs::rename(&from, &to))
        .is_ok();
    if moved {
        return Trashed::Moved;
    }
    log(format!("library: {file} is in use, kept for a later delete"));
    let _ = std::fs::write(marker(root, file), "");
    Trashed::Held
}

/// Undo: move a trashed file back into `library\`.
pub fn restore_file(root: &Path, file: &str) -> Result<(), String> {
    if !plain(file) {
        return Err("bad file name".into());
    }
    std::fs::rename(trash_dir(root).join(file), audio_dir(root).join(file))
        .map_err(|e| e.to_string())
}

/// The undo window is over: delete the trashed file.
pub fn discard_trashed(root: &Path, file: &str) {
    if plain(file) {
        let _ = std::fs::remove_file(trash_dir(root).join(file));
    }
}

/// Undo of a held file: it never left, so only the marker goes.
pub fn unmark(root: &Path, file: &str) {
    if plain(file) {
        let _ = std::fs::remove_file(marker(root, file));
    }
}

/// Try to delete a held file. True once it is settled: deleted, already gone,
/// or taken over by a new entry of the same name (then it is not ours to delete).
pub fn delete_held(root: &Path, entries: &[Entry], file: &str) -> bool {
    if !plain(file) {
        return true;
    }
    if entries.iter().any(|e| e.file == file) {
        unmark(root, file);
        return true;
    }
    match std::fs::remove_file(audio_dir(root).join(file)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return false,
    }
    unmark(root, file);
    true
}

/// Startup: delete the files marked in `.trash\`, empty `.partial\` and
/// `.trash\` (interrupted downloads, removed files), and log the files in
/// `library\` that the index does not name. Orphans are never deleted.
/// Returns them for the caller and the tests.
pub fn sweep(root: &Path, entries: &[Entry]) -> Vec<String> {
    clear_dir(&partial_dir(root));
    if let Ok(items) = std::fs::read_dir(trash_dir(root)) {
        for item in items.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            if let Some(file) = name.strip_suffix(".del") {
                delete_held(root, entries, file);
            }
        }
    }
    clear_dir(&trash_dir(root));
    let mut orphans = Vec::new();
    if let Ok(dir) = std::fs::read_dir(audio_dir(root)) {
        for item in dir.flatten() {
            if !item.path().is_file() {
                continue;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            if !entries.iter().any(|e| e.file == name) {
                log(format!("library: orphan file {name} (kept)"));
                orphans.push(name);
            }
        }
    }
    orphans
}

/// Remove everything inside `dir`, keeping `dir`.
pub fn clear_dir(dir: &Path) {
    let Ok(items) = std::fs::read_dir(dir) else {
        return;
    };
    for item in items.flatten() {
        let path = item.path();
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

/// Saved if the index has the track's id; a pasted link has no id until it is
/// saved, so its URL is matched instead.
pub fn is_saved(entries: &[Entry], track: &Track) -> bool {
    find(entries, track).is_some()
}

/// The entry for a track: by id, or by URL for a pasted link without one.
pub fn find<'a>(entries: &'a [Entry], track: &Track) -> Option<&'a Entry> {
    match &track.id {
        Some(id) => entries.iter().find(|e| &e.id == id),
        None => entries.iter().find(|e| e.url == track.url),
    }
}

impl Entry {
    /// The queue form of a saved entry. A zero length (yt-dlp gave none) is
    /// left to mpv, like a pasted link, rather than shown as live.
    pub fn track(&self) -> Track {
        Track {
            title: self.title.clone(),
            channel: self.channel.clone(),
            duration: (self.duration > 0).then_some(self.duration),
            url: self.url.clone(),
            id: Some(self.id.clone()),
            from_search: self.duration > 0,
        }
    }
}

/// Positions in `entries` matching `query` (case-insensitive substring of the
/// title or channel), newest first: the index is in save order. An empty query
/// matches everything.
pub fn filter(entries: &[Entry], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    (0..entries.len())
        .rev()
        .filter(|&i| {
            let e = &entries[i];
            q.is_empty()
                || e.title.to_lowercase().contains(&q)
                || e.channel.to_lowercase().contains(&q)
        })
        .collect()
}

/// Shuffle `items` in place with `items[first]` moved to the front and the
/// rest in a random order (Fisher–Yates over xorshift64; no `rand`).
pub fn shuffle_first<T>(items: &mut [T], first: usize, seed: u64) {
    if items.is_empty() {
        return;
    }
    items.swap(0, first.min(items.len() - 1));
    let mut x = seed | 1; // xorshift must not start at 0
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for i in (2..items.len()).rev() {
        // j in 1..=i
        let j = 1 + (next() % i as u64) as usize;
        items.swap(i, j);
    }
}

/// A seed that differs from run to run.
pub fn time_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
}

/// "1.4 GB", "312 MB", "48 KB".
pub fn fmt_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.1} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.0} MB", b / (KB * KB))
    } else {
        format!("{:.0} KB", b / KB)
    }
}

/// The saved file to play instead of the URL, with its entry. `None` when the
/// track is not saved or its file has gone (the caller then streams the URL).
pub fn local_file<'a>(
    root: &Path,
    entries: &'a [Entry],
    track: &Track,
) -> Option<(&'a Entry, PathBuf)> {
    let entry = find(entries, track)?;
    let path = audio_dir(root).join(&entry.file);
    path.is_file().then_some((entry, path))
}

/// Only a track with a known, finite length can be saved. A search result
/// without a duration is live; a pasted link is known once mpv reports one.
pub fn savable(track: &Track, mpv_duration: Option<f64>) -> bool {
    if track.from_search {
        track.duration.is_some()
    } else {
        mpv_duration.is_some_and(|d| d > 0.0)
    }
}

/// What yt-dlp's `--print after_move:` line says about a finished download.
#[derive(Clone, Debug, PartialEq)]
pub struct Printed {
    pub id: String,
    pub title: String,
    pub url: String,
    pub channel: String,
    pub duration: u64,
    pub ext: String,
    pub filepath: PathBuf,
}

/// Fields asked of yt-dlp, in `%(.{...})j` form.
pub const PRINT_TEMPLATE: &str =
    "after_move:%(.{id,title,channel,uploader,duration,ext,filepath,webpage_url})j";

/// The last JSON line of yt-dlp's stdout becomes a `Printed`. `channel` falls
/// back to `uploader`, as in `search::parse`; `fallback_url` is used when
/// yt-dlp gives no `webpage_url`.
pub fn parse_print(stdout: &str, fallback_url: &str) -> Result<Printed, String> {
    let line = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
        .ok_or("yt-dlp printed no metadata")?;
    let v: Value = serde_json::from_str(line).map_err(|e| format!("bad yt-dlp output: {e}"))?;
    let text = |k: &str| v[k].as_str().filter(|s| !s.is_empty());
    let id = text("id").ok_or("yt-dlp gave no video id")?;
    let filepath = PathBuf::from(text("filepath").ok_or("yt-dlp gave no file path")?);
    let ext = text("ext")
        .map(str::to_string)
        .or_else(|| {
            filepath
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
        })
        .ok_or("yt-dlp gave no file extension")?;
    Ok(Printed {
        id: id.to_string(),
        title: text("title").unwrap_or(id).to_string(),
        url: text("webpage_url").unwrap_or(fallback_url).to_string(),
        channel: text("channel")
            .or(text("uploader"))
            .unwrap_or("")
            .to_string(),
        duration: v["duration"].as_f64().unwrap_or(0.0) as u64,
        ext,
        filepath,
    })
}

/// The name a saved file gets. Ids from other sites can hold characters that
/// are not safe in a file name; those are dropped.
fn file_name(p: &Printed) -> Result<String, String> {
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect()
    };
    let (id, ext) = (clean(&p.id), clean(&p.ext));
    if id.is_empty() || ext.is_empty() {
        return Err(format!("unusable id or extension: {} / {}", p.id, p.ext));
    }
    Ok(format!("{id}.{ext}"))
}

/// `<stem>.orphan.<ext>`, or with `.2`, `.3`... until the name is free.
fn free_aside(dest: &Path) -> PathBuf {
    let stem = dest.file_stem().unwrap_or_default().to_string_lossy();
    let ext = dest.extension().unwrap_or_default().to_string_lossy();
    (1u32..)
        .map(|n| {
            let tag = if n == 1 { String::new() } else { format!(".{n}") };
            dest.with_file_name(format!("{stem}.orphan{tag}.{ext}"))
        })
        .find(|p| !p.exists())
        .expect("u32 range exhausted")
}

/// Move a finished download into `library\` and build its entry. The caller
/// adds the entry to the index and writes it.
pub fn finalize(root: &Path, p: &Printed, saved_at: String) -> Result<Entry, String> {
    let file = file_name(p)?;
    let dest = audio_dir(root).join(&file);
    std::fs::create_dir_all(audio_dir(root)).map_err(|e| e.to_string())?;
    // Orphans are never deleted (spec decision 14), and a rename replaces its
    // target: move a file already at `dest` aside first, then log it.
    if dest.exists() {
        let aside = free_aside(&dest);
        std::fs::rename(&dest, &aside)
            .map_err(|e| format!("could not set the existing file aside: {e}"))?;
        log(format!(
            "library: {file} already on disk without an entry, kept as {}",
            aside.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    std::fs::rename(&p.filepath, &dest).map_err(|e| format!("could not move the file: {e}"))?;
    let bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    Ok(Entry {
        id: p.id.clone(),
        title: p.title.clone(),
        url: p.url.clone(),
        channel: p.channel.clone(),
        duration: p.duration,
        file,
        bytes,
        saved_at,
    })
}

/// `2026-10-20T11:02:13+02:00`.
pub fn iso((y, mo, d, h, mi, s): (u16, u16, u16, u16, u16, u16), offset_minutes: i32) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let off = offset_minutes.abs();
    format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}{sign}{:02}:{:02}",
        off / 60,
        off % 60
    )
}

/// Minutes east of UTC from `GetTimeZoneInformation`'s return value and
/// biases (all minutes west). `None` for `TIME_ZONE_ID_INVALID` or anything
/// unknown: the biases mean nothing then.
fn offset_from_zone(id: u32, bias: i32, standard: i32, daylight: i32) -> Option<i32> {
    match id {
        0 | 1 => Some(-(bias + standard)), // UNKNOWN (no DST rules) / STANDARD
        2 => Some(-(bias + daylight)),     // DAYLIGHT
        _ => None,
    }
}

/// Local time now, with the current UTC offset (daylight saving included).
pub fn now_iso() -> String {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    use windows::Win32::System::Time::{
        GetTimeZoneInformation, SystemTimeToFileTime, TIME_ZONE_INFORMATION,
    };
    unsafe {
        let t = GetLocalTime();
        let mut tz = TIME_ZONE_INFORMATION::default();
        let id = GetTimeZoneInformation(&mut tz);
        let offset = offset_from_zone(id, tz.Bias, tz.StandardBias, tz.DaylightBias)
            .unwrap_or_else(|| {
                // No usable zone: the difference between the local and UTC
                // clocks, read at the same moment, is the offset.
                let ticks = |st| {
                    let mut ft = FILETIME::default();
                    SystemTimeToFileTime(&st, &mut ft).ok()?;
                    Some((ft.dwHighDateTime as i64) << 32 | ft.dwLowDateTime as i64)
                };
                match (ticks(t), ticks(GetSystemTime())) {
                    // 100 ns ticks; round to the nearest minute (the two
                    // reads can straddle a second).
                    (Some(l), Some(u)) => ((l - u) as f64 / 600_000_000.0).round() as i32,
                    _ => 0,
                }
            });
        iso(
            (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond),
            offset,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "musica-library-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(id: &str) -> Entry {
        Entry {
            id: id.into(),
            title: format!("title {id}"),
            url: format!("https://www.youtube.com/watch?v={id}"),
            channel: "chan".into(),
            duration: 181,
            file: format!("{id}.webm"),
            bytes: 1000,
            saved_at: "2026-10-20T11:02:13+02:00".into(),
        }
    }

    fn track(id: Option<&str>, duration: Option<u64>, from_search: bool) -> Track {
        Track {
            title: "t".into(),
            channel: String::new(),
            duration,
            url: "https://www.youtube.com/watch?v=abc".into(),
            id: id.map(str::to_string),
            from_search,
        }
    }

    #[test]
    fn index_round_trip_and_no_tmp_left() {
        let root = scratch("roundtrip");
        let entries = vec![entry("a"), entry("b")];
        save(&root, &entries).unwrap();
        assert_eq!(load(&root), entries);
        // A second save overwrites the first.
        let more = vec![entry("a"), entry("b"), entry("c")];
        save(&root, &more).unwrap();
        assert_eq!(load(&root), more);
        assert!(!root.join("library.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_index_is_empty() {
        let root = scratch("missing");
        assert!(load(&root).is_empty());
        assert!(!index_path(&root).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_index_is_set_aside_and_kept() {
        let root = scratch("corrupt");
        std::fs::write(index_path(&root), "{ not json").unwrap();
        assert!(load(&root).is_empty());
        assert!(!index_path(&root).exists());
        let bad = root.join("library.json.bad");
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{ not json");
        // A second corrupt file does not overwrite the first `.bad`.
        std::fs::write(index_path(&root), "[oops").unwrap();
        assert!(load(&root).is_empty());
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{ not json");
        assert_eq!(
            std::fs::read_to_string(root.join("library.json.bad.1")).unwrap(),
            "[oops"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn print_line_becomes_an_entry() {
        let line = r#"{"id":"wdDhTr_M7kA","title":"the playlist","channel":"Pure Grit","uploader":"u","duration":7381.0,"ext":"webm","filepath":"C:\\x\\.partial\\wdDhTr_M7kA.webm","webpage_url":"https://www.youtube.com/watch?v=wdDhTr_M7kA"}"#;
        let p = parse_print(&format!("noise\n{line}\n"), "https://fallback").unwrap();
        assert_eq!(p.id, "wdDhTr_M7kA");
        assert_eq!(p.title, "the playlist");
        assert_eq!(p.channel, "Pure Grit");
        assert_eq!(p.duration, 7381);
        assert_eq!(p.ext, "webm");
        assert_eq!(p.url, "https://www.youtube.com/watch?v=wdDhTr_M7kA");

        // channel falls back to uploader; missing fields get defaults.
        let p = parse_print(
            r#"{"id":"x1","title":null,"channel":null,"uploader":"Up","duration":null,"ext":null,"filepath":"C:\\p\\x1.m4a"}"#,
            "https://fallback",
        )
        .unwrap();
        assert_eq!(p.title, "x1");
        assert_eq!(p.channel, "Up");
        assert_eq!(p.duration, 0);
        assert_eq!(p.ext, "m4a");
        assert_eq!(p.url, "https://fallback");

        assert!(parse_print("", "u").is_err());
        assert!(parse_print(r#"{"title":"no id","filepath":"x.webm"}"#, "u").is_err());
        assert!(parse_print(r#"{"id":"a"}"#, "u").is_err());
    }

    #[test]
    fn finalize_moves_the_file_and_measures_it() {
        let root = scratch("finalize");
        std::fs::create_dir_all(partial_dir(&root)).unwrap();
        let src = partial_dir(&root).join("a1.webm");
        std::fs::write(&src, vec![0u8; 321]).unwrap();
        let p = Printed {
            id: "a1".into(),
            title: "T".into(),
            url: "u".into(),
            channel: "C".into(),
            duration: 5,
            ext: "webm".into(),
            filepath: src.clone(),
        };
        let e = finalize(&root, &p, "now".into()).unwrap();
        assert_eq!((e.file.as_str(), e.bytes), ("a1.webm", 321));
        assert!(!src.exists());
        assert!(audio_dir(&root).join("a1.webm").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finalize_keeps_an_existing_orphan() {
        let root = scratch("orphan");
        std::fs::create_dir_all(partial_dir(&root)).unwrap();
        std::fs::create_dir_all(audio_dir(&root)).unwrap();
        std::fs::write(audio_dir(&root).join("a1.webm"), b"old").unwrap();
        std::fs::write(audio_dir(&root).join("a1.orphan.webm"), b"older").unwrap();
        let src = partial_dir(&root).join("a1.webm");
        std::fs::write(&src, b"new!").unwrap();
        let p = Printed {
            id: "a1".into(),
            title: "T".into(),
            url: "u".into(),
            channel: "C".into(),
            duration: 5,
            ext: "webm".into(),
            filepath: src,
        };
        let e = finalize(&root, &p, "now".into()).unwrap();
        assert_eq!(e.bytes, 4);
        let read = |n: &str| std::fs::read(audio_dir(&root).join(n)).unwrap();
        assert_eq!(read("a1.webm"), b"new!");
        assert_eq!(read("a1.orphan.2.webm"), b"old");
        assert_eq!(read("a1.orphan.webm"), b"older");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zone_offsets() {
        // Rome: bias -60, daylight -60 more.
        assert_eq!(offset_from_zone(1, -60, 0, -60), Some(60));
        assert_eq!(offset_from_zone(2, -60, 0, -60), Some(120));
        assert_eq!(offset_from_zone(0, -60, 0, -60), Some(60));
        assert_eq!(offset_from_zone(0xFFFF_FFFF, -60, 0, -60), None);
    }

    #[test]
    fn unsafe_ids_cannot_escape_the_folder() {
        let mut p = parse_print(r#"{"id":"../x","ext":"webm","filepath":"f"}"#, "u").unwrap();
        assert_eq!(file_name(&p).unwrap(), "x.webm");
        p.id = "..".into();
        assert!(file_name(&p).is_err());
    }

    #[test]
    fn sweep_clears_leftovers_and_reports_orphans() {
        let root = scratch("sweep");
        std::fs::create_dir_all(partial_dir(&root)).unwrap();
        std::fs::create_dir_all(trash_dir(&root)).unwrap();
        std::fs::write(partial_dir(&root).join("a.webm.part"), "x").unwrap();
        std::fs::write(trash_dir(&root).join("old.webm"), "x").unwrap();
        std::fs::write(audio_dir(&root).join("a.webm"), "x").unwrap();
        std::fs::write(audio_dir(&root).join("stray.webm"), "x").unwrap();
        let orphans = sweep(&root, &[entry("a")]);
        assert_eq!(orphans, vec!["stray.webm".to_string()]);
        assert_eq!(std::fs::read_dir(partial_dir(&root)).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(trash_dir(&root)).unwrap().count(), 0);
        assert!(audio_dir(&root).join("stray.webm").is_file());
        assert!(audio_dir(&root).join("a.webm").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn put(root: &Path, file: &str) {
        std::fs::create_dir_all(audio_dir(root)).unwrap();
        std::fs::write(audio_dir(root).join(file), "x").unwrap();
    }

    #[test]
    fn trash_then_restore_or_discard() {
        let root = scratch("trash");
        put(&root, "a.webm");
        assert_eq!(trash_file(&root, "a.webm"), Trashed::Moved);
        assert!(!audio_dir(&root).join("a.webm").exists());
        assert!(trash_dir(&root).join("a.webm").is_file());
        restore_file(&root, "a.webm").unwrap();
        assert!(audio_dir(&root).join("a.webm").is_file());
        // A second trash replaces an older file of the same name, then goes.
        std::fs::create_dir_all(trash_dir(&root)).unwrap();
        std::fs::write(trash_dir(&root).join("a.webm"), "old").unwrap();
        assert_eq!(trash_file(&root, "a.webm"), Trashed::Moved);
        discard_trashed(&root, "a.webm");
        assert!(!trash_dir(&root).join("a.webm").exists());
        // A file that is not there, or a name with a folder in it.
        assert_eq!(trash_file(&root, "nope.webm"), Trashed::Missing);
        assert_eq!(trash_file(&root, "..\\library.json"), Trashed::Missing);
        assert!(restore_file(&root, "..\\x").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_in_use_is_held_and_deleted_later() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = scratch("held");
        put(&root, "a.webm");
        let path = audio_dir(&root).join("a.webm");
        // Share read only: no delete, no rename, like mpv's open file.
        let open = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert_eq!(trash_file(&root, "a.webm"), Trashed::Held);
        assert!(path.is_file());
        assert!(marker(&root, "a.webm").is_file());
        assert!(!delete_held(&root, &[], "a.webm"));
        drop(open);
        assert!(delete_held(&root, &[], "a.webm"));
        assert!(!path.exists());
        assert!(!marker(&root, "a.webm").exists());
        // Gone already counts as settled.
        assert!(delete_held(&root, &[], "a.webm"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_held_file_taken_over_by_a_new_entry_is_not_deleted() {
        let root = scratch("retaken");
        put(&root, "a.webm");
        let _ = std::fs::create_dir_all(trash_dir(&root));
        std::fs::write(marker(&root, "a.webm"), "").unwrap();
        assert!(delete_held(&root, &[entry("a")], "a.webm"));
        assert!(audio_dir(&root).join("a.webm").is_file());
        assert!(!marker(&root, "a.webm").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sweep_deletes_marked_files_but_not_orphans_or_entries() {
        let root = scratch("sweepdel");
        put(&root, "held.webm");
        put(&root, "kept.webm");
        put(&root, "stray.webm");
        std::fs::create_dir_all(trash_dir(&root)).unwrap();
        std::fs::write(marker(&root, "held.webm"), "").unwrap();
        std::fs::write(trash_dir(&root).join("undone.webm"), "x").unwrap();
        let orphans = sweep(&root, &[entry("kept")]);
        assert_eq!(orphans, vec!["stray.webm".to_string()]);
        assert!(!audio_dir(&root).join("held.webm").exists());
        assert!(audio_dir(&root).join("kept.webm").is_file());
        assert_eq!(std::fs::read_dir(trash_dir(&root)).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn savable_rule() {
        // Search results: no duration means live.
        assert!(savable(&track(Some("a"), Some(200), true), None));
        assert!(!savable(&track(Some("a"), None, true), Some(200.0)));
        // Pasted link: only once mpv has reported a duration.
        assert!(!savable(&track(None, None, false), None));
        assert!(savable(&track(None, None, false), Some(200.0)));
    }

    #[test]
    fn saved_lookup_by_id_or_url() {
        let entries = vec![entry("abc"), entry("zzz")];
        assert!(is_saved(&entries, &track(Some("abc"), Some(1), true)));
        assert!(!is_saved(&entries, &track(Some("nope"), Some(1), true)));
        // A pasted link has no id: its URL decides.
        assert!(is_saved(&entries, &track(None, None, false)));
    }

    #[test]
    fn local_file_only_while_the_file_exists() {
        let root = scratch("local");
        let entries = vec![entry("abc")];
        let t = track(Some("abc"), Some(181), true);
        // Indexed but the file is gone: stream the URL.
        assert!(local_file(&root, &entries, &t).is_none());
        std::fs::create_dir_all(audio_dir(&root)).unwrap();
        std::fs::write(audio_dir(&root).join("abc.webm"), "x").unwrap();
        let (e, path) = local_file(&root, &entries, &t).unwrap();
        assert_eq!(e.title, "title abc");
        assert_eq!(path, audio_dir(&root).join("abc.webm"));
        // Not in the index at all.
        assert!(local_file(&root, &entries, &track(Some("other"), Some(1), true)).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn named(id: &str, title: &str, channel: &str) -> Entry {
        Entry {
            title: title.into(),
            channel: channel.into(),
            ..entry(id)
        }
    }

    #[test]
    fn filter_is_case_insensitive_over_title_and_channel_newest_first() {
        let entries = vec![
            named("a", "Lofi Beats", "Chill Cow"),
            named("b", "Rain sounds", "Nature"),
            named("c", "Jazz", "LOFI girl"),
        ];
        // Empty (or blank) query: everything, newest (last saved) first.
        assert_eq!(filter(&entries, ""), vec![2, 1, 0]);
        assert_eq!(filter(&entries, "  "), vec![2, 1, 0]);
        // Title and channel both match, whatever the case.
        assert_eq!(filter(&entries, "lofi"), vec![2, 0]);
        assert_eq!(filter(&entries, "LOFI"), vec![2, 0]);
        // Title only, channel only.
        assert_eq!(filter(&entries, "rain"), vec![1]);
        assert_eq!(filter(&entries, "cow"), vec![0]);
        // A substring, not a prefix; no match is empty.
        assert_eq!(filter(&entries, "eat"), vec![0]);
        assert!(filter(&entries, "zzz").is_empty());
        assert!(filter(&[], "x").is_empty());
    }

    #[test]
    fn shuffle_keeps_everything_with_the_selected_first() {
        for seed in [0, 1, 42, u64::MAX, time_seed()] {
            for first in [0, 3, 9] {
                let mut v: Vec<u32> = (0..10).collect();
                shuffle_first(&mut v, first, seed);
                assert_eq!(v[0], first as u32);
                let mut sorted = v.clone();
                sorted.sort_unstable();
                assert_eq!(sorted, (0..10).collect::<Vec<u32>>());
            }
        }
        // It does reorder the rest (some seed differs from the identity).
        let moved = (1..20u64).any(|seed| {
            let mut v: Vec<u32> = (0..10).collect();
            shuffle_first(&mut v, 0, seed);
            v != (0..10).collect::<Vec<u32>>()
        });
        assert!(moved);
        // Tiny inputs.
        let mut none: Vec<u32> = Vec::new();
        shuffle_first(&mut none, 0, 1);
        let mut one = vec![7];
        shuffle_first(&mut one, 0, 1);
        assert_eq!(one, vec![7]);
        let mut two = vec![1, 2];
        shuffle_first(&mut two, 1, 5);
        assert_eq!(two, vec![2, 1]);
    }

    #[test]
    fn entry_track_keeps_id_and_leaves_zero_length_to_mpv() {
        let t = entry("abc").track();
        assert_eq!(t.id.as_deref(), Some("abc"));
        assert_eq!(t.duration, Some(181));
        assert!(t.from_search);
        let mut e = entry("abc");
        e.duration = 0;
        let t = e.track();
        assert_eq!(t.duration, None);
        assert!(!t.from_search);
    }

    #[test]
    fn byte_sizes() {
        assert_eq!(fmt_bytes(0), "0 KB");
        assert_eq!(fmt_bytes(48 * 1024), "48 KB");
        assert_eq!(fmt_bytes(312 * 1024 * 1024), "312 MB");
        assert_eq!(fmt_bytes(1_503_238_554), "1.4 GB");
    }

    #[test]
    fn iso_format() {
        assert_eq!(
            iso((2026, 10, 20, 11, 2, 13), 120),
            "2026-10-20T11:02:13+02:00"
        );
        assert_eq!(
            iso((2026, 1, 2, 3, 4, 5), -330),
            "2026-01-02T03:04:05-05:30"
        );
        assert!(now_iso().len() >= 25);
    }
}
