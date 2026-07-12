//! Windows backend.
//!
//! Capture: WH_MOUSE_LL / WH_KEYBOARD_LL hooks on a dedicated thread with a
//! message pump. While forwarding, hook procs return 1 to swallow events, so
//! the physical cursor never moves; each blocked WM_MOUSEMOVE still reports
//! the *proposed* position, and the delta against the parked position is the
//! raw motion we forward. The cursor is parked a safe inset away from the
//! portal edge so proposed positions are never clamped by the screen bounds.
//!
//! Injection: SendInput with MOUSEEVENTF_ABSOLUTE|VIRTUALDESK for motion
//! (normalized to the virtual desktop) and VK+scancode pairs for keys.

#![allow(clippy::missing_safety_doc)]

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use drift_core::layout::{ratio_on_edge, touches_edge, Edge};
use drift_core::proto::{InputEvent, MouseButton, Rect};
use tokio::sync::mpsc::UnboundedSender;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetCursorPos, GetMessageW, GetSystemMetrics, SetCursorPos, SetWindowsHookExW,
    KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
    WM_XBUTTONUP,
};

use crate::engine::Captured;
use crate::keymap;
use crate::platform::CaptureCtl;

const LLMHF_INJECTED: u32 = 0x1;
const LLKHF_INJECTED: u32 = 0x10;
/// Park the cursor this far inside the portal edge while forwarding, so
/// proposed positions in the hook are never clamped by the desktop bounds.
const PARK_INSET: i32 = 300;

pub fn desktop_bounds() -> Rect {
    unsafe {
        Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

// --------------------------------------------------------------- DDC/CI ----

/// Enumerate physical monitors and read each one's current input source
/// (VCP 0x60). Index is a stable 0-based order across the call.
pub fn displays() -> Vec<(u32, String, Option<u16>)> {
    with_physical_monitors(|mons| {
        use windows::Win32::Devices::Display::GetVCPFeatureAndVCPFeatureReply;
        let mut out = Vec::new();
        for (i, pm) in mons.iter().enumerate() {
            // PHYSICAL_MONITOR is packed; copy the name array out unaligned.
            let desc: [u16; 128] = unsafe { std::ptr::addr_of!(pm.szPhysicalMonitorDescription).read_unaligned() };
            let name = String::from_utf16_lossy(&desc)
                .trim_end_matches('\0')
                .trim()
                .to_string();
            let mut cur: u32 = 0;
            let mut max: u32 = 0;
            let ok = unsafe { GetVCPFeatureAndVCPFeatureReply(pm.hPhysicalMonitor, 0x60, None, &mut cur, Some(&mut max)) };
            out.push((i as u32, name, if ok != 0 { Some(cur as u16) } else { None }));
        }
        out
    })
    .unwrap_or_default()
}

/// Set the input source (VCP 0x60) of a physical monitor by 0-based index.
/// Retried a few times — Samsung DDC intermittently ignores writes.
pub fn set_display_input(index: u32, value: u16) -> Result<()> {
    with_physical_monitors(|mons| {
        use windows::Win32::Devices::Display::SetVCPFeature;
        let pm = mons.get(index as usize).ok_or_else(|| anyhow::anyhow!("display index {index} out of range"))?;
        let mut ok = false;
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            if unsafe { SetVCPFeature(pm.hPhysicalMonitor, 0x60, value as u32) } != 0 {
                ok = true;
                break;
            }
        }
        anyhow::ensure!(ok, "SetVCPFeature failed after retries");
        Ok(())
    })
    .unwrap_or_else(|| anyhow::bail!("no physical monitors"))
}

/// Run `f` with the list of physical monitors, cleaning them up after.
fn with_physical_monitors<T>(f: impl FnOnce(&[windows::Win32::Devices::Display::PHYSICAL_MONITOR]) -> T) -> Option<T> {
    use windows::Win32::Devices::Display::{
        DestroyPhysicalMonitors, GetNumberOfPhysicalMonitorsFromHMONITOR,
        GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR,
    };
    use windows::core::BOOL;
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};

    unsafe extern "system" fn cb(m: HMONITOR, _dc: HDC, _rc: *mut RECT, out: LPARAM) -> BOOL {
        (*(out.0 as *mut Vec<HMONITOR>)).push(m);
        BOOL(1)
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut handles as *mut _ as isize));
    }
    let mut all: Vec<PHYSICAL_MONITOR> = Vec::new();
    for h in handles {
        let mut n: u32 = 0;
        if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n) }.is_err() || n == 0 {
            continue;
        }
        let mut v = vec![PHYSICAL_MONITOR::default(); n as usize];
        if unsafe { GetPhysicalMonitorsFromHMONITOR(h, &mut v) }.is_ok() {
            all.extend(v);
        }
    }
    if all.is_empty() {
        return None;
    }
    let result = f(&all);
    unsafe {
        let _ = DestroyPhysicalMonitors(&all);
    }
    Some(result)
}

