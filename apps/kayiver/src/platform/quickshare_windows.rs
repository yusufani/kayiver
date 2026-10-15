//! Non-activating Quick Share card and temporary circular Kayiver mark.
//! Paint into an offscreen bitmap and present once: never erase a visible frame.
#![allow(non_snake_case)]

use crate::ui::ActiveQuickShare;
use kayiver_core::proto::QuickSharePayload;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_SHOW_SHARE: u32 = WM_APP + 10;
const WM_UPDATE_STATUS: u32 = WM_APP + 11;
const TIMER_TICK: usize = 101;
const OFFER_DURATION_MS: u32 = 10_000;
const WIDTH: i32 = 384;
const HEIGHT: i32 = 202;
const CARD_HEIGHT: i32 = 132;
const MARK: (i32, i32, i32) = (354, 174, 26);
const ACTION: RECT = RECT {
    left: 268,
    top: 82,
    right: 366,
    bottom: 116,
};
const CLOSE: RECT = RECT {
    left: 346,
    top: 10,
    right: 374,
    bottom: 38,
};
static POPUP_SCALE: AtomicU32 = AtomicU32::new(1000);
static HWND_VAL: AtomicIsize = AtomicIsize::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
static CURRENT_SHARE: OnceLock<Mutex<Option<ShareState>>> = OnceLock::new();

#[derive(Clone)]
struct ShareState {
    share: ActiveQuickShare,
    remaining_ms: u32,
    last_tick: Instant,
    transferring: bool,
    done: bool,
    error: bool,
    hovered_action: bool,
    hovered_close: bool,
}
impl ShareState {
    fn new(share: ActiveQuickShare) -> Self {
        Self {
            share,
            remaining_ms: OFFER_DURATION_MS,
            last_tick: Instant::now(),
            transferring: false,
            done: false,
            error: false,
            hovered_action: false,
            hovered_close: false,
        }
    }
    fn matches(&self, share: &ActiveQuickShare) -> bool {
        self.share.id == share.id && self.share.peer == share.peer
    }
    fn update(&mut self, share: ActiveQuickShare) -> bool {
        // Duplicate offers must not replay the popup or reset its lifetime.
        // A late receipt from the previous offer must not replace the new one.
        if !self.matches(&share) || share.status == "pending" {
            return false;
        }
        let done = share.status == "done";
        let error = share.status == "error";
        if (!done && !error) || (self.done == done && self.error == error) {
            return false;
        }
        self.done = done;
        self.error = error;
        self.transferring = false;
        self.remaining_ms = if error { 6000 } else { 3000 };
        self.last_tick = Instant::now();
        self.share = share;
        true
    }
    fn tick(&mut self, now: Instant, hovering: bool) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last_tick)
            .as_millis()
            .min(u32::MAX as u128) as u32;
        self.last_tick = now;
        if !hovering && !self.transferring {
            self.remaining_ms = self.remaining_ms.saturating_sub(elapsed);
        }
        self.remaining_ms == 0
    }
}
fn current_share() -> &'static Mutex<Option<ShareState>> {
    CURRENT_SHARE.get_or_init(|| Mutex::new(None))
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}
fn inside(r: RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}
fn in_shape(x: i32, y: i32) -> bool {
    (x >= 0 && x < WIDTH && y >= 0 && y < CARD_HEIGHT)
        || (x - MARK.0).pow(2) + (y - MARK.1).pow(2) <= MARK.2.pow(2)
}

