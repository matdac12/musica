//! `musica.exe [toggle | quit | --hidden]`: hand the request to the running
//! resident, or become the resident. Same shape as tasca's launcher.

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LPARAM, WPARAM};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetShellWindow, GetWindowThreadProcessId, PostMessageW,
    WM_APP,
};

use crate::log::log;

/// Window class and mutex of the resident. Debug builds only:
/// `MUSICA_INSTANCE=<name>` gives them a suffix, so a test build can run next
/// to the installed one without touching it. Release builds never read it.
pub fn class() -> PCWSTR {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    PCWSTR(NAME.get_or_init(|| named("Musica.Popup")).as_ptr())
}

pub fn mutex() -> PCWSTR {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    PCWSTR(
        NAME.get_or_init(|| named("Local\\Musica.Resident"))
            .as_ptr(),
    )
}

fn named(base: &str) -> Vec<u16> {
    #[cfg(debug_assertions)]
    if let Ok(suffix) = std::env::var("MUSICA_INSTANCE") {
        return format!("{base}.{suffix}")
            .encode_utf16()
            .chain([0])
            .collect();
    }
    base.encode_utf16().chain([0]).collect()
}
pub const WM_TOGGLE: u32 = WM_APP + 1;
pub const WM_SHOW: u32 = WM_APP + 2;
pub const WM_QUIT_RESIDENT: u32 = WM_APP + 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Toggle,
    Quit,
    /// `--hidden`: become the resident, tray only (Startup shortcut).
    Hidden,
    Show,
}

pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Command {
    for a in args {
        match a.as_str() {
            "toggle" => return Command::Toggle,
            "quit" => return Command::Quit,
            "--hidden" => return Command::Hidden,
            _ => {}
        }
    }
    Command::Show
}

pub fn run(command: Command) -> i32 {
    if let Ok(hwnd) = unsafe { FindWindowW(class(), PCWSTR::null()) } {
        let msg = match command {
            Command::Toggle => WM_TOGGLE,
            Command::Show => WM_SHOW,
            Command::Quit => WM_QUIT_RESIDENT,
            Command::Hidden => return 0,
        };
        // We were started by the hotkey, so we may hand foreground rights on:
        // without this the popup would appear without keyboard focus.
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let _ = AllowSetForegroundWindow(pid);
        }
        return match unsafe { PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0)) } {
            Ok(()) => 0,
            Err(e) => {
                log(format!("PostMessageW failed: {e}"));
                1
            }
        };
    }
    if command == Command::Quit {
        return 0;
    }
    // Hyperkey's AutoHotkey script runs elevated; a resident started from it
    // would be too. Relaunch unelevated through the shell, like tasca does.
    if is_elevated() && !unsafe { GetShellWindow() }.is_invalid() {
        return relaunch_through_shell();
    }
    crate::ui::run(command != Command::Hidden)
}

fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

fn relaunch_through_shell() -> i32 {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let Ok(exe) = std::env::current_exe() else {
        return 1;
    };
    match std::process::Command::new(format!("{windir}\\explorer.exe"))
        .arg(&exe)
        .spawn()
    {
        Ok(_) => {
            log("elevated: relaunched unelevated through explorer.exe");
            0
        }
        Err(e) => {
            log(format!("elevated: explorer.exe relaunch failed: {e}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args() {
        let p = |a: &[&str]| parse_args(a.iter().map(|s| s.to_string()));
        assert_eq!(p(&[]), Command::Show);
        assert_eq!(p(&["toggle"]), Command::Toggle);
        assert_eq!(p(&["quit"]), Command::Quit);
        assert_eq!(p(&["--hidden"]), Command::Hidden);
        assert_eq!(p(&["--bogus", "toggle"]), Command::Toggle);
    }
}
