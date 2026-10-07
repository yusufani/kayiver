//! Windows Quick Share bubble popup.
//!
//! Renders a floating, non-activating, sleek dark-themed action card in the
//! bottom-right corner of whichever monitor the cursor is currently on.
//! Supports clicking to open URLs or initiate/track file transfers.

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use kayiver_core::proto::QuickSharePayload;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse, EndPaint, FillRect,
    GetMonitorInfoW, InvalidateRect, LineTo, MonitorFromPoint, MoveToEx, Polygon, RoundRect,
    SelectObject, SetBkMode, SetTextColor, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW,
    KillTimer, PostMessageW, RegisterClassW, SetLayeredWindowAttributes,
    SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, LWA_ALPHA, MSG, SWP_NOACTIVATE,
    SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WM_LBUTTONUP, WM_PAINT, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::ui::ActiveQuickShare;

const WM_SHOW_SHARE: u32 = WM_APP + 10;
const WM_UPDATE_STATUS: u32 = WM_APP + 11;
const TIMER_PROGRESS: usize = 101;
const TIMER_DISMISS: usize = 102;
const TOTAL_DURATION_MS: u32 = 8000;

static HWND_VAL: AtomicIsize = AtomicIsize::new(0);
static CURRENT_SHARE: OnceLock<Mutex<Option<ShareState>>> = OnceLock::new();

struct ShareState {
    share: ActiveQuickShare,
    started_at: Instant,
    elapsed_ms: u32,
    transferring: bool,
    done: bool,
    error: bool,
}

fn current_share() -> &'static Mutex<Option<ShareState>> {
    CURRENT_SHARE.get_or_init(|| Mutex::new(None))
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Initialize the Windows quick share popup subsystem.
pub fn init() {
    crate::ui::register_quickshare_notifier(|qs| {
        present(qs);
    });
}

/// Show or update the Quick Share popup.
pub fn present(qs: ActiveQuickShare) {
    let mut cur = current_share().lock().unwrap();
    if qs.status == "pending" {
        *cur = Some(ShareState {
            share: qs,
            started_at: Instant::now(),
            elapsed_ms: 0,
            transferring: false,
            done: false,
            error: false,
        });
        drop(cur);
        let h = HWND_VAL.load(Ordering::Relaxed);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_SHOW_SHARE, WPARAM(0), LPARAM(0));
            }
        } else {
            std::thread::Builder::new()
                .name("kayiver-quickshare-win".into())
                .spawn(|| unsafe { run() })
                .ok();
        }
    } else if let Some(state) = cur.as_mut() {
        if qs.status == "done" {
            state.done = true;
            state.transferring = false;
        } else if qs.status == "error" {
            state.error = true;
            state.transferring = false;
        }
        state.share.message = qs.message;
        drop(cur);
        let h = HWND_VAL.load(Ordering::Relaxed);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_UPDATE_STATUS, WPARAM(0), LPARAM(0));
            }
        }
    }
}

unsafe fn run() {
    let Ok(hinst) = GetModuleHandleW(None) else { return };
    let class = to_wide("kayiver_quickshare_class");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    RegisterClassW(&wc);

    let name = to_wide("Kayıver Quick Share");
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
        PCWSTR(class.as_ptr()),
        PCWSTR(name.as_ptr()),
        WS_POPUP,
        0, 0, 360, 84,
        None, None, Some(hinst.into()), None,
    );
    let Ok(hwnd) = hwnd else { return };

    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 244, LWA_ALPHA);
    HWND_VAL.store(hwnd.0 as isize, Ordering::Relaxed);

    position_and_show(hwnd);

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        DispatchMessageW(&msg);
    }
}