/// Every physical display, in virtual-screen coordinates.
pub fn monitors() -> Vec<Rect> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};

    unsafe extern "system" fn cb(_m: HMONITOR, _dc: HDC, rc: *mut RECT, out: LPARAM) -> BOOL {
        let list = &mut *(out.0 as *mut Vec<Rect>);
        let r = &*rc;
        list.push(Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top });
        BOOL(1)
    }

    let mut list: Vec<Rect> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut list as *mut _ as isize));
    }
    if list.is_empty() {
        list.push(desktop_bounds());
    }
    list
}

/// Make the process per-monitor DPI aware, so `GetSystemMetrics`,
/// `EnumDisplayMonitors` and `SendInput` all speak the same (physical) pixel
/// coordinates. Without this, on a scaled display (125/150/175%) the geometry
/// we read and the coordinates SendInput expects disagree, and the injected
/// cursor lands in the wrong place. Must run before any geometry is read.
pub fn init() {
    use windows::Win32::System::StationsAndDesktops::{
        OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS,
    };
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        // Attach this thread to the session's *input* desktop. Without this,
        // a drift launched from a service / scheduled task runs on a
        // non-interactive desktop and its SendInput never reaches the visible
        // cursor — even though everything reports success. The client injects
        // on this same thread (single-threaded tokio runtime), so binding it
        // here is what makes remote-launched drift actually move the cursor.
        // GENERIC_ALL = 0x10000000.
        match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(0x1000_0000)) {
            Ok(hdesk) => {
                let ok = SetThreadDesktop(hdesk).is_ok();
                tracing::info!("input-desktop attach: opened=true set_thread_desktop={ok}");
            }
            Err(e) => tracing::info!("input-desktop attach: OpenInputDesktop failed: {e:?}"),
        }
    }
}

pub fn ensure_permissions() -> Result<()> {
    Ok(()) // no special permissions needed on Windows
}

/// Launch `drift run` inside the active console session on the visible input
/// desktop. Must be called from a process running as SYSTEM (a scheduled task
/// with the SYSTEM principal) — that is the only way to obtain the logged-in
/// user's token and start a process that can actually inject input. This is
/// how a remotely-triggered launch reaches the user's real desktop.
pub fn launch_in_active_session() -> Result<()> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
    use windows::Win32::System::Threading::{
        CreateProcessAsUserW, CREATE_UNICODE_ENVIRONMENT, NORMAL_PRIORITY_CLASS, PROCESS_INFORMATION,
        STARTUPINFOW,
    };

    unsafe {
        let session = WTSGetActiveConsoleSessionId();
        anyhow::ensure!(session != 0xFFFF_FFFF, "no active console session (no one logged in)");

        let mut token = HANDLE::default();
        WTSQueryUserToken(session, &mut token)
            .map_err(|e| anyhow::anyhow!("WTSQueryUserToken failed (must run as SYSTEM): {e:?}"))?;

        let mut env: *mut std::ffi::c_void = std::ptr::null_mut();
        let _ = CreateEnvironmentBlock(&mut env, Some(token), false);

        let exe = std::env::current_exe()?;
        // Subcommand to launch in-session (default "run"; overridable for
        // diagnostics via DRIFT_LAUNCH_ARGS).
        let args = std::env::var("DRIFT_LAUNCH_ARGS").unwrap_or_else(|_| "run".into());
        let mut cmd: Vec<u16> = format!("\"{}\" {args}", exe.display())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut desktop: Vec<u16> = "winsta0\\default\0".encode_utf16().collect();

        let mut si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();

        let res = CreateProcessAsUserW(
            Some(token),
            None,
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | NORMAL_PRIORITY_CLASS,
            Some(env),
            None,
            &si as *const _ as *const STARTUPINFOW as *mut _,
            &mut pi,
        );

        if !env.is_null() {
            let _ = DestroyEnvironmentBlock(env);
        }
        let _ = CloseHandle(token);

        res.map_err(|e| anyhow::anyhow!("CreateProcessAsUserW failed: {e:?}"))?;
        let _ = CloseHandle(pi.hProcess);
        let _ = CloseHandle(pi.hThread);
        // Silence unused warning for si (used via raw pointer above).
        let _ = &mut si;
        Ok(())
    }
}

pub fn doctor_permissions() {
    println!("  permissions : none required on Windows");
}

pub fn warp_cursor(x: i32, y: i32) {
    unsafe {
        let _ = SetCursorPos(x, y);
    }
    if let Some(s) = STATE.get() {
        *s.park.lock().unwrap() = (x, y);
    }
}

#[allow(dead_code)]
pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

