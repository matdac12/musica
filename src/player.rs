//! Playback: one long-lived, windowless `mpv --idle` child driven over its JSON
//! IPC named pipe. mpv resolves YouTube URLs itself through yt-dlp. The child
//! sits in a kill-on-close job object, so it never outlives musica.

use serde_json::{json, Value};
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::AsRawHandle;
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Pipes::PeekNamedPipe;

use crate::log::log;
use crate::tools;

#[derive(Debug)]
pub enum Event {
    Title(String),
    Paused(bool),
    Idle(bool),
    /// Seconds; `None` while unknown (and for most live streams).
    Duration(Option<f64>),
    /// Answer to `query_pos`.
    Pos(f64),
    /// The track played to its end.
    Eof,
    Error(String),
    Exited,
}

const POS_REQUEST: i64 = 10;

pub type Notify = Box<dyn Fn(Event) + Send + Sync + 'static>;

pub struct Player {
    child: Option<Child>,
    tx: Option<Sender<String>>,
    job: HANDLE,
    notify: std::sync::Arc<Notify>,
    /// App level 0..=100, not mpv's linear volume. Kept here so it can be
    /// re-applied whenever mpv (re)starts.
    level: i32,
}

impl Player {
    pub fn new(notify: Notify, level: i32) -> Self {
        let job = unsafe { make_job() }.unwrap_or_default();
        Self {
            child: None,
            tx: None,
            job,
            notify: std::sync::Arc::new(notify),
            level: level.clamp(0, 100),
        }
    }

    pub fn play(&mut self, url: &str) -> Result<(), String> {
        self.ensure_running()?;
        self.send(json!({ "command": ["loadfile", url, "replace"] }));
        self.send(json!({ "command": ["set_property", "pause", false] }));
        Ok(())
    }

    pub fn toggle_pause(&mut self) {
        self.send(json!({ "command": ["cycle", "pause"] }));
    }

    pub fn stop(&mut self) {
        self.send(json!({ "command": ["stop"] }));
    }

    pub fn restart_track(&mut self) {
        self.send(json!({ "command": ["seek", 0, "absolute"] }));
    }

    /// Set the app level (0..=100) and send the resulting absolute mpv volume.
    /// Absolute, not `add`: the app level is the source of truth, and mpv's
    /// own linear steps are inaudible near the top.
    pub fn set_level(&mut self, level: i32) {
        self.level = level.clamp(0, 100);
        self.send(volume_command(self.level));
    }

