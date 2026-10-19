//! The resident: tray icon plus a spotlight-style popup (search box, results,
//! status line). Plain Win32 controls, owner-drawn results, one UI thread.

use std::cell::RefCell;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
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
    keybd_event, GetFocus, SetFocus, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VK_DOWN, VK_ESCAPE,
    VK_MENU, VK_RETURN, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::launch::{CLASS, MUTEX, WM_QUIT_RESIDENT, WM_SHOW, WM_TOGGLE};
use crate::log::log;
use crate::player::{Event, Player};
use crate::search::{self, Track};

const WM_TRAY: u32 = WM_APP + 10;
const WM_RESULTS: u32 = WM_APP + 11;
const WM_PLAYER: u32 = WM_APP + 12;

const ID_EDIT: usize = 100;
const ID_LIST: usize = 101;
const CMD_PAUSE: usize = 1;
const CMD_STOP: usize = 2;
const CMD_SHOW: usize = 3;
const CMD_QUIT: usize = 4;

// Layout in 96-DPI pixels.
const W: i32 = 640;
const PAD: i32 = 12;
const SEARCH_H: i32 = 44;
const ROW_H: i32 = 46;
const ROWS: i32 = search::LIMIT as i32;
const STATUS_H: i32 = 34;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}
const BG: COLORREF = rgb(0x0f, 0x14, 0x1a);
const PANEL: COLORREF = rgb(0x19, 0x20, 0x2a);
const SEL: COLORREF = rgb(0x22, 0x2b, 0x37);
const TEXT: COLORREF = rgb(0xe9, 0xed, 0xf2);
const DIM: COLORREF = rgb(0x8b, 0x96, 0xa5);
const ACCENT: COLORREF = rgb(0xff, 0x9e, 0x0b);
const ERR: COLORREF = rgb(0xff, 0x6b, 0x6b);
const BORDER: COLORREF = rgb(0x2a, 0x33, 0x40);

struct Fonts {
    search: HFONT,
    title: HFONT,
    meta: HFONT,
}