pub fn set_forwarding_visuals(_on: bool) {
    // The hook swallows all motion, so the cursor simply stays parked.
    // Truly hiding a cursor owned by other processes needs an overlay
    // window; tracked in ROADMAP.
}

// ------------------------------------------------------------- capture ----

struct CapState {
    ctl: Arc<CaptureCtl>,
    tx: UnboundedSender<Captured>,
    /// Where the physical cursor is parked while forwarding; deltas are
    /// computed against this point.
    park: Mutex<(i32, i32)>,
    esc_downs: Mutex<[Option<Instant>; 2]>,
}

static STATE: OnceLock<CapState> = OnceLock::new();

pub fn start_capture(ctl: Arc<CaptureCtl>, tx: UnboundedSender<Captured>) -> Result<()> {
    if STATE
        .set(CapState { ctl, tx, park: Mutex::new((0, 0)), esc_downs: Mutex::new([None, None]) })
        .is_err()
    {
        bail!("capture already started");
    }

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
    std::thread::Builder::new().name("drift-capture".into()).spawn(move || unsafe {
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(HINSTANCE::default()), 0);
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(HINSTANCE::default()), 0);
        match (mouse, keyboard) {
            (Ok(_), Ok(_)) => {
                let _ = ready_tx.send(Ok(()));
            }
            (m, k) => {
                let _ = ready_tx.send(Err(anyhow::anyhow!("SetWindowsHookExW failed: {m:?} / {k:?}")));
                return;
            }
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
    })?;

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| anyhow::anyhow!("capture thread did not start"))?
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let Some(state) = STATE.get() else {
        return CallNextHookEx(None, code, wparam, lparam);
    };
    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    let msg = wparam.0 as u32;

    if info.flags & LLMHF_INJECTED != 0 {
        // Our own SendInput/SetCursorPos events: never touch them.
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let forwarding = state.ctl.forwarding.load(Ordering::SeqCst);

    if !forwarding {
        if msg == WM_MOUSEMOVE {
            maybe_enter_portal(state, info.pt.x, info.pt.y);
        }
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let park = *state.park.lock().unwrap();
    let captured = match msg {
        WM_MOUSEMOVE => {
            let dx = info.pt.x - park.0;
            let dy = info.pt.y - park.1;
            if dx == 0 && dy == 0 {
                None
            } else {
                Some(InputEvent::MouseMove { dx, dy })
            }
        }
        WM_LBUTTONDOWN => Some(InputEvent::MouseButton { button: MouseButton::Left, pressed: true }),
        WM_LBUTTONUP => Some(InputEvent::MouseButton { button: MouseButton::Left, pressed: false }),
        WM_RBUTTONDOWN => Some(InputEvent::MouseButton { button: MouseButton::Right, pressed: true }),
        WM_RBUTTONUP => Some(InputEvent::MouseButton { button: MouseButton::Right, pressed: false }),
        WM_MBUTTONDOWN => Some(InputEvent::MouseButton { button: MouseButton::Middle, pressed: true }),
        WM_MBUTTONUP => Some(InputEvent::MouseButton { button: MouseButton::Middle, pressed: false }),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let which = (info.mouseData >> 16) as u16;
            let button = if which == 2 { MouseButton::X2 } else { MouseButton::X1 };
            Some(InputEvent::MouseButton { button, pressed: msg == WM_XBUTTONDOWN })
        }
        WM_MOUSEWHEEL => Some(InputEvent::Wheel { dx: 0, dy: (info.mouseData >> 16) as i16 as i32 }),
        WM_MOUSEHWHEEL => Some(InputEvent::Wheel { dx: (info.mouseData >> 16) as i16 as i32, dy: 0 }),
        _ => None,
    };

    if let Some(ev) = captured {
        let _ = state.tx.send(Captured::Input(ev));
    }
    LRESULT(1) // swallow
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    let Some(state) = STATE.get() else {
        return CallNextHookEx(None, code, wparam, lparam);
    };
    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    if info.flags.0 & LLKHF_INJECTED != 0 {
        return CallNextHookEx(None, code, wparam, lparam);
    }
    if !state.ctl.forwarding.load(Ordering::SeqCst) {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    let msg = wparam.0 as u32;
    let pressed = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let released = msg == WM_KEYUP || msg == WM_SYSKEYUP;
    if !(pressed || released) {
        return CallNextHookEx(None, code, wparam, lparam);
    }

    const VK_ESCAPE: u16 = 0x1B;
    if pressed && info.vkCode as u16 == VK_ESCAPE && check_panic(state) {
        return LRESULT(1);
    }

    if let Some(key) = keymap::native_to_hid(info.vkCode as u16) {
        let _ = state.tx.send(Captured::Input(InputEvent::Key { key, pressed }));
    }
    LRESULT(1) // swallow
}

unsafe fn maybe_enter_portal(state: &CapState, x: i32, y: i32) {
    if Instant::now() < *state.ctl.cooldown_until.lock().unwrap() {
        return;
    }
    let bounds = state.ctl.bounds;
    let portals = state.ctl.portals.read().unwrap().clone();
    for edge in portals {
        if touches_edge(bounds, edge, x, y) {
            state.ctl.forwarding.store(true, Ordering::SeqCst);
            // Park the cursor away from the edge so blocked-event positions
            // never clamp (which would eat outward motion).
            let (px, py) = park_point(bounds, edge, x, y);
            let _ = SetCursorPos(px, py);
            *state.park.lock().unwrap() = (px, py);
            let ratio = ratio_on_edge(bounds, edge, x, y);
            let _ = state.tx.send(Captured::EdgeHit { edge, ratio });
            return;
        }
    }
}

fn park_point(bounds: Rect, edge: Edge, x: i32, y: i32) -> (i32, i32) {
    match edge {
        Edge::Left => ((bounds.x + PARK_INSET).min(bounds.right() - 1), y),
        Edge::Right => ((bounds.right() - 1 - PARK_INSET).max(bounds.x), y),
        Edge::Top => (x, (bounds.y + PARK_INSET).min(bounds.bottom() - 1)),
        Edge::Bottom => (x, (bounds.bottom() - 1 - PARK_INSET).max(bounds.y)),
    }
}

fn check_panic(state: &CapState) -> bool {
    let now = Instant::now();
    let window = Duration::from_millis(900);
    let mut esc = state.esc_downs.lock().unwrap();
    let hit = matches!(
        (esc[0], esc[1]),
        (Some(a), Some(b)) if now.duration_since(a) < window && now.duration_since(b) < window
    );
    esc[0] = esc[1];
    esc[1] = Some(now);
    if hit {
        *esc = [None, None];
        state.ctl.forwarding.store(false, Ordering::SeqCst);
        let _ = state.tx.send(Captured::Panic);
    }
    hit
}

// ------------------------------------------------------------ injector ----

/// HID usages that need KEYEVENTF_EXTENDEDKEY on Windows.
const EXTENDED_HIDS: &[u16] = &[
    0x46, // PrintScreen
    0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, // Ins/Home/PgUp/Del/End/PgDn
    0x4F, 0x50, 0x51, 0x52, // arrows
    0x54, // KP divide
    0x58, // KP enter
    0xE4, 0xE6, // RCtrl, RAlt
    0xE3, 0xE7, // LWin, RWin
];

pub struct Injector {
    bounds: Rect,
    down_keys: Vec<u16>,
    down_buttons: Vec<MouseButton>,
}

impl Injector {
    pub fn new() -> Result<Self> {
        Ok(Injector { bounds: desktop_bounds(), down_keys: Vec::new(), down_buttons: Vec::new() })
    }

    fn send_mouse(&self, flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data as u32,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }

    pub fn mouse_to(&mut self, x: i32, y: i32, _dx: i32, _dy: i32) {
        // Normalize to 0..65535 across the virtual desktop.
        let nx = ((x - self.bounds.x) as i64 * 65535 / (self.bounds.w.max(1) as i64 - 1).max(1)) as i32;
        let ny = ((y - self.bounds.y) as i64 * 65535 / (self.bounds.h.max(1) as i64 - 1).max(1)) as i32;
        self.send_mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0);
    }

    pub fn button(&mut self, b: MouseButton, pressed: bool) {
        if pressed {
            if !self.down_buttons.contains(&b) {
                self.down_buttons.push(b);
            }
        } else {
            self.down_buttons.retain(|&x| x != b);
        }
        let (flags, data) = match (b, pressed) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::X1, p) => (if p { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP }, 1),
            (MouseButton::X2, p) => (if p { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP }, 2),
        };
        self.send_mouse(flags, 0, 0, data);
    }

    pub fn wheel(&mut self, dx: i32, dy: i32) {
        if dy != 0 {
            self.send_mouse(MOUSEEVENTF_WHEEL, 0, 0, dy);
        }
        if dx != 0 {
            self.send_mouse(MOUSEEVENTF_HWHEEL, 0, 0, dx);
        }
    }

    pub fn key(&mut self, hid: u16, pressed: bool) {
        let Some(vk) = keymap::hid_to_native(hid) else { return };
        if pressed {
            if !self.down_keys.contains(&hid) {
                self.down_keys.push(hid);
            }
        } else {
            self.down_keys.retain(|&k| k != hid);
        }
        let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if !pressed {
            flags |= KEYEVENTF_KEYUP;
        }
        if EXTENDED_HIDS.contains(&hid) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
    }

    pub fn release_all(&mut self) {
        for hid in std::mem::take(&mut self.down_keys) {
            self.key(hid, false);
        }
        for b in std::mem::take(&mut self.down_buttons) {
            self.button(b, false);
        }
    }
}
