//! Best-effort append-only log at `%LOCALAPPDATA%\Musica\musica.log`.

use std::io::Write;
use std::path::PathBuf;

pub fn dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Musica")
}

pub fn log(msg: impl AsRef<str>) {
    if cfg!(test) {
        return;
    }
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("musica.log"))
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{secs} [{}] {}", std::process::id(), msg.as_ref());
    }
}