pub fn init() {
    crate::ui::register_quickshare_notifier(present);
}
pub fn present(qs: ActiveQuickShare) {
    let mut cur = current_share().lock().unwrap();
    let message = if qs.status == "pending" {
        if cur.as_ref().is_some_and(|s| s.matches(&qs)) {
            return;
        }
        *cur = Some(ShareState::new(qs));
        WM_SHOW_SHARE
    } else {
        if !cur.as_mut().is_some_and(|s| s.update(qs)) {
            return;
        }
        WM_UPDATE_STATUS
    };
    drop(cur);
    let h = HWND_VAL.load(Ordering::Acquire);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), message, WPARAM(0), LPARAM(0));
        }
    } else if STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        if std::thread::Builder::new()
            .name("kayiver-quickshare-win".into())
            .spawn(|| {
                unsafe {
                    run();
                }
                HWND_VAL.store(0, Ordering::Release);
                STARTED.store(false, Ordering::Release);
            })
            .is_err()
        {
            STARTED.store(false, Ordering::Release);
        }
    }
}
unsafe fn create_window() -> windows::core::Result<HWND> {
    let hinst = GetModuleHandleW(None)?;
    let class = wide("kayiver_quickshare_class");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    RegisterClassW(&wc);
    let name = wide("Kayıver Quick Share");
    // The window region clips the transparent gap; a color-keyed layered
    // surface is unnecessary and exposes intermediate GDI draws to DWM.
    CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        PCWSTR(class.as_ptr()),
        PCWSTR(name.as_ptr()),
        WS_POPUP,
        0,
        0,
        WIDTH,
        HEIGHT,
        None,
        None,
        Some(hinst.into()),
        None,
    )
}
unsafe fn run() {
    let Ok(hwnd) = create_window() else {
        return;
    };
    HWND_VAL.store(hwnd.0 as isize, Ordering::Release);
    position_and_show(hwnd);
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
        let _ = TranslateMessage(&msg);
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
    if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
        return;
    }
    let (mut dpi_x, mut dpi_y) = (96, 96);
    let _ = windows::Win32::UI::HiDpi::GetDpiForMonitor(
        hmon,
        windows::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI,
        &mut dpi_x,
        &mut dpi_y,
    );
    let scale = (dpi_x as f64 / 96.0)
        .min((mi.rcWork.right - mi.rcWork.left - 24).max(1) as f64 / WIDTH as f64)
        .min((mi.rcWork.bottom - mi.rcWork.top - 24).max(1) as f64 / HEIGHT as f64)
        .max(0.1);
    POPUP_SCALE.store((scale * 1000.0).round() as u32, Ordering::Relaxed);
    let px = |v: i32| (v as f64 * scale).round() as i32;
    let (w, h) = (px(WIDTH), px(HEIGHT));
    let region = CreateRoundRectRgn(0, 0, w, px(CARD_HEIGHT), px(22), px(22));
    let circle = CreateEllipticRgn(
        px(MARK.0 - MARK.2),
        px(MARK.1 - MARK.2),
        px(MARK.0 + MARK.2),
        px(MARK.1 + MARK.2),
    );
    if !region.0.is_null() && !circle.0.is_null() {
        if CombineRgn(Some(region), Some(region), Some(circle), RGN_OR).0 == 0
            || SetWindowRgn(hwnd, Some(region), false) == 0
        {
            let _ = DeleteObject(region.into());
        }
    } else if !region.0.is_null() {
        let _ = DeleteObject(region.into());
    }
    if !circle.0.is_null() {
        let _ = DeleteObject(circle.into());
    }
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOPMOST),
        mi.rcWork.right - w - px(16),
        mi.rcWork.bottom - h - px(16),
        w,
        h,
        SWP_NOACTIVATE,
    );
    if let Some(state) = current_share().lock().unwrap().as_mut() {
        state.last_tick = Instant::now();
    }
    let _ = InvalidateRect(Some(hwnd), None, false);
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    SetTimer(Some(hwnd), TIMER_TICK, 50, None);
}
unsafe fn dismiss(hwnd: HWND) {
    let _ = KillTimer(Some(hwnd), TIMER_TICK);
    let _ = ShowWindow(hwnd, SW_HIDE);
    *current_share().lock().unwrap() = None;
    crate::ui::dismiss_quick_share();
}
unsafe fn cursor_in_popup(hwnd: HWND) -> (i32, i32) {
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let mut rc = RECT::default();
    let _ = GetWindowRect(hwnd, &mut rc);
    let scale = POPUP_SCALE.load(Ordering::Relaxed) as f64 / 1000.0;
    (
        ((pt.x - rc.left) as f64 / scale) as i32,
        ((pt.y - rc.top) as f64 / scale) as i32,
    )
}
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1), // WM_PAINT supplies a complete buffered frame.
        WM_SHOW_SHARE => {
            position_and_show(hwnd);
            LRESULT(0)
        }
        WM_UPDATE_STATUS => {
            let _ = InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let (x, y) = cursor_in_popup(hwnd);
            let actionable = current_share()
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|s| !s.transferring && !s.done && !s.error);
            let _ = SetCursor(
                LoadCursorW(
                    None,
                    if inside(CLOSE, x, y) || (actionable && inside(ACTION, x, y)) {
                        IDC_HAND
                    } else {
                        IDC_ARROW
                    },
                )
                .ok(),
            );
            LRESULT(1)
        }
        WM_TIMER if wparam.0 == TIMER_TICK => {
            let (x, y) = cursor_in_popup(hwnd);
            let mut cur = current_share().lock().unwrap();
            let Some(state) = cur.as_mut() else {
                drop(cur);
                dismiss(hwnd);
                return LRESULT(0);
            };
            let old = (
                state.remaining_ms,
                state.hovered_action,
                state.hovered_close,
            );
            state.hovered_action = inside(ACTION, x, y);
            state.hovered_close = inside(CLOSE, x, y);
            let expired = state.tick(Instant::now(), in_shape(x, y));
            let changed = old.1 != state.hovered_action
                || old.2 != state.hovered_close
                || (!state.done
                    && !state.error
                    && !state.transferring
                    && old.0 != state.remaining_ms);
            drop(cur);
            if expired {
                dismiss(hwnd);
            } else if changed {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let scale = POPUP_SCALE.load(Ordering::Relaxed) as f64 / 1000.0;
            let x = ((lparam.0 & 0xffff) as i16 as f64 / scale) as i32;
            let y = (((lparam.0 >> 16) & 0xffff) as i16 as f64 / scale) as i32;
            if inside(CLOSE, x, y) {
                dismiss(hwnd);
                return LRESULT(0);
            }
            if inside(ACTION, x, y) {
                let mut cur = current_share().lock().unwrap();
                if let Some(state) = cur.as_mut() {
                    if state.transferring || state.done || state.error {
                        return LRESULT(0);
                    }
                    match &state.share.payload {
                        QuickSharePayload::Url { url, .. } => {
                            let url = url.clone();
                            drop(cur);
                            dismiss(hwnd);
                            crate::platform::open_url(&url);
                        }
                        QuickSharePayload::File { .. } => {
                            let (peer, id) = (state.share.peer.clone(), state.share.id);
                            state.transferring = true;
                            drop(cur);
                            if !crate::ui::send_cmd(crate::ui::UiCmd::QuickShareAccept { peer, id })
                            {
                                let mut cur = current_share().lock().unwrap();
                                if let Some(state) = cur.as_mut() {
                                    state.transferring = false;
                                    state.error = true;
                                    state.share.message = Some("Bağlantı hazır değil".into());
                                    state.remaining_ms = 6000;
                                }
                            }
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let target = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let state = current_share().lock().unwrap().clone();
            if let Some(state) = state {
                paint_buffered(target, rc.right, rc.bottom, &state);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// The screen DC sees exactly one completed frame, including DPI scaling.
unsafe fn paint_buffered(target: HDC, width: i32, height: i32, state: &ShareState) {
    if width <= 0 || height <= 0 {
        return;
    }
    let dc = CreateCompatibleDC(Some(target));
    if dc.0.is_null() {
        return;
    }
    let bitmap = CreateCompatibleBitmap(target, width, height);
    if bitmap.0.is_null() {
        let _ = DeleteDC(dc);
        return;
    }
    let old = SelectObject(dc, bitmap.into());
    let saved = SaveDC(dc);
    SetMapMode(dc, MM_ANISOTROPIC);
    let _ = SetWindowExtEx(dc, WIDTH, HEIGHT, None);
    let _ = SetViewportExtEx(dc, width, height, None);
    draw_card(dc, state);
    let _ = RestoreDC(dc, saved);
    let _ = BitBlt(target, 0, 0, width, height, Some(dc), 0, 0, SRCCOPY);
    SelectObject(dc, old);
    let _ = DeleteObject(bitmap.into());
    let _ = DeleteDC(dc);
}
unsafe fn rounded(dc: HDC, r: RECT, radius: i32, fill: COLORREF, border: COLORREF) {
    let brush = CreateSolidBrush(fill);
    let pen = CreatePen(PS_SOLID, 1, border);
    let old_b = SelectObject(dc, brush.into());
    let old_p = SelectObject(dc, pen.into());
    let _ = RoundRect(dc, r.left, r.top, r.right, r.bottom, radius, radius);
    SelectObject(dc, old_b);
    SelectObject(dc, old_p);
    let _ = DeleteObject(brush.into());
    let _ = DeleteObject(pen.into());
}
unsafe fn label(
    dc: HDC,
    text: &str,
    mut r: RECT,
    size: i32,
    weight: i32,
    color: COLORREF,
    flags: DRAW_TEXT_FORMAT,
) {
    let name = wide("Segoe UI");
    let font = CreateFontW(
        -size,
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
        PCWSTR(name.as_ptr()),
    );
    let old = SelectObject(dc, font.into());
    SetTextColor(dc, color);
    SetBkMode(dc, TRANSPARENT);
    let mut text: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(
        dc,
        &mut text,
        &mut r,
        flags | DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
    );
    SelectObject(dc, old);
    let _ = DeleteObject(font.into());
}
unsafe fn draw_card(dc: HDC, s: &ShareState) {
    let ink = rgb(237, 243, 250);
    let muted = rgb(151, 166, 185);
    let bg = rgb(19, 27, 39);
    let border = rgb(54, 68, 87);
    let accent = rgb(99, 168, 255);
    let clear = CreateSolidBrush(bg);
    FillRect(
        dc,
        &RECT {
            left: 0,
            top: 0,
            right: WIDTH,
            bottom: HEIGHT,
        },
        clear,
    );
    let _ = DeleteObject(clear.into());
    rounded(
        dc,
        RECT {
            left: 0,
            top: 0,
            right: WIDTH,
            bottom: CARD_HEIGHT,
        },
        22,
        bg,
        border,
    );
    label(
        dc,
        "KAYIVER  /  HIZLI PAYLAŞIM",
        RECT {
            left: 18,
            top: 12,
            right: 340,
            bottom: 34,
        },
        10,
        600,
        rgb(102, 211, 186),
        DT_LEFT,
    );
    if s.hovered_close {
        rounded(dc, CLOSE, 12, rgb(42, 54, 71), rgb(42, 54, 71));
    }
    label(
        dc,
        "×",
        CLOSE,
        21,
        400,
        if s.hovered_close { ink } else { muted },
        DT_CENTER,
    );
    let (name, subtitle) = match &s.share.payload {
        QuickSharePayload::Url { url, title } => {
            let domain = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
                .unwrap_or(url);
            (
                title
                    .as_deref()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or(domain)
                    .to_string(),
                "Web bağlantısı".to_string(),
            )
        }
        QuickSharePayload::File { name, size, .. } => (
            name.clone(),
            format!("{} · Dosya", crate::engine::quickshare::format_size(*size)),
        ),
    };
    label(
        dc,
        &name,
        RECT {
            left: 18,
            top: 37,
            right: 366,
            bottom: 63,
        },
        16,
        600,
        ink,
        DT_LEFT,
    );
    label(
        dc,
        &format!("{} · {}", s.share.peer, subtitle),
        RECT {
            left: 18,
            top: 63,
            right: 366,
            bottom: 81,
        },
        11,
        400,
        muted,
        DT_LEFT,
    );
    let (status, button, color) = if s.done {
        (
            "İndirilenler’e kaydedildi".to_string(),
            "Kaydedildi",
            rgb(85, 214, 174),
        )
    } else if s.error {
        (
            s.share
                .message
                .clone()
                .unwrap_or_else(|| "Aktarım tamamlanamadı".into()),
            "Başarısız",
            rgb(255, 135, 145),
        )
    } else if s.transferring {
        ("Güvenli bağlantıyla alınıyor…".into(), "Alınıyor…", muted)
    } else if matches!(s.share.payload, QuickSharePayload::Url { .. }) {
        ("Tarayıcıda açmaya hazır".into(), "Aç  ↗", accent)
    } else {
        ("İndirilenler’e kaydet".into(), "Kaydet  ↓", accent)
    };
    label(
        dc,
        &status,
        RECT {
            left: 18,
            top: 88,
            right: 258,
            bottom: 111,
        },
        11,
        400,
        if s.error { color } else { muted },
        DT_LEFT,
    );
    let actionable = !s.done && !s.error && !s.transferring;
    let button_bg = if actionable {
        if s.hovered_action {
            rgb(136, 189, 255)
        } else {
            accent
        }
    } else {
        rgb(31, 44, 60)
    };
    rounded(dc, ACTION, 12, button_bg, button_bg);
    label(
        dc,
        button,
        ACTION,
        12,
        600,
        if actionable { rgb(10, 28, 50) } else { color },
        DT_CENTER,
    );
    if !s.done && !s.error && !s.transferring {
        // This line is expiry, never fabricated file-transfer progress.
        let fraction = s.remaining_ms as f64 / OFFER_DURATION_MS as f64;
        let track = RECT {
            left: 18,
            top: 126,
            right: WIDTH - 18,
            bottom: 128,
        };
        let brush = CreateSolidBrush(rgb(36, 50, 67));
        FillRect(dc, &track, brush);
        let _ = DeleteObject(brush.into());
        let brush = CreateSolidBrush(rgb(71, 111, 143));
        FillRect(
            dc,
            &RECT {
                right: track.left + ((track.right - track.left) as f64 * fraction).round() as i32,
                ..track
            },
            brush,
        );
        let _ = DeleteObject(brush.into());
    }
    // Circular, temporary Kayiver brand anchor, with a portal and cursor trail.
    let (cx, cy, r) = MARK;
    let b = CreateSolidBrush(bg);
    let p = CreatePen(PS_SOLID, 1, border);
    let ob = SelectObject(dc, b.into());
    let op = SelectObject(dc, p.into());
    let _ = Ellipse(dc, cx - r, cy - r, cx + r, cy + r);
    let portal = CreatePen(PS_SOLID, 2, rgb(94, 234, 212));
    SelectObject(dc, portal.into());
    let _ = MoveToEx(dc, cx - 3, cy - 16, None);
    let _ = LineTo(dc, cx - 3, cy + 16);
    SelectObject(dc, op);
    let _ = DeleteObject(portal.into());
    for (offset, color) in [(-15, rgb(38, 87, 85)), (-10, rgb(54, 139, 126)), (2, ink)] {
        let cursor_b = CreateSolidBrush(color);
        let cursor_p = CreatePen(PS_SOLID, 1, color);
        SelectObject(dc, cursor_b.into());
        SelectObject(dc, cursor_p.into());
        let points = [
            POINT {
                x: cx + offset,
                y: cy - 9,
            },
            POINT {
                x: cx + offset,
                y: cy + 8,
            },
            POINT {
                x: cx + offset + 5,
                y: cy + 3,
            },
            POINT {
                x: cx + offset + 9,
                y: cy + 11,
            },
            POINT {
                x: cx + offset + 12,
                y: cy + 9,
            },
            POINT {
                x: cx + offset + 8,
                y: cy + 2,
            },
            POINT {
                x: cx + offset + 15,
                y: cy + 2,
            },
        ];
        let _ = Polygon(dc, &points);
        SelectObject(dc, ob);
        SelectObject(dc, op);
        let _ = DeleteObject(cursor_b.into());
        let _ = DeleteObject(cursor_p.into());
    }
    SelectObject(dc, ob);
    SelectObject(dc, op);
    let _ = DeleteObject(b.into());
    let _ = DeleteObject(p.into());
}

#[cfg(test)]
mod popup_tests {
    use super::*;
    fn offer(id: u64) -> ActiveQuickShare {
        ActiveQuickShare {
            peer: "Studio PC".into(),
            id,
            payload: QuickSharePayload::File {
                name: "Tasarım dosyaları +3.zip".into(),
                size: 12_884_902,
                source_path: String::new(),
            },
            status: "pending".into(),
            message: None,
        }
    }
    #[test]
    fn lifetime_pauses_for_hover_and_transfer_including_completion() {
        let mut s = ShareState::new(offer(1));
        let now = s.last_tick;
        assert!(!s.tick(now + std::time::Duration::from_secs(20), true));
        assert_eq!(s.remaining_ms, OFFER_DURATION_MS);
        s.transferring = true;
        assert!(!s.tick(now + std::time::Duration::from_secs(40), false));
        let mut done = offer(1);
        done.status = "done".into();
        assert!(s.update(done));
        let now = s.last_tick;
        assert!(!s.tick(now + std::time::Duration::from_secs(20), true));
        assert!(s.tick(now + std::time::Duration::from_secs(24), false));
    }
    #[test]
    fn duplicate_and_stale_notifications_do_not_replay_or_replace() {
        let mut s = ShareState::new(offer(2));
        s.remaining_ms = 1234;
        assert!(!s.update(offer(2)));
        assert_eq!(s.remaining_ms, 1234);
        let mut old = offer(1);
        old.status = "done".into();
        assert!(!s.update(old));
        let mut foreign = offer(2);
        foreign.peer = "other".into();
        foreign.status = "error".into();
        assert!(!s.update(foreign));
        assert!(!s.done && !s.error);
    }
    #[test]
    fn bubble_mark_and_gap_have_separate_hit_targets() {
        assert!(in_shape(20, 60));
        assert!(in_shape(MARK.0, MARK.1));
        assert!(!in_shape(20, 175));
        assert!(!in_shape(354, 141));
        assert!(!inside(CLOSE, 335, 48));
        assert!(!inside(ACTION, 265, 100));
    }
}

#[cfg(test)]
mod native_popup_tests {
    use super::*;
    use windows::Win32::System::Threading::{GetGuiResources, GR_GDIOBJECTS};
    #[test]
    fn buffered_frames_and_window_lifecycle() {
        unsafe {
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let mut info = BITMAPINFO::default();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = WIDTH * 2;
            info.bmiHeader.biHeight = -HEIGHT * 2;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            let mut bits = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(dc, bitmap.into());
            let base = ActiveQuickShare {
                peer: "MacBook Pro".into(),
                id: 9001,
                payload: QuickSharePayload::File {
                    name: "Tasarım dosyaları +3.zip".into(),
                    size: 12_884_902,
                    source_path: String::new(),
                },
                status: "pending".into(),
                message: None,
            };
            let dir = std::env::var_os("KAYIVER_POPUP_RENDER_DIR").map(std::path::PathBuf::from);
            if let Some(dir) = &dir {
                std::fs::create_dir_all(dir).unwrap();
            }
            let mut state = ShareState::new(base.clone());
            for kind in ["file", "url", "transfer", "done", "error", "long-name"] {
                state.share = base.clone();
                state.transferring = false;
                state.done = false;
                state.error = false;
                match kind {
                    "url" => {
                        state.share.payload = QuickSharePayload::Url {
                            url: "https://example.com/project/design-review".into(),
                            title: None,
                        }
                    }
                    "transfer" => state.transferring = true,
                    "done" => state.done = true,
                    "error" => {
                        state.error = true;
                        state.share.message =
                            Some("Bağlantı kesildi. Yeniden kopyalayıp deneyin.".into());
                    }
                    "long-name" => {
                        state.share.peer = "Engineering Studio — Windows Workstation".into();
                        state.share.payload = QuickSharePayload::File { name: "Çok uzun bir klasör adı — tasarım gözden geçirme belgeleri ve görseller +12.zip".into(), size: 134_217_728, source_path: String::new() };
                    }
                    _ => {}
                }
                for scale in [1.0, 1.25, 2.0] {
                    let w = (WIDTH as f64 * scale) as i32;
                    let h = (HEIGHT as f64 * scale) as i32;
                    paint_buffered(dc, w, h, &state);
                    GdiFlush().ok().unwrap();
                    assert_ne!(GetPixel(dc, (w as f64 * 0.5) as i32, 10), COLORREF(0));
                    if let Some(dir) = &dir {
                        let source = std::slice::from_raw_parts(
                            bits as *const u8,
                            (WIDTH * 2 * HEIGHT * 2 * 4) as usize,
                        );
                        let mut rgba = Vec::new();
                        for y in 0..h {
                            for x in 0..w {
                                let idx = ((y * WIDTH * 2 + x) * 4) as usize;
                                let lx = x as f64 / scale;
                                let ly = y as f64 / scale;
                                let card =
                                    lx >= 0.0 && lx < WIDTH as f64 && ly < CARD_HEIGHT as f64;
                                let mark = (lx - MARK.0 as f64).powi(2)
                                    + (ly - MARK.1 as f64).powi(2)
                                    < (MARK.2 as f64).powi(2);
                                rgba.extend_from_slice(&[
                                    source[idx + 2],
                                    source[idx + 1],
                                    source[idx],
                                    if card || mark { 255 } else { 0 },
                                ]);
                            }
                        }
                        let f = std::fs::File::create(
                            dir.join(format!("popup-{kind}-{}.png", (scale * 100.0) as u32)),
                        )
                        .unwrap();
                        let mut encoder = png::Encoder::new(f, w as u32, h as u32);
                        encoder.set_color(png::ColorType::Rgba);
                        encoder.set_depth(png::BitDepth::Eight);
                        encoder
                            .write_header()
                            .unwrap()
                            .write_image_data(&rgba)
                            .unwrap();
                    }
                }
            }
            // Exercise the real paint loop repeatedly and catch selected-object leaks.
            let process = windows::Win32::System::Threading::GetCurrentProcess();
            let before = GetGuiResources(process, GR_GDIOBJECTS);
            for _ in 0..300 {
                paint_buffered(dc, WIDTH, HEIGHT, &state);
            }
            assert!(
                GetGuiResources(process, GR_GDIOBJECTS) <= before + 2,
                "GDI objects leaked while repainting"
            );
            SelectObject(dc, old);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);

            // Only creates our own popup. No mouse, keys, clipboard or other apps are touched.
            let previous_focus = GetForegroundWindow();
            let hwnd = create_window().unwrap();
            *current_share().lock().unwrap() = Some(ShareState::new(base));
            position_and_show(hwnd);
            let _ = UpdateWindow(hwnd);
            assert_eq!(GetForegroundWindow(), previous_focus, "popup stole focus");
            assert!(IsWindowVisible(hwnd).as_bool());
            assert_eq!(
                SendMessageW(hwnd, WM_MOUSEACTIVATE, None, None).0,
                MA_NOACTIVATE as isize
            );
            assert_eq!(SendMessageW(hwnd, WM_ERASEBKGND, None, None).0, 1);
            let region = CreateRectRgn(0, 0, 0, 0);
            assert_ne!(GetWindowRgn(hwnd, region).0, 0);
            let scale = POPUP_SCALE.load(Ordering::Relaxed) as f64 / 1000.0;
            assert!(
                !PtInRegion(region, (20.0 * scale) as i32, (175.0 * scale) as i32).as_bool(),
                "gap intercepts clicks"
            );
            let _ = DeleteObject(region.into());
            let mut bounds = RECT::default();
            GetWindowRect(hwnd, &mut bounds).unwrap();
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            assert!(GetMonitorInfoW(mon, &mut mi).as_bool());
            assert!(
                bounds.left >= mi.rcWork.left
                    && bounds.top >= mi.rcWork.top
                    && bounds.right <= mi.rcWork.right
                    && bounds.bottom <= mi.rcWork.bottom,
                "popup overlaps taskbar or leaves work area"
            );
            dismiss(hwnd);
            assert!(!IsWindowVisible(hwnd).as_bool());
            assert!(current_share().lock().unwrap().is_none());
            DestroyWindow(hwnd).unwrap();
        }
    }
}