struct App {
    hwnd: HWND,
    edit: HWND,
    list: HWND,
    dpi: u32,
    fonts: Fonts,
    bg: HBRUSH,
    panel: HBRUSH,
    sel: HBRUSH,
    accent: HBRUSH,
    icon: HICON,
    taskbar_created: u32,
    results: Vec<Track>,
    seq: u64,
    searching: bool,
    loading: bool,
    error: Option<String>,
    now_url: Option<String>,
    now_title: String,
    paused: bool,
    idle: bool,
    player: Player,
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
        let _mutex = CreateMutexW(None, true, MUTEX)?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(0);
        }
        log("resident starting");
        let instance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS,
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
        SendMessageW(
            edit,
            0x1501,
            Some(WPARAM(1)),
            Some(LPARAM(
                w!("Search YouTube, or paste a link").as_ptr() as isize
            )),
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
        let player = Player::new(Box::new(move |ev: Event| {
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
        }));
        let app = App {
            hwnd,
            edit,
            list,
            dpi: 0,
            fonts: make_fonts(96),
            bg: CreateSolidBrush(BG),
            panel: CreateSolidBrush(PANEL),
            sel: CreateSolidBrush(SEL),
            accent: CreateSolidBrush(ACCENT),
            icon: make_icon(),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            results: Vec::new(),
            seq: 0,
            searching: false,
            loading: false,
            error: None,
            now_url: None,
            now_title: String::new(),
            paused: false,
            idle: true,
            player,
        };
        APP.with(|a| *a.borrow_mut() = Some(app));
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
    let font = |px: i32, weight: i32| unsafe {
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
            w!("Segoe UI"),
        )
    };
    Fonts {
        search: font(18, 400),
        title: font(15, 600),
        meta: font(13, 400),
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
        for f in [old.search, old.title, old.meta] {
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
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn height() -> i32 {
    PAD + SEARCH_H + 8 + ROWS * ROW_H + 4 + STATUS_H + 4
}

fn show_popup() {
    let Some(hwnd) = with(|a| a.hwnd) else { return };
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
        let (w, h) = (scale(W, dx), scale(height(), dx));
        let x = work.left + (work.right - work.left - w) / 2;
        let y = work.top + (work.bottom - work.top) / 5;
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
        focus_popup(hwnd);
        let edit = with(|a| a.edit).unwrap();
        let _ = SetFocus(Some(edit));
        SendMessageW(edit, 0xB1, Some(WPARAM(0)), Some(LPARAM(-1))); // EM_SETSEL all
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

/// Keys handled before the controls see them. Returns true when consumed.
fn pretranslate(msg: &MSG) -> bool {
    let Some((edit, list, has_results)) = with(|a| (a.edit, a.list, !a.results.is_empty())) else {
        return false;
    };
    if msg.hwnd != edit && msg.hwnd != list {
        return false;
    }
    unsafe {
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
        match key {
            k if k == VK_ESCAPE.0 => {
                hide_popup();
                true
            }
            k if k == VK_RETURN.0 && msg.hwnd == edit => {
                submit();
                true
            }
            k if k == VK_RETURN.0 => {
                let i = SendMessageW(list, LB_GETCURSEL, None, None).0;
                if i >= 0 {
                    play_index(i as usize);
                }
                true
            }
            k if k == VK_DOWN.0 && msg.hwnd == edit && has_results => {
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
                with(|a| a.player.toggle_pause());
                true
            }
            _ => false,
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
        play_url(q, None);
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
    refresh_status();
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
    let Some(Some((list, n))) = with(|a| {
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
                a.results = v;
            }
            Err(e) => {
                log(format!("search #{seq} failed: {e}"));
                a.error = Some(e);
                a.results.clear();
            }
        }
        Some((a.list, a.results.len()))
    }) else {
        return;
    };
    unsafe {
        SendMessageW(list, LB_SETCOUNT, Some(WPARAM(n)), None);
        if n > 0 {
            SendMessageW(list, LB_SETCURSEL, Some(WPARAM(0)), None);
            if GetFocus() != list {
                let _ = SetFocus(Some(list));
            }
        }
        let _ = InvalidateRect(Some(list), None, true);
    }
    refresh_status();
}

fn play_index(i: usize) {
    if let Some(Some(t)) = with(|a| a.results.get(i).cloned()) {
        play_url(t.url, Some(t.title));
    }
}

fn play_url(url: String, title: Option<String>) {
    log(format!("play {url}"));
    let r = with(|a| {
        a.error = None;
        a.loading = true;
        a.now_url = Some(url.clone());
        a.now_title = title.unwrap_or_else(|| url.clone());
        a.player.play(&url)
    });
    if let Some(Err(e)) = r {
        log(format!("play failed: {e}"));
        with(|a| {
            a.loading = false;
            a.error = Some(e);
        });
    }
    if let Some(list) = with(|a| a.list) {
        unsafe {
            let _ = InvalidateRect(Some(list), None, true);
        }
    }
    refresh_status();
    tray(NIM_MODIFY);
}

fn on_player(ev: Event) {
    with(|a| match ev {
        Event::Title(t) => {
            if !t.is_empty() && !t.starts_with("watch?") {
                a.now_title = t;
            }
        }
        Event::Paused(p) => a.paused = p,
        Event::Idle(idle) => {
            a.idle = idle;
            if !idle {
                a.loading = false;
            } else if !a.loading {
                a.now_url = None;
            }
        }
        Event::Error(e) => {
            a.loading = false;
            a.now_url = None;
            a.error = Some(format!("Couldn't play: {e}"));
        }
        Event::Exited => {
            a.loading = false;
            a.idle = true;
            a.now_url = None;
        }
    });
    if let Some(list) = with(|a| a.list) {
        unsafe {
            let _ = InvalidateRect(Some(list), None, true);
        }
    }
    refresh_status();
    tray(NIM_MODIFY);
}

fn status_rect(a: &App) -> RECT {
    let s = |v| scale(v, a.dpi.max(96));
    let top = s(PAD + SEARCH_H + 8 + ROWS * ROW_H + 4);
    RECT {
        left: 0,
        top,
        right: s(W),
        bottom: top + s(STATUS_H),
    }
}

fn refresh_status() {
    with(|a| unsafe {
        let r = status_rect(a);
        let _ = InvalidateRect(Some(a.hwnd), Some(&r), true);
    });
}

fn status(a: &App) -> (String, COLORREF) {
    if let Some(e) = &a.error {
        return (e.clone(), ERR);
    }
    if a.searching {
        return ("Searching YouTube…".into(), DIM);
    }
    if a.loading {
        return (format!("Loading  {}", a.now_title), DIM);
    }
    if !a.idle {
        let icon = if a.paused { "❚❚" } else { "▶" };
        return (
            format!("{icon}  {}", a.now_title),
            if a.paused { DIM } else { ACCENT },
        );
    }
    (
        "Enter search · ↓ results · Enter play · Space pause · Esc hide".into(),
        DIM,
    )
}

fn paint(a: &App, hdc: HDC) {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(a.hwnd, &mut rc);
        FillRect(hdc, &rc, a.bg);
        let s = |v| scale(v, a.dpi.max(96));
        // Search field panel.
        let field = RECT {
            left: s(PAD),
            top: s(PAD),
            right: rc.right - s(PAD),
            bottom: s(PAD + SEARCH_H),
        };
        let old_brush = SelectObject(hdc, a.panel.into());
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
            s(10),
            s(10),
        );
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_brush);
        let _ = DeleteObject(pen.into());
        // Status line.
        let (text, color) = status(a);
        let mut r = status_rect(a);
        r.left = s(PAD + 4);
        r.right = rc.right - s(PAD + 4);
        SelectObject(hdc, a.fonts.meta.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, color);
        let mut t: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(
            hdc,
            &mut t,
            &mut r,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
    }
}

fn draw_item(a: &App, dis: &DRAWITEMSTRUCT) {
    unsafe {
        let hdc = dis.hDC;
        let rc = dis.rcItem;
        let selected = dis.itemState.0 & ODS_SELECTED.0 != 0;
        FillRect(hdc, &rc, if selected { a.sel } else { a.bg });
        let Some(t) = a.results.get(dis.itemID as usize) else {
            return;
        };
        let s = |v| scale(v, a.dpi.max(96));
        let playing = a.now_url.as_deref() == Some(t.url.as_str());
        if selected || playing {
            let bar = RECT {
                left: rc.left,
                top: rc.top + s(8),
                right: rc.left + s(3),
                bottom: rc.bottom - s(8),
            };
            FillRect(hdc, &bar, if playing { a.accent } else { a.panel });
        }
        SetBkMode(hdc, TRANSPARENT);
        let flags = DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
        let mut title_rc = RECT {
            left: rc.left + s(14),
            top: rc.top + s(5),
            right: rc.right - s(10),
            bottom: rc.top + s(26),
        };
        SelectObject(hdc, a.fonts.title.into());
        SetTextColor(hdc, if playing { ACCENT } else { TEXT });
        let title = if playing {
            format!("▶  {}", t.title)
        } else {
            t.title.clone()
        };
        let mut w: Vec<u16> = title.encode_utf16().collect();
        DrawTextW(hdc, &mut w, &mut title_rc, flags);
        let mut meta_rc = RECT {
            left: rc.left + s(14),
            top: rc.top + s(25),
            right: rc.right - s(10),
            bottom: rc.bottom - s(3),
        };
        SelectObject(hdc, a.fonts.meta.into());
        SetTextColor(hdc, DIM);
        let mut w: Vec<u16> = t.meta().encode_utf16().collect();
        DrawTextW(hdc, &mut w, &mut meta_rc, flags);
    }
}

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
        let tip = if a.idle && !a.loading {
            "musica".to_string()
        } else {
            format!("musica — {}", a.now_title)
        };
        for (d, c) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *d = c;
        }
        let _ = Shell_NotifyIconW(op, &nid);
    });
}

