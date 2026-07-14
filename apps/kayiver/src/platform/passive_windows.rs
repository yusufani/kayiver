//! Windows "passive shared monitor" notice.
//!
//! When this machine is NOT the one being shown on the shared panel, its copy
//! of that monitor is invisible (the panel is displaying the other machine).
//! We cover exactly that monitor with a plain full-screen notice so that, if
//! the user flips the monitor's physical input to this machine, they see why
//! the cursor won't go there and how to take it (Ctrl+Alt+M). Shown/hidden as
//! the shared-monitor owner changes; a no-op when this machine owns the panel.

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use kayiver_core::proto::Rect;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DrawTextW, EndPaint, FillRect, SetBkMode, SetTextColor,
    DT_CENTER, DT_VCENTER, DT_WORDBREAK, HBRUSH, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, RegisterClassW,
    SetWindowPos, ShowWindow, HWND_TOPMOST, MSG, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNA, WNDCLASSW,
    WM_APP, WM_PAINT, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

static HWND_VAL: AtomicIsize = AtomicIsize::new(0);
static PENDING: OnceLock<Mutex<Option<(Rect, String)>>> = OnceLock::new();
const WM_UPDATE: u32 = WM_APP + 7;

fn pending() -> &'static Mutex<Option<(Rect, String)>> {
    PENDING.get_or_init(|| Mutex::new(None))
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Show the notice on `rect` with `msg`, or hide it (None). Safe to call from
/// any thread; the window lives on its own thread with a message pump.
pub fn show(state: Option<(Rect, String)>) {
    *pending().lock().unwrap() = state;
    let h = HWND_VAL.load(Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_UPDATE, WPARAM(0), LPARAM(0));
        }
    } else {
        std::thread::Builder::new().name("kayiver-passive".into()).spawn(|| unsafe { run() }).ok();
    }
}

unsafe fn run() {
    let Ok(hinst) = GetModuleHandleW(None) else { return };
    let class = to_wide("kayiver_passive_class");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: PCWSTR(class.as_ptr()),
        hbrBackground: CreateSolidBrush(COLORREF(0x0014_0B0B)), // near-black
        ..Default::default()
    };
    RegisterClassW(&wc);
    let name = to_wide("Kayıver");
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        PCWSTR(class.as_ptr()),
        PCWSTR(name.as_ptr()),
        WS_POPUP,
        0, 0, 0, 0,
        None, None, Some(hinst.into()), None,
    );
    let Ok(hwnd) = hwnd else { return };
    HWND_VAL.store(hwnd.0 as isize, Ordering::Relaxed);
    apply(hwnd);

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        DispatchMessageW(&msg);
    }
}

/// Position + show/hide the window to match the pending state.
unsafe fn apply(hwnd: HWND) {
    match pending().lock().unwrap().clone() {
        Some((r, _)) => {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), r.x, r.y, r.w, r.h, SWP_NOACTIVATE);
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            unsafe { windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true) };
        }
        None => {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_UPDATE => {
            apply(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
            let bg: HBRUSH = CreateSolidBrush(COLORREF(0x0014_0B0B));
            FillRect(hdc, &rc, bg);
            SetBkMode(hdc, TRANSPARENT);
            let text = pending()
                .lock()
                .unwrap()
                .clone()
                .map(|(_, m)| m)
                .unwrap_or_default();
            // accent line
            SetTextColor(hdc, COLORREF(0x00D3_A934)); // teal-ish (BGR)
            let mut top = rc;
            top.bottom = rc.top + (rc.bottom - rc.top) / 2;
            let mut title = to_wide("Kayıver — bu ekran şu an pasif");
            DrawTextW(hdc, &mut title, &mut top, DT_CENTER | DT_VCENTER | DT_WORDBREAK);
            SetTextColor(hdc, COLORREF(0x00A3_938B)); // muted
            let mut bottom = rc;
            bottom.top = rc.top + (rc.bottom - rc.top) / 2;
            let mut w = to_wide(&text);
            DrawTextW(hdc, &mut w, &mut bottom, DT_CENTER | DT_WORDBREAK);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
