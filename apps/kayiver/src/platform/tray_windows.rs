//! Windows system-tray indicator (notification area).
//!
//! Runs on its own thread with a message-only window, fully isolated from the
//! input hooks — if anything here fails, kayiver keeps working. Shows kayiver's
//! live state (disconnected / connected / cursor-is-here) as the icon tooltip
//! and a balloon on state changes, plus a right-click menu (open editor, quit).

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, NOTIFYICONDATAW_0, NIIF_INFO, NIIF_WARNING,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, LoadIconW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
    TrackPopupMenu, TranslateMessage, HICON, HMENU, IDI_APPLICATION, MF_STRING, MSG,
    TPM_BOTTOMALIGN, TPM_RIGHTALIGN, WM_APP, WM_COMMAND, WM_DESTROY, WM_RBUTTONUP, WNDCLASSW,
    WINDOW_EX_STYLE, WINDOW_STYLE,
};

const WM_TRAY: u32 = WM_APP + 1;
const ID_OPEN: usize = 1001;
const ID_QUIT: usize = 1002;
const TRAY_UID: u32 = 0xD41F;

/// 0 = disconnected, 1 = connected (cursor local), 2 = cursor is on this PC.
static STATE: AtomicU8 = AtomicU8::new(0);
static HWND_VAL: AtomicIsize = AtomicIsize::new(0);
static HOST_NAME: OnceLock<String> = OnceLock::new();

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Write a string into a fixed-size u16 buffer (tooltip / balloon fields).
fn fill(buf: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(buf.len() - 1).collect();
    buf[..w.len()].copy_from_slice(&w);
    buf[w.len()] = 0;
}

fn tooltip() -> String {
    let host = HOST_NAME.get().map(|s| s.as_str()).unwrap_or("host");
    match STATE.load(Ordering::Relaxed) {
        2 => format!("kayiver — controlling this PC (from {host})"),
        1 => format!("kayiver — connected to {host}"),
        _ => "kayiver — waiting for host…".to_string(),
    }
}

/// Public: start the tray (call once, on the client). Non-fatal.
pub fn start(host: &str) {
    let _ = HOST_NAME.set(host.to_string());
    std::thread::Builder::new()
        .name("kayiver-tray".into())
        .spawn(|| unsafe { run() })
        .ok();
}

/// Public: update the indicator when connection / focus changes.
/// Balloon notifications fire on both transitions: connected AND lost.
pub fn set_state(connected: bool, cursor_here: bool) {
    let s = if !connected { 0 } else if cursor_here { 2 } else { 1 };
    let prev = STATE.swap(s, Ordering::Relaxed);
    if prev != s {
        let came_up = prev == 0 && s != 0;
        let went_down = prev != 0 && s == 0;
        unsafe { refresh_with(came_up, went_down) };
    }
}

unsafe fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut n = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        ..Default::default()
    };
    n.Anonymous = NOTIFYICONDATAW_0 { uVersion: 4 };
    n
}

/// Refresh tooltip + optionally pop a balloon on connect / disconnect.
unsafe fn refresh_with(connected_balloon: bool, lost_balloon: bool) {
    let h = HWND_VAL.load(Ordering::Relaxed);
    if h == 0 {
        return;
    }
    let hwnd = HWND(h as *mut _);
    let mut data = nid(hwnd);
    data.uFlags = NIF_TIP | NIF_ICON | NIF_MESSAGE;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = load_icon();
    fill(&mut data.szTip, &tooltip());
    if connected_balloon {
        data.uFlags |= NIF_INFO;
        fill(&mut data.szInfoTitle, "Kayıver bağlandı");
        fill(&mut data.szInfo, &tooltip());
        data.dwInfoFlags = NIIF_INFO;
    } else if lost_balloon {
        let host = HOST_NAME.get().map(|s| s.as_str()).unwrap_or("host");
        data.uFlags |= NIF_INFO;
        fill(&mut data.szInfoTitle, "Kayıver bağlantısı koptu");
        fill(
            &mut data.szInfo,
            &format!("{host} ile bağlantı kesildi — yeniden deneniyor. Sebep için editörü aç (sağ tık → Aç)."),
        );
        data.dwInfoFlags = NIIF_WARNING;
    }
    let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
}

unsafe fn load_icon() -> HICON {
    // Icon resource #1 = the app icon embedded by build.rs (winres);
    // fall back to the stock application icon if it's missing.
    if let Ok(hinst) = GetModuleHandleW(None) {
        if let Ok(icon) = LoadIconW(Some(hinst.into()), PCWSTR(1 as *const u16)) {
            return icon;
        }
    }
    LoadIconW(None, IDI_APPLICATION).unwrap_or(HICON(std::ptr::null_mut()))
}

unsafe fn run() {
    let hinst = match GetModuleHandleW(None) {
        Ok(h) => h,
        Err(_) => return,
    };
    let class_name = to_wide("kayiver_tray_class");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    RegisterClassW(&wc);

    let win_name = to_wide("kayiver");
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(class_name.as_ptr()),
        PCWSTR(win_name.as_ptr()),
        WINDOW_STYLE(0),
        0, 0, 0, 0,
        None, // top-level hidden window (never shown)
        None,
        Some(hinst.into()),
        None,
    );
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(_) => return,
    };
    HWND_VAL.store(hwnd.0 as isize, Ordering::Relaxed);

    // Add the icon.
    let mut data = nid(hwnd);
    data.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = load_icon();
    fill(&mut data.szTip, &tooltip());
    let _ = Shell_NotifyIconW(NIM_ADD, &data);

    // Pump messages for this window.
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    // Cleanup on quit.
    let data = nid(hwnd);
    let _ = Shell_NotifyIconW(NIM_DELETE, &data);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let ev = (lparam.0 & 0xFFFF) as u32;
            if ev == WM_RBUTTONUP {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as usize {
                ID_OPEN => open_editor_window(),
                ID_QUIT => {
                    std::process::exit(0);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Open the (locally served) editor as a chromeless app window using whichever
/// Chromium browser is installed, so it feels like a native panel instead of a
/// browser tab. Falls back to the default browser.
fn open_editor_window() {
    let url = "http://127.0.0.1:24818";
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let candidates = [
        format!(r"{pf}\Google\Chrome\Application\chrome.exe"),
        format!(r"{pf86}\Google\Chrome\Application\chrome.exe"),
        format!(r"{local}\Google\Chrome\Application\chrome.exe"),
        format!(r"{pf86}\Microsoft\Edge\Application\msedge.exe"),
        format!(r"{pf}\Microsoft\Edge\Application\msedge.exe"),
    ];
    for c in candidates {
        if std::path::Path::new(&c).exists() {
            if std::process::Command::new(&c)
                .arg(format!("--app={url}"))
                .arg("--window-size=1040,720")
                .spawn()
                .is_ok()
            {
                return;
            }
        }
    }
    // Fallback: default browser.
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return,
    };
    let open = to_wide("Open layout editor");
    let quit = to_wide("Quit kayiver");
    let _ = AppendMenuW(menu, MF_STRING, ID_OPEN, PCWSTR(open.as_ptr()));
    let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, PCWSTR(quit.as_ptr()));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // Required so the menu dismisses correctly.
    let _ = SetForegroundWindow(hwnd);
    let _ = TrackPopupMenu(menu, TPM_RIGHTALIGN | TPM_BOTTOMALIGN, pt.x, pt.y, Some(0), hwnd, None);
    let _ = DestroyMenu(menu);
}

// Unused params kept for signature clarity.
const _: HMENU = HMENU(std::ptr::null_mut());
