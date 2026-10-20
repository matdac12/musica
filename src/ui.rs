//! The resident: tray icon plus one popup with two views. Search is a search
//! box over owner-drawn results; Player (shown while something plays) is fully
//! painted here: the current track, progress, controls and what plays next.
//! Plain Win32, one UI thread.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_SELECTED};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, GetAsyncKeyState, GetFocus, SetFocus, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT, VK_S,
    VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::download::Downloader;
use crate::launch::{class, mutex, WM_QUIT_RESIDENT, WM_SHOW, WM_TOGGLE};
use crate::library::{self, Entry, Printed};
use crate::log::log;
use crate::player::{self, Event, Player};
use crate::search::{self, fmt_secs, Track};
use crate::settings::{self, Settings};

const WM_TRAY: u32 = WM_APP + 10;
const WM_RESULTS: u32 = WM_APP + 11;
const WM_PLAYER: u32 = WM_APP + 12;
const WM_LIBRARY: u32 = WM_APP + 13;
const TIMER_POS: usize = 1;

const ID_EDIT: usize = 100;
const ID_LIST: usize = 101;
const CMD_PAUSE: usize = 1;
const CMD_STOP: usize = 2;
const CMD_SHOW: usize = 3;
const CMD_QUIT: usize = 4;
const CMD_NEXT: usize = 5;

// Layout in 96-DPI pixels.
const W: i32 = 640;
const PAD: i32 = 12;
const SEARCH_H: i32 = 44;
const ROW_H: i32 = 46;
const ROWS: i32 = search::LIMIT as i32;
const STATUS_H: i32 = 34;
const SEARCH_VIEW_H: i32 = PAD + SEARCH_H + 8 + ROWS * ROW_H + 4 + STATUS_H + 4;

// Player view.
const P: i32 = 18;
const BAR_Y: i32 = 104;
const CTRL_CY: i32 = 152;
const UPNEXT_Y: i32 = 194;
const UPNEXT_ROWS: i32 = 4;
const ROWS_Y: i32 = UPNEXT_Y + 22;
const FOOT_Y: i32 = ROWS_Y + UPNEXT_ROWS * ROW_H + 6;
const PLAYER_VIEW_H: i32 = FOOT_Y + 36;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}
const BG: COLORREF = rgb(0x0f, 0x14, 0x1a);
const PANEL: COLORREF = rgb(0x19, 0x20, 0x2a);
const SEL: COLORREF = rgb(0x22, 0x2b, 0x37);
const TRACK: COLORREF = rgb(0x26, 0x30, 0x3c);
const RULE: COLORREF = rgb(0x1c, 0x24, 0x2e);
const TEXT: COLORREF = rgb(0xe9, 0xed, 0xf2);
const DIM: COLORREF = rgb(0x8b, 0x96, 0xa5);
const ACCENT: COLORREF = rgb(0xff, 0x9e, 0x0b);
const LIVE: COLORREF = rgb(0xff, 0x6b, 0x6b);
const ERR: COLORREF = rgb(0xff, 0x6b, 0x6b);
const BORDER: COLORREF = rgb(0x2a, 0x33, 0x40);

// Segoe MDL2 Assets glyphs.
const G_PREV: &str = "\u{E892}";
const G_NEXT: &str = "\u{E893}";
const G_PLAY: &str = "\u{E768}";
const G_PAUSE: &str = "\u{E769}";
const G_HEART: &str = "\u{E006}";
const G_HEART_FILL: &str = "\u{E00B}";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Search,
    Player,
}

/// What the Search view's list shows: the saved library (filtered by the box)
/// or the last YouTube search.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Library,
    Results,
}

struct Fonts {
    search: HFONT,
    big: HFONT,
    title: HFONT,
    meta: HFONT,
    icons: HFONT,
}

/// A short message shown in the status line / player footer.
struct Toast {
    text: String,
    error: bool,
    until: Instant,
}

struct App {
    hwnd: HWND,
    edit: HWND,
    list: HWND,
    dpi: u32,
    fonts: Fonts,
    brushes: [HBRUSH; 6],
    icon: HICON,
    taskbar_created: u32,
    view: View,
    // Search.
    source: Source,
    results: Vec<Track>,
    /// Library positions shown, newest first, filtered by `query`.
    view_idx: Vec<usize>,
    /// The box text (trimmed) the library view was last filtered with.
    query: String,
    seq: u64,
    searching: bool,
    error: Option<String>,
    // Playback. The queue is the result list a track was played from.
    queue: Vec<Track>,
    qi: usize,
    playing: bool,
    loading: bool,
    now_title: String,
    paused: bool,
    pos: f64,
    mpv_duration: Option<f64>,
    /// The playing track comes from the library file: mpv's title is the file
    /// name then, so it is ignored, and the length is the index's.
    local: bool,
    local_duration: Option<f64>,
    /// File name of the last saved file played: a late `media-title` for it can
    /// still arrive after the next (streamed) track has started.
    local_name: String,
    /// App volume level 0..=100 (not mpv's volume); see `player::volume_for_level`.
    level: i32,
    /// What is on disk, so border presses don't rewrite the file.
    saved_level: i32,
    volume_flash: Option<Instant>,
    player: Player,
    // Library: the index lives here, loaded at startup.
    lib_root: std::path::PathBuf,
    lib: Vec<Entry>,
    /// URLs queued or downloading, for the dim heart.
    saving: Vec<String>,
    downloader: Option<Downloader>,
    toast: Option<Toast>,
}

// Indexes into `App::brushes`.
const B_BG: usize = 0;
const B_PANEL: usize = 1;
const B_SEL: usize = 2;
const B_ACCENT: usize = 3;
const B_TRACK: usize = 4;
const B_RULE: usize = 5;

impl App {
    fn current(&self) -> Option<&Track> {
        if self.playing {
            self.queue.get(self.qi)
        } else {
            None
        }
    }

    /// Track length: the index's for a saved file; else the search result's
    /// when it has one (no duration there means a live stream); else what mpv
    /// reports (pasted links).
    fn duration(&self) -> Option<f64> {
        let t = self.current()?;
        if let Some(d) = self.local_duration {
            Some(d)
        } else if t.from_search {
            t.duration.map(|d| d as f64)
        } else {
            self.mpv_duration
        }
    }

    /// Rows in the Search list.
    fn rows(&self) -> usize {
        match self.source {
            Source::Library => self.view_idx.len(),
            Source::Results => self.results.len(),
        }
    }

    /// The list as shown, as a queue.
    fn shown(&self) -> Vec<Track> {
        match self.source {
            Source::Library => self
                .view_idx
                .iter()
                .filter_map(|&i| self.lib.get(i))
                .map(Entry::track)
                .collect(),
            Source::Results => self.results.clone(),
        }
    }

    fn s(&self, v: i32) -> i32 {
        scale(v, self.dpi.max(96))
    }