fn tray_menu(hwnd: HWND) {
    let Some((title, playing, paused)) =
        with(|a| (a.now_title.clone(), !a.idle || a.loading, a.paused))
    else {
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
            let _ = AppendMenuW(menu, MF_STRING, CMD_STOP, w!("Stop"));
        }
        let _ = AppendMenuW(menu, MF_STRING, CMD_SHOW, w!("Search…\tCaps+M"));
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
            WM_TRAY => {
                match lp.0 as u32 {
                    WM_LBUTTONUP => toggle_popup(),
                    WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(hwnd),
                    _ => {}
                }
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
                            play_index(i as usize);
                        }
                    }
                    (ID_EDIT, 0x100) | (ID_EDIT, 0x200) => {
                        // EN_SETFOCUS / EN_KILLFOCUS: repaint the field outline.
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    (CMD_PAUSE, 0) => {
                        with(|a| a.player.toggle_pause());
                    }
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
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                LRESULT(0)
            }
            WM_CTLCOLOREDIT => {
                let hdc = HDC(wp.0 as _);
                SetTextColor(hdc, TEXT);
                SetBkColor(hdc, PANEL);
                LRESULT(with(|a| a.panel.0 as isize).unwrap_or(0))
            }
            WM_CTLCOLORLISTBOX => LRESULT(with(|a| a.bg.0 as isize).unwrap_or(0)),
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
                with(|a| paint(a, hdc));
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
