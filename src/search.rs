//! YouTube search through `yt-dlp ytsearchN:<query>` (flat, no extraction).

use serde_json::Value;
use std::process::Stdio;

use crate::tools;

pub const LIMIT: usize = 10;

#[derive(Clone, Debug)]
pub struct Track {
    pub title: String,
    pub channel: String,
    /// Seconds; `None` for live streams.
    pub duration: Option<u64>,
    pub url: String,
    /// The YouTube video id: from the search, or read out of a pasted YouTube
    /// link. `None` for any other link; a save then takes yt-dlp's id.
    pub id: Option<String>,
    /// False for a pasted link, whose length comes from mpv instead.
    pub from_search: bool,
}

pub fn fmt_secs(s: u64) -> String {
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

impl Track {
    /// A pasted link: no search metadata. Its id is only what the URL itself
    /// says (see `youtube_id`), enough to know a saved track before any
    /// download. yt-dlp's own id still decides when a download finishes.
    pub fn from_link(url: String) -> Self {
        Self {
            title: url.clone(),
            channel: String::new(),
            duration: None,
            id: youtube_id(&url),
            url,
            from_search: false,
        }
    }

    /// Same video: ids when both have one, else the URL. The library, the
    /// results and the queue all point at one video with different `url`s.
    pub fn is_same(&self, other: &Track) -> bool {
        match (&self.id, &other.id) {
            (Some(a), Some(b)) => a == b,
            _ => self.url == other.url,
        }
    }

    pub fn meta(&self) -> String {
        let length = self.duration.map(fmt_secs).unwrap_or_else(|| "live".into());
        if self.channel.is_empty() {
            length
        } else {
            format!("{}  ·  {length}", self.channel)
        }
    }
}

/// The video id in a YouTube link (`watch?v=`, `youtu.be/`, `/shorts/`,
/// `/live/`), or `None`.
pub fn youtube_id(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let query = query.split('#').next().unwrap_or("");
    let path = path.split('#').next().unwrap_or("");
    let candidate = if host == "youtu.be" {
        path.split('/').next()
    } else if host == "youtube.com" || host.ends_with(".youtube.com") {
        if path == "watch" {
            query.split('&').find_map(|kv| kv.strip_prefix("v="))
        } else {
            path.strip_prefix("shorts/")
                .or_else(|| path.strip_prefix("live/"))
                .and_then(|p| p.split('/').next())
        }
    } else {
        None
    }?;
    let ok = candidate.len() == 11
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| candidate.to_string())
}

pub fn is_url(q: &str) -> bool {
    let q = q.trim();
    q.starts_with("https://") || q.starts_with("http://")
}

pub fn run(query: &str) -> Result<Vec<Track>, String> {
    let ytdlp =
        tools::find("yt-dlp.exe").ok_or("yt-dlp not found (winget install yt-dlp.yt-dlp)")?;
    let out = tools::command(&ytdlp)
        .args([
            "--flat-playlist",
            "--dump-single-json",
            "--no-warnings",
            "--skip-download",
        ])
        .arg(format!("ytsearch{LIMIT}:{query}"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("yt-dlp failed to start: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err
            .lines()
            .last()
            .unwrap_or("yt-dlp failed")
            .trim()
            .to_string());
    }
    parse(&out.stdout)
}

pub fn parse(json: &[u8]) -> Result<Vec<Track>, String> {
    let v: Value = serde_json::from_slice(json).map_err(|e| format!("bad yt-dlp output: {e}"))?;
    let entries = v["entries"].as_array().cloned().unwrap_or_default();
    Ok(entries
        .iter()
        .filter_map(|e| {
            // Videos only: channels and playlists come back as `YoutubeTab`.
            if e["ie_key"].as_str().is_some_and(|k| k != "Youtube") {
                return None;
            }
            let id = e["id"].as_str()?;
            let url = e["url"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={id}"));
            Some(Track {
                title: e["title"].as_str().unwrap_or(id).to_string(),
                channel: e["channel"]
                    .as_str()
                    .or(e["uploader"].as_str())
                    .unwrap_or("")
                    .to_string(),
                duration: e["duration"].as_f64().map(|d| d as u64),
                url,
                id: Some(id.to_string()),
                from_search: true,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_search() {
        let json = br#"{"entries":[
            {"id":"a1","title":"Lofi","channel":"Girl","duration":3882.0,"url":"https://www.youtube.com/watch?v=a1"},
            {"id":"UCx","ie_key":"YoutubeTab","title":"A channel","url":"https://www.youtube.com/channel/UCx"},
            {"id":"b2","ie_key":"Youtube","title":"Radio","uploader":"Up","duration":null},
            {"title":"no id"}
        ]}"#;
        let t = parse(json).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].meta(), "Girl  ·  1:04:42");
        assert_eq!(t[0].id.as_deref(), Some("a1"));
        assert_eq!(t[1].id.as_deref(), Some("b2"));
        assert_eq!(t[1].url, "https://www.youtube.com/watch?v=b2");
        assert_eq!(t[1].meta(), "Up  ·  live");
    }

    #[test]
    fn pasted_link_takes_its_id_from_the_url() {
        let t = Track::from_link("https://youtu.be/dQw4w9WgXcQ?si=abc".into());
        assert_eq!(t.id.as_deref(), Some("dQw4w9WgXcQ"));
        assert!(!t.from_search);
        assert_eq!(t.title, t.url);
        let other = Track::from_link("https://example.com/a".into());
        assert!(other.id.is_none());
    }

    #[test]
    fn youtube_ids() {
        let id = Some("dQw4w9WgXcQ".to_string());
        for u in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?list=PL1&v=dQw4w9WgXcQ&t=5s",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ#x",
            "https://youtu.be/dQw4w9WgXcQ",
            "http://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ?feature=share",
        ] {
            assert_eq!(youtube_id(u), id, "{u}");
        }
        for u in [
            "https://www.youtube.com/playlist?list=PLabc",
            "https://www.youtube.com/watch?v=short",
            "https://notyoutube.com/watch?v=dQw4w9WgXcQ",
            "https://example.com/dQw4w9WgXcQ",
            "lofi",
        ] {
            assert_eq!(youtube_id(u), None, "{u}");
        }
    }

    #[test]
    fn same_video_by_id_else_url() {
        let a = Track::from_link("https://youtu.be/dQw4w9WgXcQ".into());
        let b = Track::from_link("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=5".into());
        assert_ne!(a.url, b.url);
        assert!(a.is_same(&b));
        let c = Track::from_link("https://example.com/a".into());
        assert!(c.is_same(&c.clone()));
        assert!(!c.is_same(&a));
    }

    #[test]
    fn urls() {
        assert!(is_url(" https://youtu.be/x "));
        assert!(!is_url("lofi hip hop"));
    }
}