    /// Ask for `time-pos`; the answer arrives as `Event::Pos`. Polled by the UI
    /// while the player is on screen, rather than observed: an observed
    /// time-pos would stream dozens of events a second all the time.
    pub fn query_pos(&mut self) {
        self.send(json!({ "command": ["get_property", "time-pos"], "request_id": POS_REQUEST }));
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.as_mut().map(|c| c.try_wait()), Some(Ok(None)))
    }

    fn send(&self, cmd: Value) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd.to_string());
        }
    }

    fn ensure_running(&mut self) -> Result<(), String> {
        if self.is_running() && self.tx.is_some() {
            return Ok(());
        }
        self.shutdown();
        let mpv = tools::find("mpv.exe")
            .ok_or("mpv not found (winget install mpv-player.mpv-CI.MSVC)")?;
        let pipe = format!(r"\\.\pipe\musica-mpv-{}", std::process::id());
        let mut cmd = tools::command(&mpv);
        cmd.args([
            "--idle=yes",
            "--no-config",
            "--no-terminal",
            "--no-video",
            "--force-window=no",
            "--audio-display=no",
            "--load-stats-overlay=no",
            "--load-osd-console=no",
            "--load-auto-profiles=no",
            "--load-select=no",
            "--load-positioning=no",
            "--load-commands=no",
            "--load-context-menu=no",
            "--load-console=no",
            "--osc=no",
            "--volume-max=100",
            "--ytdl-format=bestaudio/best",
            // Keep the network buffer small: memory is the point of this app.
            "--demuxer-max-bytes=4MiB",
            "--demuxer-max-back-bytes=512KiB",
            "--demuxer-readahead-secs=10",
            "--cache-secs=20",
        ])
        .arg(format!("--input-ipc-server={pipe}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        if let Some(ytdlp) = tools::find("yt-dlp.exe") {
            cmd.arg(format!(
                "--script-opts=ytdl_hook-ytdl_path={}",
                ytdlp.display()
            ));
        }
        // Debug builds only: `MUSICA_TEST_AO=null` plays silently, for tests.
        #[cfg(debug_assertions)]
        if let Ok(ao) = std::env::var("MUSICA_TEST_AO") {
            cmd.arg(format!("--ao={ao}"));
        }
        let child = cmd
            .spawn()
            .map_err(|e| format!("mpv failed to start: {e}"))?;
        if !self.job.is_invalid() {
            let _ = unsafe { AssignProcessToJobObject(self.job, HANDLE(child.as_raw_handle())) };
        }
        log(format!(
            "mpv started: pid {} ({})",
            child.id(),
            mpv.display()
        ));
        self.child = Some(child);

        let conn = connect(&pipe, Duration::from_secs(5)).ok_or("mpv IPC pipe did not come up")?;
        let (tx, rx) = mpsc::channel::<String>();
        for (id, prop) in [
            (1, "media-title"),
            (2, "pause"),
            (3, "idle-active"),
            (4, "duration"),
        ] {
            let _ = tx.send(json!({ "command": ["observe_property", id, prop] }).to_string());
        }
        // Re-apply the level on every (re)start: a new mpv starts at 100%.
        let _ = tx.send(volume_command(self.level).to_string());
        let notify = self.notify.clone();
        std::thread::spawn(move || io_loop(conn, rx, &**notify));
        self.tx = Some(tx);
        Ok(())
    }

    fn shutdown(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(json!({ "command": ["quit"] }).to_string());
        }
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_millis(500);
            while Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shutdown();
        if !self.job.is_invalid() {
            let _ = unsafe { CloseHandle(self.job) };
        }
    }
}

unsafe fn make_job() -> Option<HANDLE> {
    unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .ok()?;
        Some(job)
    }
}

/// App level 0..=100 to mpv's linear volume: `100·(level/100)³`.
///
/// Cubic, so equal steps feel equal: the quiet end gets usable range and the
/// top stays fine-grained (mpv's own linear ±5 was inaudible up there). mpv
/// keeps `--volume-max=100`, so level 100 is 0 dBFS and never above it.
pub fn volume_for_level(level: i32) -> f64 {
    let x = level.clamp(0, 100) as f64 / 100.0;
    100.0 * x * x * x
}

/// Gain relative to full scale, for the toast: 60·log10(level/100).
/// `None` at level 0 (silence).
pub fn db_for_level(level: i32) -> Option<f64> {
    let volume = volume_for_level(level);
    (volume > 0.0).then(|| 20.0 * (volume / 100.0).log10())
}

fn volume_command(level: i32) -> Value {
    json!({ "command": ["set_property", "volume", volume_for_level(level)] })
}

