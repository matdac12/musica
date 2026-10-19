//! Finding `mpv.exe` and `yt-dlp.exe`, and spawning them without a console.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Look on PATH, then in winget's Links and Packages folders. The resident may
/// have been started before winget changed PATH, so PATH alone is not enough.
pub fn find(exe: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(exe);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let winget = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Microsoft\\WinGet");
    let link = winget.join("Links").join(exe);
    if link.is_file() {
        return Some(link);
    }
    for entry in std::fs::read_dir(winget.join("Packages")).ok()?.flatten() {
        let p = entry.path().join(exe);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub fn command(exe: &PathBuf) -> Command {
    let mut cmd = Command::new(exe);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}
