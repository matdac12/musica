//! The download worker: one thread, a FIFO of URLs, one yt-dlp at a time.
//! Each yt-dlp joins the shared kill-on-close job, so it never outlives
//! musica, and has no timeout (a long mix on a slow line may take a while).
//!
//! The worker only downloads into `library\.partial\` and reports what yt-dlp
//! printed. The UI thread owns the index: it moves the file and adds the entry.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{channel, Sender};

use crate::library::{self, Printed};
use crate::{job, tools};

/// Called on the worker thread with the job's URL and its outcome.
pub type Notify = Box<dyn Fn(String, Result<Printed, String>) + Send + 'static>;

pub struct Downloader {
    tx: Sender<String>,
}

impl Downloader {
    pub fn new(root: PathBuf, notify: Notify) -> Self {
        let (tx, rx) = channel::<String>();
        std::thread::spawn(move || {
            for url in rx {
                let r = run(&root, &url);
                if let Err(e) = &r {
                    crate::log::log(format!("download {url} failed: {e}"));
                    library::clear_dir(&library::partial_dir(&root));
                }
                notify(url, r);
            }
        });
        Self { tx }
    }

    pub fn enqueue(&self, url: String) {
        let _ = self.tx.send(url);
    }
}

/// The yt-dlp arguments after the executable. Native best audio, no
/// re-encode (so no ffmpeg), the one video of a playlist link. Never
/// `player_client=android` (AGENTS.md).
fn args(partial: &Path, url: &str) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = [
        "-q",
        "--no-warnings",
        "--no-playlist",
        "--playlist-items",
        "1",
        "-f",
        "bestaudio/best",
        "--no-simulate",
        "-o",
    ]
    .map(Into::into)
    .to_vec();
    a.push(partial.join("%(id)s.%(ext)s").into());
    a.push("--print".into());
    a.push(library::PRINT_TEMPLATE.into());
    a.push("--".into());
    a.push(url.into());
    a
}

fn run(root: &Path, url: &str) -> Result<Printed, String> {
    let ytdlp =
        tools::find("yt-dlp.exe").ok_or("yt-dlp not found (winget install yt-dlp.yt-dlp)")?;
    let partial = library::partial_dir(root);
    std::fs::create_dir_all(&partial).map_err(|e| e.to_string())?;
    crate::log::log(format!("download start {url}"));
    let child = tools::command(&ytdlp)
        .args(args(&partial, url))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("yt-dlp failed to start: {e}"))?;
    job::join(&child);
    let out = child
        .wait_with_output()
        .map_err(|e| format!("yt-dlp failed: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        crate::log::log(format!("yt-dlp stderr: {}", stderr.trim()));
        return Err(
            last_line(&stderr).unwrap_or_else(|| format!("yt-dlp exited with {}", out.status))
        );
    }
    let printed = library::parse_print(&String::from_utf8_lossy(&out.stdout), url)?;
    if !printed.filepath.is_file() {
        return Err("yt-dlp finished but the file is missing".into());
    }
    Ok(printed)
}

/// yt-dlp's last non-empty stderr line, minus its `ERROR:` prefix.
fn last_line(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(|l| l.trim_start_matches("ERROR:").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_are_native_audio_no_reencode_no_android() {
        let a: Vec<String> = args(Path::new("C:\\p"), "https://x/y")
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(a.windows(2).any(|w| w == ["-f", "bestaudio/best"]));
        assert!(a.contains(&"--no-playlist".to_string()));
        assert!(!a.iter().any(|s| s == "-x" || s.contains("android")));
        assert_eq!(a.last().unwrap(), "https://x/y");
        assert!(a.contains(&"C:\\p\\%(id)s.%(ext)s".to_string()));
    }

    #[test]
    fn error_line() {
        assert_eq!(
            last_line("WARNING: x\nERROR: [youtube] abc: Video unavailable\n\n").as_deref(),
            Some("[youtube] abc: Video unavailable")
        );
        assert_eq!(last_line("  \n"), None);
    }

    /// Needs the network and yt-dlp: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn failed_download_leaves_no_file_and_reports_the_error() {
        let root = std::env::temp_dir().join(format!("musica-dl-fail-{}", std::process::id()));
        let r = run(&root, "https://www.youtube.com/watch?v=aaaaaaaaaaa");
        let left = std::fs::read_dir(library::partial_dir(&root)).map(|d| d.count());
        let _ = std::fs::remove_dir_all(&root);
        assert!(r.is_err(), "{r:?}");
        assert_eq!(left.unwrap_or(0), 0);
    }
}
