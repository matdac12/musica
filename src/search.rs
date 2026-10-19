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
}

impl Track {
    pub fn meta(&self) -> String {
        let length = match self.duration {
            Some(s) if s >= 3600 => format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60),
            Some(s) => format!("{}:{:02}", s / 60, s % 60),
            None => "live".into(),
        };
        if self.channel.is_empty() {
            length
        } else {
            format!("{}  ·  {length}", self.channel)
        }
    }
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
            {"id":"b2","title":"Radio","uploader":"Up","duration":null},
            {"title":"no id"}
        ]}"#;
        let t = parse(json).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].meta(), "Girl  ·  1:04:42");
        assert_eq!(t[1].url, "https://www.youtube.com/watch?v=b2");
        assert_eq!(t[1].meta(), "Up  ·  live");
    }

    #[test]
    fn urls() {
        assert!(is_url(" https://youtu.be/x "));
        assert!(!is_url("lofi hip hop"));
    }
}