fn connect(pipe: &str, timeout: Duration) -> Option<File> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(f) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe)
        {
            return Some(f);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One thread owns the pipe. A synchronous pipe handle serializes reads and
/// writes, so a blocking read would stall commands: poll with PeekNamedPipe
/// instead and only read what is already there.
fn io_loop(mut conn: File, rx: Receiver<String>, notify: &(dyn Fn(Event) + Send + Sync)) {
    let mut pending = Vec::<u8>::new();
    let mut buf = [0u8; 8192];
    loop {
        match rx.recv_timeout(Duration::from_millis(60)) {
            Ok(line) => {
                if conn
                    .write_all(line.as_bytes())
                    .and_then(|_| conn.write_all(b"\n"))
                    .is_err()
                {
                    break;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let mut avail = 0u32;
        let ok = unsafe {
            PeekNamedPipe(
                HANDLE(conn.as_raw_handle()),
                None,
                0,
                None,
                Some(&mut avail),
                None,
            )
        }
        .is_ok();
        if !ok {
            break;
        }
        while avail > 0 {
            let want = (avail as usize).min(buf.len());
            let Ok(n) = conn.read(&mut buf[..want]) else {
                break;
            };
            if n == 0 {
                break;
            }
            avail -= n as u32;
            pending.extend_from_slice(&buf[..n]);
        }
        while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=pos).collect();
            if let Some(ev) = parse_event(&line) {
                notify(ev);
            }
        }
    }
    log("mpv IPC closed");
    notify(Event::Exited);
}

fn parse_event(line: &[u8]) -> Option<Event> {
    let v: Value = serde_json::from_slice(line).ok()?;
    if v["request_id"] == POS_REQUEST {
        return v["data"].as_f64().map(Event::Pos);
    }
    match v["event"].as_str()? {
        "property-change" => match v["name"].as_str()? {
            "media-title" => Some(Event::Title(v["data"].as_str().unwrap_or("").to_string())),
            "pause" => Some(Event::Paused(v["data"].as_bool().unwrap_or(false))),
            "idle-active" => Some(Event::Idle(v["data"].as_bool().unwrap_or(false))),
            "duration" => Some(Event::Duration(v["data"].as_f64())),
            _ => None,
        },
        "end-file" if v["reason"] == "eof" => Some(Event::Eof),
        "end-file" if v["reason"] == "error" => {
            let msg = v["file_error"]
                .as_str()
                .unwrap_or("playback failed")
                .to_string();
            log(format!("end-file error: {msg}"));
            Some(Event::Error(msg))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_curve() {
        assert_eq!(volume_for_level(0), 0.0);
        assert!((volume_for_level(50) - 12.5).abs() < 1e-9);
        assert!((volume_for_level(85) - 61.4125).abs() < 1e-9);
        assert_eq!(volume_for_level(100), 100.0);
        for level in 0..100 {
            assert!(volume_for_level(level) < volume_for_level(level + 1));
        }
    }

    #[test]
    fn level_clamps_to_0_100() {
        assert_eq!(volume_for_level(-1), 0.0);
        assert_eq!(volume_for_level(-100), 0.0);
        assert_eq!(volume_for_level(101), 100.0);
        assert_eq!(volume_for_level(9999), 100.0);
    }

    #[test]
    fn db_is_fine_at_the_top_and_coarse_at_the_bottom() {
        assert!(db_for_level(0).is_none());
        assert!(db_for_level(100).unwrap().abs() < 1e-9);
        // 60·log10(0.5) = -18.06 dB at half level.
        assert!((db_for_level(50).unwrap() + 18.0618).abs() < 1e-4);
        // Fine where it used to be inaudible, coarse where the range is needed.
        let top = db_for_level(100).unwrap() - db_for_level(98).unwrap();
        let bottom = db_for_level(20).unwrap() - db_for_level(18).unwrap();
        assert!(top < 0.6, "top step {top}");
        assert!(bottom > 2.0, "bottom step {bottom}");
        // Twenty ±2 presses from full is a clearly audible -13.3 dB.
        assert!((db_for_level(60).unwrap() + 13.311).abs() < 1e-3);
    }

    #[test]
    fn events() {
        let ev = |s: &str| parse_event(s.as_bytes());
        assert!(
            matches!(ev(r#"{"event":"property-change","id":1,"name":"media-title","data":"Lofi"}"#), Some(Event::Title(t)) if t == "Lofi")
        );
        assert!(matches!(
            ev(r#"{"event":"property-change","id":2,"name":"pause","data":true}"#),
            Some(Event::Paused(true))
        ));
        assert!(matches!(
            ev(r#"{"event":"end-file","reason":"error","file_error":"loading failed"}"#),
            Some(Event::Error(_))
        ));
        assert!(ev(r#"{"event":"end-file","reason":"stop"}"#).is_none());
        assert!(matches!(
            ev(r#"{"event":"end-file","reason":"eof"}"#),
            Some(Event::Eof)
        ));
        assert!(
            matches!(ev(r#"{"data":12.5,"request_id":10,"error":"success"}"#), Some(Event::Pos(p)) if p == 12.5)
        );
        assert!(matches!(
            ev(r#"{"event":"property-change","id":5,"name":"duration"}"#),
            Some(Event::Duration(None))
        ));
        assert!(ev(r#"{"request_id":0,"error":"success"}"#).is_none());
    }
}
