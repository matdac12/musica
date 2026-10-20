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

/// Startup: empty `.partial\` and `.trash\` (interrupted downloads, removed
/// files), and log the files in `library\` that the index does not name.
/// Orphans are never deleted. Returns them for the caller and the tests.
pub fn sweep(root: &Path, entries: &[Entry]) -> Vec<String> {
    clear_dir(&partial_dir(root));
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
    match &track.id {
        Some(id) => entries.iter().any(|e| &e.id == id),
        None => entries.iter().any(|e| e.url == track.url),
    }
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

/// Move a finished download into `library\` and build its entry. The caller
/// adds the entry to the index and writes it.
pub fn finalize(root: &Path, p: &Printed, saved_at: String) -> Result<Entry, String> {
    let file = file_name(p)?;
    let dest = audio_dir(root).join(&file);
    std::fs::create_dir_all(audio_dir(root)).map_err(|e| e.to_string())?;
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

/// Local time now, with the current UTC offset (daylight saving included).
pub fn now_iso() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    unsafe {
        let t = GetLocalTime();
        let mut tz = TIME_ZONE_INFORMATION::default();
        let daylight = GetTimeZoneInformation(&mut tz) == 2; // TIME_ZONE_ID_DAYLIGHT
        let bias = tz.Bias
            + if daylight {
                tz.DaylightBias
            } else {
                tz.StandardBias
            };
        iso(
            (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond),
            -bias,
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