    fn show_toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            error,
            until: Instant::now() + Duration::from_millis(4000),
        });
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` on the app state. Returns None on re-entry instead of panicking.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(v: i32, dpi: u32) -> i32 {
    v * dpi as i32 / 96
}

pub fn run(show: bool) -> i32 {
    match unsafe { run_inner(show) } {
        Ok(code) => code,
        Err(e) => {
            log(format!("resident failed: {e:?}"));
            1
        }
    }
}

unsafe fn run_inner(show: bool) -> Result<i32> {
    unsafe {
        let _mutex = CreateMutexW(None, true, mutex())?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(0);
        }
        log("resident starting");
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class(),
            w!("musica"),
            WS_POPUP | WS_CLIPCHILDREN,
            0,
            0,
            W,
            400,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let corner: u32 = 2; // DWMWCP_ROUND
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as _,
            4,
        );
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &BORDER as *const _ as _, 4);

        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            w!(""),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x80), // ES_AUTOHSCROLL
            0,
            0,
            10,
            10,
            Some(hwnd),
            Some(HMENU(ID_EDIT as _)),
            Some(instance.into()),
            None,
        )?;
        let cue = w!("Search YouTube, or paste a link");
        SendMessageW(
            edit,
            0x1501,
            Some(WPARAM(1)),
            Some(LPARAM(cue.as_ptr() as isize)),
        );
        let list = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("LISTBOX"),
            w!(""),
            // LBS_NOTIFY | LBS_OWNERDRAWFIXED | LBS_NOINTEGRALHEIGHT | LBS_NODATA
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0x1 | 0x10 | 0x100 | 0x2000),
            0,
            0,
            10,
            10,
            Some(hwnd),
            Some(HMENU(ID_LIST as _)),
            Some(instance.into()),
            None,
        )?;

        let hwnd_raw = hwnd.0 as isize;
        let settings = settings::load();
        let lib_root = library::root();
        let lib = library::load(&lib_root);
        library::sweep(&lib_root, &lib);
        log(format!("library: {} tracks", lib.len()));
        let player = Player::new(
            Box::new(move |ev: Event| {
                let ptr = Box::into_raw(Box::new(ev));
                if PostMessageW(
                    Some(HWND(hwnd_raw as _)),
                    WM_PLAYER,
                    WPARAM(0),
                    LPARAM(ptr as isize),
                )
                .is_err()
                {
                    drop(Box::from_raw(ptr));
                }
            }),
            settings.level,
        );
        let app = App {
            hwnd,
            edit,
            list,
            dpi: 0,
            fonts: make_fonts(96),
            brushes: [BG, PANEL, SEL, ACCENT, TRACK, RULE].map(|c| CreateSolidBrush(c)),
            icon: make_icon(),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            view: View::Search,
            source: Source::Library,
            results: Vec::new(),
            view_idx: Vec::new(),
            query: String::new(),
            seq: 0,
            searching: false,
            error: None,
            queue: Vec::new(),
            qi: 0,
            playing: false,
            loading: false,
            now_title: String::new(),
            paused: false,
            pos: 0.0,
            mpv_duration: None,
            local: false,
            local_duration: None,
            local_name: String::new(),
            level: settings.level,
            saved_level: settings.level,
            volume_flash: None,
            player,
            lib_root,
            lib,
            saving: Vec::new(),
            downloader: None,
            toast: None,
        };
        APP.with(|a| *a.borrow_mut() = Some(app));
        sync_list();
        tray(NIM_ADD);
        if show {
            show_popup();
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if pretranslate(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        log("resident exiting");
        APP.with(|a| a.borrow_mut().take());
        Ok(0)
    }
}

fn make_fonts(dpi: u32) -> Fonts {
    let font = |px: i32, weight: i32, face: PCWSTR| unsafe {
        CreateFontW(
            -scale(px, dpi),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            face,
        )
    };
    let ui = w!("Segoe UI");
    Fonts {
        search: font(18, 400, ui),
        big: font(20, 600, ui),
        title: font(15, 600, ui),
        meta: font(13, 400, ui),
        icons: font(16, 400, w!("Segoe MDL2 Assets")),
    }
}

/// Re-scale fonts and children for the monitor the popup is about to open on.
fn apply_dpi(dpi: u32) {
    let Some((hwnd, edit, list, old)) = with(|a| {
        if a.dpi == dpi {
            return None;
        }
        let old = std::mem::replace(&mut a.fonts, make_fonts(dpi));
        a.dpi = dpi;
        Some((a.hwnd, a.edit, a.list, old))
    })
    .flatten() else {
        return;
    };
    unsafe {
        let font = with(|a| a.fonts.search).unwrap();
        SendMessageW(
            edit,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        SendMessageW(
            list,
            LB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(scale(ROW_H, dpi) as isize)),
        );
        for f in [old.search, old.big, old.title, old.meta, old.icons] {
            let _ = DeleteObject(f.into());
        }
        let s = |v| scale(v, dpi);
        let edit_h = s(26);
        let _ = MoveWindow(
            edit,
            s(PAD + 14),
            s(PAD) + (s(SEARCH_H) - edit_h) / 2,
            s(W - 2 * PAD - 28),
            edit_h,
            true,
        );
        let _ = MoveWindow(
            list,
            s(PAD),
            s(PAD + SEARCH_H + 8),
            s(W - 2 * PAD),
            s(ROWS * ROW_H),
            true,
        );
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn view_height(view: View) -> i32 {
    match view {
        View::Search => SEARCH_VIEW_H,
        View::Player => PLAYER_VIEW_H,
    }
}

fn show_popup() {
    let Some((hwnd, playing)) = with(|a| (a.hwnd, a.playing)) else {
        return;
    };
    let view = if playing { View::Player } else { View::Search };
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        apply_dpi(dx);
        let work = mi.rcWork;
        let (w, h) = (scale(W, dx), scale(view_height(view), dx));
        let x = work.left + (work.right - work.left - w) / 2;
        let y = work.top + (work.bottom - work.top) / 5;
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        set_view(view, false);
        let _ = ShowWindow(hwnd, SW_SHOW);
        focus_popup(hwnd);
        set_view(view, false); // focus the right child now that we are foreground
        let _ = SetTimer(Some(hwnd), TIMER_POS, 500, None);
        with(|a| a.player.query_pos());
    }
}

/// Switch views: show or hide the search controls, resize, move focus.
/// `clear` empties the search box (typing from the player starts a new query).
fn set_view(view: View, clear: bool) {
    let Some((hwnd, edit, list, dpi)) = with(|a| {
        a.view = view;
        (a.hwnd, a.edit, a.list, a.dpi.max(96))
    }) else {
        return;
    };
    unsafe {
        let show = if view == View::Search {
            SW_SHOW
        } else {
            SW_HIDE
        };
        let _ = ShowWindow(edit, show);
        let _ = ShowWindow(list, show);
        let mut rc = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rc);
        let h = scale(view_height(view), dpi);
        if rc.bottom - rc.top != h {
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                rc.right - rc.left,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        match view {
            View::Player => {
                let _ = SetFocus(Some(hwnd));
            }
            View::Search => {
                if clear {
                    let _ = SetWindowTextW(edit, w!(""));
                }
                let _ = SetFocus(Some(edit));
                SendMessageW(edit, 0xB1, Some(WPARAM(0)), Some(LPARAM(-1))); // EM_SETSEL all
            }
        }
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

fn focus_popup(hwnd: HWND) {
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return;
        }
        // Foreground lock: a tapped Alt counts as fresh input and lifts it.
        keybd_event(VK_MENU.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn hide_popup() {
    if let Some(hwnd) = with(|a| a.hwnd) {
        unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_POS);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn toggle_popup() {
    let Some(hwnd) = with(|a| a.hwnd) else { return };
    let visible = unsafe { IsWindowVisible(hwnd).as_bool() && GetForegroundWindow() == hwnd };
    if visible {
        hide_popup();
    } else {
        show_popup();
    }
}

fn repaint() {
    if let Some(hwnd) = with(|a| a.hwnd) {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

fn repaint_list() {
    if let Some(list) = with(|a| a.list) {
        unsafe {
            let _ = InvalidateRect(Some(list), None, true);
        }
    }
}

/// A modifier as held *now*. `GetAsyncKeyState`, not `GetKeyState`, so the
/// state does not depend on which control's queue holds the key message.
fn held(key: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(key.0 as i32) < 0 }
}

/// Plain arrows step 2 levels; Shift makes it 10.
fn step() -> i32 {
    if held(VK_SHIFT) {
        10
    } else {
        2
    }
}

/// Step the app level in either view, show the toast and save at once: the
/// level is the only thing standing between a restart and full volume.
fn change_level(delta: i32) {
    let save = with(|a| {
        a.level = (a.level + delta).clamp(0, 100);
        a.player.set_level(a.level);
        a.volume_flash = Some(Instant::now() + Duration::from_millis(1500));
        if a.level != a.saved_level {
            a.saved_level = a.level;
            Some(a.level)
        } else {
            None
        }
    })
    .flatten();
    if let Some(level) = save {
        settings::save(&Settings { level });
    }
    repaint();
}

fn volume_text(level: i32) -> String {
    match player::db_for_level(level) {
        Some(db) => format!("Volume {level}%  {db:.1} dB"),
        None => format!("Volume {level}%  silent"),
    }
}

/// Footer-size toast: the level always, the dB only when it fits.
fn volume_text_fitted(level: i32, room: i32, hdc: HDC, font: HFONT) -> String {
    let full = volume_text(level);
    if room > 0 && text_width(hdc, &full, font) <= room {
        full
    } else {
        format!("Volume {level}%")
    }
}

fn volume_toast(a: &App) -> Option<String> {
    a.volume_flash
        .filter(|until| Instant::now() < *until)
        .map(|_| volume_text(a.level))
}

fn text_width(hdc: HDC, s: &str, font: HFONT) -> i32 {
    unsafe {
        let old = SelectObject(hdc, font.into());
        let mut size = SIZE::default();
        let units: Vec<u16> = s.encode_utf16().collect();
        let _ = GetTextExtentPoint32W(hdc, &units, &mut size);
        SelectObject(hdc, old);
        size.cx
    }
}

/// Keys handled before the controls see them. Returns true when consumed.
fn pretranslate(msg: &MSG) -> bool {
    let Some((hwnd, edit, list, view, has_rows, playing)) =
        with(|a| (a.hwnd, a.edit, a.list, a.view, a.rows() > 0, a.playing))
    else {
        return false;
    };
    unsafe {
        if view == View::Player && msg.hwnd == hwnd {
            if msg.message == WM_CHAR && msg.wParam.0 >= 0x21 {
                // Typing on the player starts a new search.
                set_view(View::Search, true);
                SendMessageW(edit, WM_CHAR, Some(msg.wParam), Some(msg.lParam));
                return true;
            }
            if msg.message != WM_KEYDOWN {
                return false;
            }
            let key = msg.wParam.0 as u16;
            match key {
                k if k == VK_SPACE.0 => with(|a| a.player.toggle_pause()).is_some(),
                k if k == VK_S.0 && held(VK_CONTROL) => {
                    save_current();
                    true
                }
                k if k == VK_RIGHT.0 => {
                    next();
                    true
                }
                k if k == VK_LEFT.0 => {
                    prev();
                    true
                }
                k if k == VK_UP.0 => {
                    change_level(step());
                    true
                }
                k if k == VK_DOWN.0 => {
                    change_level(-step());
                    true
                }
                k if k == VK_TAB.0 => {
                    set_view(View::Search, false);
                    true
                }
                k if k == VK_ESCAPE.0 => {
                    hide_popup();
                    true
                }
                _ => false,
            }
        } else if msg.hwnd == edit || msg.hwnd == list {
            if msg.message == WM_CHAR && msg.hwnd == list && msg.wParam.0 >= 0x21 {
                // Typing while in the results goes to the search box.
                let _ = SetFocus(Some(edit));
                SendMessageW(edit, WM_CHAR, Some(msg.wParam), Some(msg.lParam));
                return true;
            }
            if msg.message != WM_KEYDOWN {
                return false;
            }
            let key = msg.wParam.0 as u16;
            // ↑/↓ already move the result selection, so volume here is
            // Ctrl+↑/↓ (Ctrl+Shift for ±10), in the box and in the list.
            if (key == VK_UP.0 || key == VK_DOWN.0) && held(VK_CONTROL) {
                change_level(if key == VK_UP.0 { step() } else { -step() });
                return true;
            }
            match key {
                k if k == VK_ESCAPE.0 || k == VK_TAB.0 => {
                    if playing {
                        set_view(View::Player, false);
                    } else if k == VK_ESCAPE.0 {
                        hide_popup();
                    }
                    true
                }
                k if k == VK_RETURN.0 && msg.hwnd == edit => {
                    submit();
                    true
                }
                k if k == VK_RETURN.0 => {
                    let i = SendMessageW(list, LB_GETCURSEL, None, None).0;
                    if i >= 0 {
                        play_shown(i as usize, held(VK_SHIFT));
                    }
                    true
                }
                k if k == VK_DOWN.0 && msg.hwnd == edit && has_rows => {
                    let _ = SetFocus(Some(list));
                    if SendMessageW(list, LB_GETCURSEL, None, None).0 < 0 {
                        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
                    }
                    true
                }
                k if k == VK_UP.0
                    && msg.hwnd == list
                    && SendMessageW(list, LB_GETCURSEL, None, None).0 <= 0 =>
                {
                    let _ = SetFocus(Some(edit));
                    true
                }
                k if k == VK_SPACE.0 && msg.hwnd == list => {
                    with(|a| a.player.toggle_pause()).is_some()
                }
                _ => false,
            }
        } else {
            false
        }
    }
}

fn edit_text(edit: HWND) -> String {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetWindowTextW(edit, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
        .trim()
        .to_string()
}

fn submit() {
    let Some(edit) = with(|a| a.edit) else { return };
    let q = edit_text(edit);
    if q.is_empty() {
        return;
    }
    if search::is_url(&q) {
        play_queue(vec![Track::from_link(q)], 0);
        return;
    }
    let Some((hwnd, seq)) = with(|a| {
        a.seq += 1;
        a.searching = true;
        a.error = None;
        (a.hwnd, a.seq)
    }) else {
        return;
    };
    repaint();
    log(format!("search #{seq}: {q}"));
    let hwnd_raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        let r = search::run(&q);
        let ptr = Box::into_raw(Box::new((seq, r)));
        unsafe {
            if PostMessageW(
                Some(HWND(hwnd_raw as _)),
                WM_RESULTS,
                WPARAM(0),
                LPARAM(ptr as isize),
            )
            .is_err()
            {
                drop(Box::from_raw(ptr));
            }
        }
    });
}

fn on_results(seq: u64, r: std::result::Result<Vec<Track>, String>) {
    let Some(Some(())) = with(|a| {
        if seq != a.seq {
            return None;
        }
        a.searching = false;
        match r {
            Ok(v) => {
                log(format!("search #{seq}: {} results", v.len()));
                if v.is_empty() {
                    a.error = Some("No results".into());
                }
                a.source = Source::Results;
                a.results = v;
            }
            Err(e) => {
                // Offline or broken: show the saved matches instead.
                log(format!("search #{seq} failed: {e}"));
                a.error = Some("YouTube search failed · showing saved matches".into());
                a.source = Source::Library;
                a.results.clear();
            }
        }
        Some(())
    }) else {
        return;
    };
    sync_list();
    let Some((list, n)) = with(|a| (a.list, a.rows())) else {
        return;
    };
    if n > 0 {
        unsafe {
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
            if GetFocus() != list {
                let _ = SetFocus(Some(list));
            }
        }
    }
    repaint_list();
}

/// Point the list at its source: refilter the library by the box text, set the
/// row count (which clears the selection).
fn sync_list() {
    let Some(edit) = with(|a| a.edit) else { return };
    let q = edit_text(edit);
    let Some((list, n)) = with(|a| {
        if a.source == Source::Library {
            a.view_idx = library::filter(&a.lib, &q);
        }
        a.query = q;
        (a.list, a.rows())
    }) else {
        return;
    };
    unsafe {
        SendMessageW(list, LB_SETCOUNT, Some(WPARAM(n)), None);
        let _ = InvalidateRect(Some(list), None, true);
    }
    repaint();
}

/// The box changed. While the library shows, each keystroke refilters it (no
/// network). YouTube results stay until the next Enter, unless the box is
/// emptied: then the library comes back and a search still in flight is dropped.
fn on_edit_change() {
    let Some(edit) = with(|a| a.edit) else { return };
    let empty = edit_text(edit).is_empty();
    let library = with(|a| {
        if empty {
            a.source = Source::Library;
            a.results = Vec::new();
            a.error = None;
            a.seq += 1;
            a.searching = false;
        }
        a.source == Source::Library
    });
    if library == Some(true) {
        sync_list();
    } else {
        // Results stay; the status line does not use the text.
        repaint();
    }
}

/// Play row `i` of the list as shown: it becomes the queue, starting there.
/// `shuffle` (library only) puts row `i` first and the rest in random order.
fn play_shown(i: usize, shuffle: bool) {
    let Some((mut queue, library)) = with(|a| (a.shown(), a.source == Source::Library)) else {
        return;
    };
    if i >= queue.len() {
        return;
    }
    let mut qi = i;
    if shuffle && library {
        library::shuffle_first(&mut queue, i, library::time_seed());
        qi = 0;
    }
    play_queue(queue, qi);
}

fn play_queue(queue: Vec<Track>, qi: usize) {
    with(|a| {
        a.queue = queue;
        a.qi = qi;
    });
    play_current();
    let visible = with(|a| unsafe { IsWindowVisible(a.hwnd).as_bool() }).unwrap_or(false);
    if visible {
        set_view(View::Player, false);
    }
}

fn play_current() {
    let r = with(|a| {
        let t = a.queue.get(a.qi)?.clone();
        log(format!("play {} ({}/{})", t.url, a.qi + 1, a.queue.len()));
        a.error = None;
        a.playing = true;
        a.loading = true;
        a.paused = false;
        a.pos = 0.0;
        a.mpv_duration = None;
        a.now_title = t.title.clone();
        // A saved track plays from its file (no network); if the file has
        // gone, the URL.
        let local = library::local_file(&a.lib_root, &a.lib, &t)
            .map(|(e, path)| (e.title.clone(), e.duration, path));
        a.local = local.is_some();
        a.local_duration = None;
        let target = match local {
            Some((title, secs, path)) => {
                log(format!("from disk: {}", path.display()));
                a.now_title = title;
                a.local_name = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                a.local_duration = (secs > 0).then_some(secs as f64);
                path.to_string_lossy().into_owned()
            }
            None => t.url.clone(),
        };
        Some(a.player.play(&target))
    })
    .flatten();
    if let Some(Err(e)) = r {
        log(format!("play failed: {e}"));
        with(|a| {
            a.loading = false;
            a.playing = false;
            a.error = Some(e);
        });
    }
    repaint_list();
    repaint();
    tray(NIM_MODIFY);
}

/// Save the playing track: queue a download. Never a toggle.
fn save_current() {
    with(|a| {
        let Some(t) = a.current().cloned() else {
            return;
        };
        if library::is_saved(&a.lib, &t) {
            a.show_toast("Already saved", false);
        } else if a.saving.contains(&t.url) {
            // Already queued or downloading: ignored.
        } else if !library::savable(&t, a.mpv_duration) {
            a.show_toast("Live streams can't be saved", true);
        } else {
            let (hwnd_raw, root) = (a.hwnd.0 as isize, a.lib_root.clone());
            a.saving.push(t.url.clone());
            a.downloader
                .get_or_insert_with(|| {
                    Downloader::new(
                        root,
                        Box::new(move |url, r| {
                            let ptr = Box::into_raw(Box::new((url, r)));
                            unsafe {
                                if PostMessageW(
                                    Some(HWND(hwnd_raw as _)),
                                    WM_LIBRARY,
                                    WPARAM(0),
                                    LPARAM(ptr as isize),
                                )
                                .is_err()
                                {
                                    drop(Box::from_raw(ptr));
                                }
                            }
                        }),
                    )
                })
                .enqueue(t.url);
        }
    });
    repaint_list();
    repaint();
}

/// A download finished or failed. The entry exists only now, with its file in
/// place. A pasted link gets its id here, from yt-dlp's own metadata, and is
/// deduplicated by it: the new file is dropped if that id is already saved.
fn on_library(url: String, r: std::result::Result<Printed, String>) {
    with(|a| {
        a.saving.retain(|u| *u != url);
        let p = match r {
            Ok(p) => p,
            Err(e) => return a.show_toast(format!("Couldn't save: {e}"), true),
        };
        if a.lib.iter().any(|e| e.id == p.id) {
            let _ = std::fs::remove_file(&p.filepath);
            return a.show_toast("Already saved", false);
        }
        let entry = match library::finalize(&a.lib_root, &p, library::now_iso()) {
            Ok(entry) => entry,
            Err(e) => {
                log(format!("save {} failed: {e}", p.id));
                let _ = std::fs::remove_file(&p.filepath);
                return a.show_toast(format!("Couldn't save: {e}"), true);
            }
        };
        a.lib.push(entry);
        if let Err(e) = library::save(&a.lib_root, &a.lib) {
            // No entry without a durable index: roll back, drop the file.
            log(format!("could not write the index: {e}"));
            if let Some(entry) = a.lib.pop() {
                let _ = std::fs::remove_file(library::audio_dir(&a.lib_root).join(entry.file));
            }
            return a.show_toast(format!("Couldn't save: {e}"), true);
        }
        log(format!(
            "saved {} ({} bytes)",
            p.id,
            a.lib.last().map_or(0, |e| e.bytes)
        ));
        for t in a.queue.iter_mut().filter(|t| t.url == url) {
            t.id.get_or_insert_with(|| p.id.clone());
        }
        a.show_toast("Saved", false);
    });
    // A new entry shifts the library rows; YouTube results are untouched.
    if with(|a| a.source == Source::Library) == Some(true) {
        sync_list();
    } else {
        repaint_list();
        repaint();
    }
}

fn next() {
    let more = with(|a| {
        if a.playing && a.qi + 1 < a.queue.len() {
            a.qi += 1;
            true
        } else {
            false
        }
    });
    if more == Some(true) {
        play_current();
    }
}

/// Like Spotify: past the first 3 seconds, ← restarts the track.
fn prev() {
    let back = with(|a| {
        if !a.playing {
            return None;
        }
        if a.pos > 3.0
            || a.qi == 0
            || a.current()
                .is_some_and(|t| t.duration.is_none() && t.from_search)
        {
            a.player.restart_track();
            a.pos = 0.0;
            Some(false)
        } else {
            a.qi -= 1;
            Some(true)
        }
    })
    .flatten();
    match back {
        Some(true) => play_current(),
        Some(false) => repaint(),
        None => {}
    }
}

fn on_player(ev: Event) {
    let mut advance = false;
    with(|a| match ev {
        Event::Title(t) => {
            if !a.local && t != a.local_name && !t.is_empty() && !t.starts_with("watch?") {
                a.now_title = t;
            }
        }
        Event::Paused(p) => a.paused = p,
        Event::Idle(idle) => {
            if !idle {
                a.loading = false;
            } else if !a.loading {
                a.playing = false;
            }
        }
        Event::Duration(d) => a.mpv_duration = d,
        Event::Pos(p) => a.pos = p,
        Event::Eof => advance = a.qi + 1 < a.queue.len(),
        Event::Error(e) => {
            a.loading = false;
            a.playing = false;
            a.error = Some(format!("Couldn't play: {e}"));
        }
        Event::Exited => {
            a.loading = false;
            a.playing = false;
        }
    });
    if advance {
        next();
        return;
    }
    repaint_list();
    repaint();
    tray(NIM_MODIFY);
}

// ---------------------------------------------------------------- painting

fn text(hdc: HDC, s: &str, rc: RECT, font: HFONT, color: COLORREF, flags: DRAW_TEXT_FORMAT) {
    // DrawTextW gets a dangling pointer for an empty slice (a pasted link has
    // no channel), which crashes in user32 even with a zero length.
    if s.is_empty() {
        return;
    }
    unsafe {
        let mut r = rc;
        SelectObject(hdc, font.into());
        SetTextColor(hdc, color);
        let mut w: Vec<u16> = s.encode_utf16().collect();
        DrawTextW(hdc, &mut w, &mut r, flags | DT_NOPREFIX);
    }
}

const LINE: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(DT_SINGLELINE.0 | DT_END_ELLIPSIS.0);

fn paint(a: &App, hdc: HDC) {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(a.hwnd, &mut rc);
        FillRect(hdc, &rc, a.brushes[B_BG]);
        SetBkMode(hdc, TRANSPARENT);
        match a.view {
            View::Search => paint_search(a, hdc, rc),
            View::Player => paint_player(a, hdc, rc),
        }
    }
}

fn toast_now(a: &App) -> Option<(String, COLORREF)> {
    a.toast
        .as_ref()
        .filter(|t| Instant::now() < t.until)
        .map(|t| (t.text.clone(), if t.error { ERR } else { ACCENT }))
}

fn status_rect(a: &App) -> RECT {
    let top = a.s(PAD + SEARCH_H + 8 + ROWS * ROW_H + 4);
    RECT {
        left: a.s(PAD + 4),
        top,
        right: a.s(W - PAD - 4),
        bottom: top + a.s(STATUS_H),
    }
}

fn status(a: &App) -> (String, COLORREF) {
    if let Some(toast) = volume_toast(a) {
        return (toast, ACCENT);
    }
    if let Some(toast) = toast_now(a) {
        return toast;
    }
    if let Some(e) = &a.error {
        return (e.clone(), ERR);
    }
    if a.searching {
        return ("Searching YouTube…".into(), DIM);
    }
    if a.loading {
        return (format!("Loading  {}", a.now_title), DIM);
    }
    if a.playing {
        let icon = if a.paused { "❚❚" } else { "▶" };
        return (
            format!("{icon}  {}      Tab: player", a.now_title),
            if a.paused { DIM } else { ACCENT },
        );
    }
    if a.source == Source::Library {
        return (library_status(a), DIM);
    }
    (
        "Enter search · ↓ results · Ctrl+↑↓ volume · Esc hide".into(),
        DIM,
    )
}

fn library_status(a: &App) -> String {
    let n = a.view_idx.len();
    if !a.query.is_empty() {
        if n == 0 {
            "No saved match · Enter: search YouTube".into()
        } else {
            format!("{n} saved · Enter: search YouTube")
        }
    } else if a.lib.is_empty() {
        "Nothing saved yet · ♥ on the player saves a track".into()
    } else {
        let bytes: u64 = a.lib.iter().map(|e| e.bytes).sum();
        let tracks = if n == 1 { "track" } else { "tracks" };
        format!("Library · {n} {tracks} · {}", library::fmt_bytes(bytes))
    }
}

fn paint_search(a: &App, hdc: HDC, rc: RECT) {
    unsafe {
        let field = RECT {
            left: a.s(PAD),
            top: a.s(PAD),
            right: rc.right - a.s(PAD),
            bottom: a.s(PAD + SEARCH_H),
        };
        let old_brush = SelectObject(hdc, a.brushes[B_PANEL].into());
        let pen = CreatePen(
            PS_SOLID,
            1,
            if GetFocus() == a.edit { ACCENT } else { BORDER },
        );
        let old_pen = SelectObject(hdc, pen.into());
        let _ = RoundRect(
            hdc,
            field.left,
            field.top,
            field.right,
            field.bottom,
            a.s(10),
            a.s(10),
        );
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(pen.into());
        let (s, color) = status(a);
        text(
            hdc,
            &s,
            status_rect(a),
            a.fonts.meta,
            color,
            LINE | DT_VCENTER,
        );
    }
}

/// Clickable areas of the player view, in client pixels.
struct PlayerHits {
    prev: RECT,
    play: RECT,
    next: RECT,
    heart: RECT,
    rows: Vec<RECT>,
}

fn player_hits(a: &App) -> PlayerHits {
    let cx = a.s(W) / 2;
    let cy = a.s(CTRL_CY);
    let r = a.s(20);
    let side = |dx: i32| RECT {
        left: cx + dx - a.s(16),
        top: cy - a.s(16),
        right: cx + dx + a.s(16),
        bottom: cy + a.s(16),
    };
    let rows = (0..UPNEXT_ROWS)
        .map(|k| RECT {
            left: a.s(P - 8),
            top: a.s(ROWS_Y + k * ROW_H),
            right: a.s(W - P + 8),
            bottom: a.s(ROWS_Y + (k + 1) * ROW_H),
        })
        .collect();
    PlayerHits {
        prev: side(-a.s(64)),
        play: RECT {
            left: cx - r,
            top: cy - r,
            right: cx + r,
            bottom: cy + r,
        },
        next: side(a.s(64)),
        heart: RECT {
            left: a.s(W - P - 32),
            top: a.s(40),
            right: a.s(W - P),
            bottom: a.s(68),
        },
        rows,
    }
}

/// The small heart on a result or Up next row: accent when saved, dim while
/// queued or downloading, nothing otherwise.
fn row_heart(a: &App, t: &Track) -> Option<COLORREF> {
    if library::is_saved(&a.lib, t) {
        Some(ACCENT)
    } else if a.saving.contains(&t.url) {
        Some(DIM)
    } else {
        None
    }
}

/// Draw `row_heart` at the right end of a meta line and return the meta line
/// narrowed so its text stops short of it.
fn paint_row_heart(a: &App, hdc: HDC, t: &Track, meta: RECT) -> RECT {
    let Some(color) = row_heart(a, t) else {
        return meta;
    };
    let w = a.s(18);
    let heart = RECT {
        left: meta.right - w,
        ..meta
    };
    text(
        hdc,
        G_HEART_FILL,
        heart,
        a.fonts.icons,
        color,
        DT_SINGLELINE | DT_RIGHT | DT_VCENTER,
    );
    RECT {
        right: heart.left - a.s(4),
        ..meta
    }
}

fn paint_player(a: &App, hdc: HDC, rc: RECT) {
    unsafe {
        let s = |v| a.s(v);
        let line = |top: i32, h: i32| RECT {
            left: s(P),
            top: s(top),
            right: rc.right - s(P),
            bottom: s(top + h),
        };
        let Some(t) = a.current() else {
            text(
                hdc,
                "Nothing playing. Type to search.",
                line(40, 26),
                a.fonts.title,
                DIM,
                LINE,
            );
            return;
        };
        let header = if a.loading {
            "LOADING…".to_string()
        } else if a.paused {
            "PAUSED".to_string()
        } else {
            "NOW PLAYING".to_string()
        };
        let header = if a.queue.len() > 1 {
            format!("{header}  ·  {} of {}", a.qi + 1, a.queue.len())
        } else {
            header
        };
        text(hdc, &header, line(18, 16), a.fonts.meta, DIM, LINE);
        let hits = player_hits(a);
        let title_rc = RECT {
            right: hits.heart.left - s(6),
            ..line(40, 28)
        };
        text(hdc, &a.now_title, title_rc, a.fonts.big, TEXT, LINE);
        let (glyph, color) = if library::is_saved(&a.lib, t) {
            (G_HEART_FILL, ACCENT)
        } else if a.saving.contains(&t.url) {
            (G_HEART_FILL, DIM)
        } else {
            (G_HEART, DIM)
        };
        text(
            hdc,
            glyph,
            hits.heart,
            a.fonts.icons,
            color,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER,
        );
        text(hdc, &t.channel, line(70, 18), a.fonts.meta, DIM, LINE);

        // Progress.
        let bar = RECT {
            left: s(P),
            top: s(BAR_Y),
            right: rc.right - s(P),
            bottom: s(BAR_Y) + s(4).max(3),
        };
        FillRect(hdc, &bar, a.brushes[B_TRACK]);
        let times = line(BAR_Y + 9, 16);
        match a.duration() {
            Some(d) if d > 0.0 => {
                let frac = (a.pos / d).clamp(0.0, 1.0);
                let fill = RECT {
                    right: bar.left + ((bar.right - bar.left) as f64 * frac) as i32,
                    ..bar
                };
                FillRect(hdc, &fill, a.brushes[B_ACCENT]);
                text(hdc, &fmt_secs(a.pos as u64), times, a.fonts.meta, DIM, LINE);
                text(
                    hdc,
                    &fmt_secs(d as u64),
                    times,
                    a.fonts.meta,
                    DIM,
                    LINE | DT_RIGHT,
                );
            }
            _ => {
                text(hdc, "● LIVE", times, a.fonts.meta, LIVE, LINE | DT_RIGHT);
            }
        }

        // Controls.
        let has_prev = a.qi > 0 || a.pos > 3.0;
        let has_next = a.qi + 1 < a.queue.len();
        text(
            hdc,
            G_PREV,
            hits.prev,
            a.fonts.icons,
            if has_prev { TEXT } else { BORDER },
            DT_SINGLELINE | DT_CENTER | DT_VCENTER,
        );
        text(
            hdc,
            G_NEXT,
            hits.next,
            a.fonts.icons,
            if has_next { TEXT } else { BORDER },
            DT_SINGLELINE | DT_CENTER | DT_VCENTER,
        );
        let old_brush = SelectObject(hdc, a.brushes[B_ACCENT].into());
        let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
        let p = hits.play;
        let _ = Ellipse(hdc, p.left, p.top, p.right + 1, p.bottom + 1);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let glyph = if a.paused { G_PLAY } else { G_PAUSE };
        text(
            hdc,
            glyph,
            p,
            a.fonts.icons,
            BG,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER,
        );

        // Up next.
        text(hdc, "UP NEXT", line(UPNEXT_Y, 16), a.fonts.meta, DIM, LINE);
        let upcoming: Vec<&Track> = a
            .queue
            .iter()
            .skip(a.qi + 1)
            .take(UPNEXT_ROWS as usize)
            .collect();
        if upcoming.is_empty() {
            text(
                hdc,
                "Nothing queued. Type to search.",
                line(ROWS_Y + 6, 20),
                a.fonts.meta,
                DIM,
                LINE,
            );
        }
        for (k, t) in upcoming.iter().enumerate() {
            let r = hits.rows[k];
            let title = RECT {
                left: s(P),
                top: r.top + s(5),
                right: rc.right - s(P),
                bottom: r.top + s(26),
            };
            let meta = RECT {
                left: s(P),
                top: r.top + s(25),
                right: rc.right - s(P),
                bottom: r.bottom - s(3),
            };
            text(hdc, &t.title, title, a.fonts.title, TEXT, LINE);
            let meta = paint_row_heart(a, hdc, t, meta);
            text(hdc, &t.meta(), meta, a.fonts.meta, DIM, LINE);
        }

        // Footer.
        let rule = RECT {
            left: 0,
            top: s(FOOT_Y),
            right: rc.right,
            bottom: s(FOOT_Y) + 1,
        };
        FillRect(hdc, &rule, a.brushes[B_RULE]);
        let foot = line(FOOT_Y + 1, 34);
        const HINT: &str = "Space pause · ←→ tracks · ↑↓ volume  Shift ±10 · Tab search";
        text(hdc, HINT, foot, a.fonts.meta, DIM, LINE | DT_VCENTER);
        let hint_w = text_width(hdc, HINT, a.fonts.meta);
        let right = if let Some(toast) = toast_now(a) {
            toast
        } else if volume_toast(a).is_some() {
            // Leave room for the hint; drop the dB when it would overlap.
            let room = (rc.right - s(P)) - (s(P) + hint_w + s(12));
            (volume_text_fitted(a.level, room, hdc, a.fonts.meta), ACCENT)
        } else {
            ("type to search".to_string(), DIM)
        };
        // Right-aligned, but never over the hint: long messages get an ellipsis.
        let foot_right = RECT {
            left: foot.left + hint_w + s(12),
            ..foot
        };
        text(
            hdc,
            &right.0,
            foot_right,
            a.fonts.meta,
            right.1,
            LINE | DT_VCENTER | DT_RIGHT,
        );
    }
}

fn on_player_click(x: i32, y: i32) {
    let Some(hits) = with(|a| {
        if a.view == View::Player && a.playing {
            Some(player_hits(a))
        } else {
            None
        }
    })
    .flatten() else {
        return;
    };
    let pt = POINT { x, y };
    let inside = |r: &RECT| unsafe { PtInRect(r, pt).as_bool() };
    if inside(&hits.play) {
        with(|a| a.player.toggle_pause());
    } else if inside(&hits.heart) {
        save_current();
    } else if inside(&hits.prev) {
        prev();
    } else if inside(&hits.next) {
        next();
    } else if let Some(k) = hits.rows.iter().position(inside) {
        let target = with(|a| {
            let i = a.qi + 1 + k;
            (i < a.queue.len()).then(|| {
                a.qi = i;
            })
        })
        .flatten();
        if target.is_some() {
            play_current();
        }
    }
}

fn draw_item(a: &App, dis: &DRAWITEMSTRUCT) {
    unsafe {
        let hdc = dis.hDC;
        let rc = dis.rcItem;
        let selected = dis.itemState.0 & ODS_SELECTED.0 != 0;
        FillRect(
            hdc,
            &rc,
            if selected {
                a.brushes[B_SEL]
            } else {
                a.brushes[B_BG]
            },
        );
        // A result row (with its heart) or a library row (all saved, so no
        // heart; flagged when its file has gone).
        let id = dis.itemID as usize;
        let row = match a.source {
            Source::Results => a.results.get(id).map(|t| (t.clone(), false, true)),
            Source::Library => a.view_idx.get(id).and_then(|&i| a.lib.get(i)).map(|e| {
                let gone = !library::audio_dir(&a.lib_root).join(&e.file).is_file();
                (e.track(), gone, false)
            }),
        };
        let Some((t, missing, hearts)) = row else {
            return;
        };
        let t = &t;
        let playing = a.current().is_some_and(|c| c.is_same(t));
        if playing {
            let bar = RECT {
                left: rc.left,
                top: rc.top + a.s(8),
                right: rc.left + a.s(3),
                bottom: rc.bottom - a.s(8),
            };
            FillRect(hdc, &bar, a.brushes[B_ACCENT]);
        }
        SetBkMode(hdc, TRANSPARENT);
        let title_rc = RECT {
            left: rc.left + a.s(14),
            top: rc.top + a.s(5),
            right: rc.right - a.s(10),
            bottom: rc.top + a.s(26),
        };
        let title = if playing {
            format!("▶  {}", t.title)
        } else {
            t.title.clone()
        };
        text(
            hdc,
            &title,
            title_rc,
            a.fonts.title,
            if playing { ACCENT } else { TEXT },
            LINE,
        );
        let meta_rc = RECT {
            left: rc.left + a.s(14),
            top: rc.top + a.s(25),
            right: rc.right - a.s(10),
            bottom: rc.bottom - a.s(3),
        };
        let mut meta_rc = if hearts {
            paint_row_heart(a, hdc, t, meta_rc)
        } else {
            meta_rc
        };
        if missing {
            const GONE: &str = "File missing";
            let w = text_width(hdc, GONE, a.fonts.meta) + a.s(4);
            let gone = RECT {
                left: meta_rc.right - w,
                ..meta_rc
            };
            text(hdc, GONE, gone, a.fonts.meta, ERR, LINE | DT_RIGHT);
            meta_rc.right = gone.left - a.s(8);
        }
        text(hdc, &t.meta(), meta_rc, a.fonts.meta, DIM, LINE);
    }
}

// ---------------------------------------------------------------- tray

fn tray(op: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE) {
    with(|a| unsafe {
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: a.hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: a.icon,
            ..Default::default()
        };
        let tip = if a.playing {
            format!("musica — {}", a.now_title)
        } else {
            "musica".to_string()
        };
        for (d, c) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *d = c;
        }
        let _ = Shell_NotifyIconW(op, &nid);
    });
}

fn tray_menu(hwnd: HWND) {
    let Some((title, playing, paused, has_next)) = with(|a| {
        (
            a.now_title.clone(),
            a.playing,
            a.paused,
            a.qi + 1 < a.queue.len(),
        )
    }) else {
        return;
    };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        if playing {
            let label: String = title.chars().take(60).collect();
            let _ = AppendMenuW(
                menu,
                MF_STRING | MF_GRAYED,
                0,
                PCWSTR(wide(&label).as_ptr()),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                CMD_PAUSE,
                if paused { w!("Resume") } else { w!("Pause") },
            );
            if has_next {
                let _ = AppendMenuW(menu, MF_STRING, CMD_NEXT, w!("Next"));
            }
            let _ = AppendMenuW(menu, MF_STRING, CMD_STOP, w!("Stop"));
        }
        let _ = AppendMenuW(menu, MF_STRING, CMD_SHOW, w!("Open\tCaps+M"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit musica"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

/// A 16px-at-96-DPI accent disc with a note on it, drawn at runtime so the
/// build needs no resource compiler.
fn make_icon() -> HICON {
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let color = CreateCompatibleBitmap(screen, size, size);
        let mask = CreateBitmap(size, size, 1, 1, None);
        let full = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };

        // Mask: white = transparent, black disc = opaque.
        let old = SelectObject(dc, mask.into());
        FillRect(dc, &full, HBRUSH(GetStockObject(WHITE_BRUSH).0));
        SelectObject(dc, GetStockObject(BLACK_BRUSH));
        SelectObject(dc, GetStockObject(NULL_PEN));
        let _ = Ellipse(dc, 0, 0, size + 1, size + 1);

        // Colour: black outside the disc, accent disc, dark note.
        SelectObject(dc, color.into());
        FillRect(dc, &full, HBRUSH(GetStockObject(BLACK_BRUSH).0));
        let accent = CreateSolidBrush(ACCENT);
        SelectObject(dc, accent.into());
        let _ = Ellipse(dc, 0, 0, size + 1, size + 1);
        let font = CreateFontW(
            -(size * 3 / 4),
            0,
            0,
            0,
            700,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            w!("Segoe UI Symbol"),
        );
        SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, BG);
        let mut note: Vec<u16> = "♪".encode_utf16().collect();
        let mut r = full;
        DrawTextW(
            dc,
            &mut note,
            &mut r,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX,
        );
        SelectObject(dc, old);

        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(accent.into());
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        icon
    }
}

// ---------------------------------------------------------------- wndproc

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TOGGLE => {
                toggle_popup();
                LRESULT(0)
            }
            WM_SHOW => {
                show_popup();
                LRESULT(0)
            }
            WM_QUIT_RESIDENT => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_RESULTS => {
                let b = Box::from_raw(lp.0 as *mut (u64, std::result::Result<Vec<Track>, String>));
                let (seq, r) = *b;
                on_results(seq, r);
                LRESULT(0)
            }
            WM_PLAYER => {
                on_player(*Box::from_raw(lp.0 as *mut Event));
                LRESULT(0)
            }
            WM_LIBRARY => {
                let b = Box::from_raw(lp.0 as *mut (String, std::result::Result<Printed, String>));
                let (url, r) = *b;
                on_library(url, r);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == TIMER_POS => {
                let poll = with(|a| {
                    if a.view == View::Player && a.playing && !a.paused {
                        a.player.query_pos();
                    }
                    let now = Instant::now();
                    let toast_done = a.toast.as_ref().is_some_and(|t| now >= t.until);
                    if toast_done {
                        a.toast = None;
                    }
                    let flash_done = a.volume_flash.is_some_and(|t| now >= t);
                    if flash_done {
                        a.volume_flash = None;
                    }
                    toast_done || flash_done
                });
                if poll == Some(true) {
                    repaint();
                }
                LRESULT(0)
            }
            WM_TRAY => {
                match lp.0 as u32 {
                    WM_LBUTTONUP => toggle_popup(),
                    WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let x = (lp.0 & 0xffff) as i16 as i32;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as i32;
                on_player_click(x, y);
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = wp.0 & 0xffff;
                let code = (wp.0 >> 16) & 0xffff;
                match (id, code) {
                    (ID_LIST, 2) => {
                        // LBN_DBLCLK
                        let list = with(|a| a.list).unwrap();
                        let i = SendMessageW(list, LB_GETCURSEL, None, None).0;
                        if i >= 0 {
                            play_shown(i as usize, false);
                        }
                    }
                    (ID_EDIT, 0x300) => on_edit_change(), // EN_CHANGE
                    (ID_EDIT, 0x100) | (ID_EDIT, 0x200) => {
                        // EN_SETFOCUS / EN_KILLFOCUS: repaint the field outline.
                        repaint();
                    }
                    (CMD_PAUSE, 0) => {
                        with(|a| a.player.toggle_pause());
                    }
                    (CMD_NEXT, 0) => next(),
                    (CMD_STOP, 0) => {
                        with(|a| a.player.stop());
                    }
                    (CMD_SHOW, 0) => show_popup(),
                    (CMD_QUIT, 0) => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_ACTIVATE => {
                if (wp.0 & 0xffff) as u32 == WA_INACTIVE {
                    hide_popup();
                }
                LRESULT(0)
            }
            WM_CTLCOLOREDIT => {
                let hdc = HDC(wp.0 as _);
                SetTextColor(hdc, TEXT);
                SetBkColor(hdc, PANEL);
                LRESULT(with(|a| a.brushes[B_PANEL].0 as isize).unwrap_or(0))
            }
            WM_CTLCOLORLISTBOX => LRESULT(with(|a| a.brushes[B_BG].0 as isize).unwrap_or(0)),
            WM_DRAWITEM => {
                let dis = &*(lp.0 as *const DRAWITEMSTRUCT);
                with(|a| draw_item(a, dis));
                LRESULT(1)
            }
            WM_MEASUREITEM => {
                let mis = &mut *(lp.0 as *mut MEASUREITEMSTRUCT);
                mis.itemHeight = ROW_H as u32;
                LRESULT(1)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                // Paint off-screen and blit once: the progress bar repaints
                // twice a second and would flicker otherwise.
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let mem = CreateCompatibleDC(Some(hdc));
                let bmp = CreateCompatibleBitmap(hdc, rc.right.max(1), rc.bottom.max(1));
                let old = SelectObject(mem, bmp.into());
                with(|a| paint(a, mem));
                let _ = BitBlt(hdc, 0, 0, rc.right, rc.bottom, Some(mem), 0, 0, SRCCOPY);
                SelectObject(mem, old);
                let _ = DeleteObject(bmp.into());
                let _ = DeleteDC(mem);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DPICHANGED => LRESULT(0),
            WM_DESTROY => {
                tray(NIM_DELETE);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => {
                if with(|a| a.taskbar_created == msg) == Some(true) {
                    tray(NIM_ADD);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
    }
}