unsafe fn position_and_show(hwnd: HWND) {
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    let hmon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(hmon, &mut mi);

    let (w, h) = (360, 84);
    let x = mi.rcWork.right - w - 24;
    let y = mi.rcWork.bottom - h - 36;

    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    let _ = InvalidateRect(Some(hwnd), None, true);

    let _ = SetTimer(Some(hwnd), TIMER_PROGRESS, 50, None);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SHOW_SHARE => {
            position_and_show(hwnd);
            LRESULT(0)
        }
        WM_UPDATE_STATUS => {
            let _ = InvalidateRect(Some(hwnd), None, true);
            let _ = SetTimer(Some(hwnd), TIMER_DISMISS, 2500, None);
            LRESULT(0)
        }
        WM_TIMER => {
            let timer_id = wparam.0;
            if timer_id == TIMER_DISMISS {
                let _ = KillTimer(Some(hwnd), TIMER_DISMISS);
                let _ = KillTimer(Some(hwnd), TIMER_PROGRESS);
                let _ = ShowWindow(hwnd, SW_HIDE);
                *current_share().lock().unwrap() = None;
                return LRESULT(0);
            }

            if timer_id == TIMER_PROGRESS {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let mut win_rc = RECT::default();
                let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut win_rc);

                // Pause timer while mouse hovers over window
                let hovering = pt.x >= win_rc.left && pt.x <= win_rc.right && pt.y >= win_rc.top && pt.y <= win_rc.bottom;

                let mut cur = current_share().lock().unwrap();
                if let Some(state) = cur.as_mut() {
                    if !hovering && !state.transferring && !state.done && !state.error {
                        state.elapsed_ms += 50;
                        if state.elapsed_ms >= TOTAL_DURATION_MS {
                            drop(cur);
                            let _ = KillTimer(Some(hwnd), TIMER_PROGRESS);
                            let _ = ShowWindow(hwnd, SW_HIDE);
                            *current_share().lock().unwrap() = None;
                            return LRESULT(0);
                        }
                    }
                } else {
                    let _ = KillTimer(Some(hwnd), TIMER_PROGRESS);
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    return LRESULT(0);
                }
                drop(cur);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xffff) as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i32;

            // Close button: x: 330..356, y: 6..28
            if x >= 330 && y <= 30 {
                let _ = KillTimer(Some(hwnd), TIMER_PROGRESS);
                let _ = ShowWindow(hwnd, SW_HIDE);
                *current_share().lock().unwrap() = None;
                let _ = crate::ui::send_cmd(crate::ui::UiCmd::QuickShareDismiss);
                return LRESULT(0);
            }

            // Action button: x: 248..330, y: 22..62
            if x >= 244 && x <= 334 && y >= 20 && y <= 64 {
                let mut cur = current_share().lock().unwrap();
                if let Some(state) = cur.as_mut() {
                    match &state.share.payload {
                        QuickSharePayload::Url { url, .. } => {
                            let url = url.clone();
                            drop(cur);
                            crate::platform::open_url(&url);
                            let _ = KillTimer(Some(hwnd), TIMER_PROGRESS);
                            let _ = ShowWindow(hwnd, SW_HIDE);
                            *current_share().lock().unwrap() = None;
                            let _ = crate::ui::send_cmd(crate::ui::UiCmd::QuickShareDismiss);
                            return LRESULT(0);
                        }
                        QuickSharePayload::File { .. } => {
                            if !state.transferring && !state.done {
                                state.transferring = true;
                                let peer = state.share.peer.clone();
                                let id = state.share.id;
                                drop(cur);
                                let _ = crate::ui::send_cmd(crate::ui::UiCmd::QuickShareAccept { peer, id });
                                let _ = InvalidateRect(Some(hwnd), None, true);
                            }
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);

            // Dark card background
            let bg = CreateSolidBrush(COLORREF(0x001C_1818));
            let border_pen = CreatePen(PS_SOLID, 1, COLORREF(0x003A_3636));
            let old_brush = SelectObject(hdc, bg.into());
            let old_pen = SelectObject(hdc, border_pen.into());
            let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, 16, 16);

            let cur = current_share().lock().unwrap();
            if let Some(state) = cur.as_ref() {
                SetBkMode(hdc, TRANSPARENT);

                let is_url = matches!(state.share.payload, QuickSharePayload::Url { .. });

                // Circular icon badge
                let (cx, cy, radius) = (36, 42, 22);
                let (circle_bg_color, border_color) = if is_url {
                    (COLORREF(0x007A_3218), COLORREF(0x00FA_A560)) // Blue gradient fill & border in BGR
                } else {
                    (COLORREF(0x003A_6B12), COLORREF(0x0099_D334)) // Emerald green fill & border in BGR
                };

                let circle_brush = CreateSolidBrush(circle_bg_color);
                let circle_pen = CreatePen(PS_SOLID, 2, border_color);
                let prev_b = SelectObject(hdc, circle_brush.into());
                let prev_p = SelectObject(hdc, circle_pen.into());
                let _ = Ellipse(hdc, cx - radius, cy - radius, cx + radius, cy + radius);
                let _ = SelectObject(hdc, prev_b);
                let _ = SelectObject(hdc, prev_p);
                let _ = DeleteObject(circle_brush.into());
                let _ = DeleteObject(circle_pen.into());

                // Draw vector icon inside circle
                let white_pen = CreatePen(PS_SOLID, 2, COLORREF(0x00FF_FF_FF));
                let old_pen_icon = SelectObject(hdc, white_pen.into());

                if is_url {
                    // Globe: outer circle + equator + central meridian
                    let r = 11;
                    let null_brush = windows::Win32::Graphics::Gdi::GetStockObject(windows::Win32::Graphics::Gdi::NULL_BRUSH);
                    let old_b = SelectObject(hdc, null_brush);
                    let _ = Ellipse(hdc, cx - r, cy - r, cx + r, cy + r);
                    // Equator line
                    let _ = MoveToEx(hdc, cx - r, cy, None);
                    let _ = LineTo(hdc, cx + r, cy);
                    // Central vertical line
                    let _ = MoveToEx(hdc, cx, cy - r, None);
                    let _ = LineTo(hdc, cx, cy + r);
                    // Meridian ellipse
                    let _ = Ellipse(hdc, cx - 5, cy - r, cx + 5, cy + r);
                    let _ = SelectObject(hdc, old_b);
                } else {
                    // Folder icon
                    let white_brush = CreateSolidBrush(COLORREF(0x00FF_FF_FF));
                    let old_b = SelectObject(hdc, white_brush.into());
                    let null_pen = windows::Win32::Graphics::Gdi::GetStockObject(windows::Win32::Graphics::Gdi::NULL_PEN);
                    let _ = SelectObject(hdc, null_pen);

                    // Folder tab + body
                    let tab_pts = [
                        POINT { x: cx - 11, y: cy - 3 },
                        POINT { x: cx - 11, y: cy - 7 },
                        POINT { x: cx - 3, y: cy - 7 },
                        POINT { x: cx, y: cy - 3 },
                    ];
                    let _ = Polygon(hdc, &tab_pts);

                    // Folder main rectangle
                    let _ = RoundRect(hdc, cx - 11, cy - 3, cx + 12, cy + 9, 3, 3);

                    let _ = SelectObject(hdc, old_b);
                    let _ = DeleteObject(white_brush.into());
                }

                let _ = SelectObject(hdc, old_pen_icon);
                let _ = DeleteObject(white_pen.into());

                // Title and details
                let (title_str, detail_str) = match &state.share.payload {
                    QuickSharePayload::Url { url, .. } => {
                        let domain = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(url);
                        (format!("Web Bağlantısı ({})", state.share.peer), domain.to_string())
                    }
                    QuickSharePayload::File { name, size, .. } => {
                        let sz = crate::engine::quickshare::format_size(*size);
                        (format!("Dosya Paylaşımı ({})", state.share.peer), format!("{name} · {sz}"))
                    }
                };

                let mut title_rc = RECT { left: 64, top: 18, right: 240, bottom: 38 };
                SetTextColor(hdc, COLORREF(0x00F5_F4F4));
                let mut title_w = to_wide(&title_str);
                DrawTextW(hdc, &mut title_w, &mut title_rc, DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

                let mut detail_rc = RECT { left: 64, top: 40, right: 240, bottom: 62 };
                SetTextColor(hdc, COLORREF(0x00A0_9B98));
                let mut detail_w = to_wide(&detail_str);
                DrawTextW(hdc, &mut detail_w, &mut detail_rc, DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

                // Action button
                let btn_rc = RECT { left: 248, top: 24, right: 326, bottom: 58 };
                let (btn_txt, btn_color) = if state.done {
                    ("✓ İndirildi", COLORREF(0x0038_A824)) // green
                } else if state.error {
                    ("Hata", COLORREF(0x0024_24D8)) // red
                } else if state.transferring {
                    ("Aktarılıyor…", COLORREF(0x0050_5050))
                } else if is_url {
                    ("Aç", COLORREF(0x00D8_6020)) // blue
                } else {
                    ("Aktar", COLORREF(0x00D8_6020))
                };

                let btn_brush = CreateSolidBrush(btn_color);
                let btn_pen = CreatePen(PS_SOLID, 1, btn_color);
                let _ = SelectObject(hdc, btn_brush.into());
                let _ = SelectObject(hdc, btn_pen.into());
                let _ = RoundRect(hdc, btn_rc.left, btn_rc.top, btn_rc.right, btn_rc.bottom, 10, 10);
                let _ = DeleteObject(btn_brush.into());
                let _ = DeleteObject(btn_pen.into());

                SetTextColor(hdc, COLORREF(0x00FF_FF_FF));
                let mut btn_w = to_wide(btn_txt);
                let mut btn_draw_rc = btn_rc;
                DrawTextW(hdc, &mut btn_w, &mut btn_draw_rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

                // Close button (✕)
                let mut close_rc = RECT { left: 334, top: 8, right: 352, bottom: 26 };
                SetTextColor(hdc, COLORREF(0x0080_8080));
                let mut close_w = to_wide("✕");
                DrawTextW(hdc, &mut close_w, &mut close_rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

                // Progress bar at the bottom
                if !state.transferring && !state.done && !state.error {
                    let pct = 1.0 - (state.elapsed_ms as f32 / TOTAL_DURATION_MS as f32).clamp(0.0, 1.0);
                    let bar_w = ((rc.right - rc.left) as f32 * pct) as i32;
                    let prog_rc = RECT { left: rc.left, top: rc.bottom - 3, right: rc.left + bar_w, bottom: rc.bottom };
                    let prog_brush = CreateSolidBrush(COLORREF(0x00EB_823B));
                    FillRect(hdc, &prog_rc, prog_brush);
                    let _ = DeleteObject(prog_brush.into());
                }
            }

            let _ = SelectObject(hdc, old_brush);
            let _ = SelectObject(hdc, old_pen);
            let _ = DeleteObject(bg.into());
            let _ = DeleteObject(border_pen.into());

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)

        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
