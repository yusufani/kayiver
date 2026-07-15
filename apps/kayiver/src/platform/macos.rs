//! macOS backend.
//!
//! Capture: a CGEventTap at the HID level on a dedicated thread. While
//! forwarding, events are swallowed (callback returns NULL) and the cursor is
//! frozen via `CGAssociateMouseAndMouseCursorPosition(false)` — mouse deltas
//! keep flowing to the tap with the cursor pinned, exactly like an FPS game
//! grabs the mouse. No warp-recenter tricks, no visible jitter.
//!
//! Injection: CGEventPost at the HID level. Modifier flags are tracked
//! explicitly because synthetic modifier key events do not implicitly flag
//! subsequent events.
//!
//! Permissions: Accessibility (event posting + active tap) and Input
//! Monitoring (listening). `kayiver doctor` reports both.

#![allow(non_snake_case, non_upper_case_globals, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use kayiver_core::layout::{ratio_on_edge, touches_edge, Edge};
use kayiver_core::proto::{InputEvent, MouseButton, Rect};
use tokio::sync::mpsc::UnboundedSender;

use crate::engine::Captured;
use crate::keymap;
use crate::platform::CaptureCtl;

// ---------------------------------------------------------------- FFI ----

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFStringRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type TapCallback =
    unsafe extern "C" fn(proxy: *mut c_void, etype: u32, event: CGEventRef, user: *mut c_void) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, cb: TapCallback, user: *mut c_void) -> CFMachPortRef;
    fn CGEventTapEnable(port: CFMachPortRef, enable: bool);
    fn CGEventGetLocation(e: CGEventRef) -> CGPoint;
    fn CGEventGetFlags(e: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(e: CGEventRef, field: u32) -> i64;
    fn CGEventSetIntegerValueField(e: CGEventRef, field: u32, value: i64);
    fn CGEventSetFlags(e: CGEventRef, flags: u64);
    fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
    fn CGEventCreateMouseEvent(source: CGEventSourceRef, ty: u32, pos: CGPoint, button: u32) -> CGEventRef;
    fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, keycode: u16, keydown: bool) -> CGEventRef;
    fn CGEventCreateScrollWheelEvent2(source: CGEventSourceRef, units: u32, wheel_count: u32, w1: i32, w2: i32, w3: i32) -> CGEventRef;
    fn CGEventPost(tap: u32, e: CGEventRef);
    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGWarpMouseCursorPosition(p: CGPoint) -> i32;
    fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
    fn CGDisplayHideCursor(display: u32) -> i32;
    fn CGDisplayShowCursor(display: u32) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGGetActiveDisplayList(max: u32, ids: *mut u32, count: *mut u32) -> i32;
    fn CGGetOnlineDisplayList(max: u32, ids: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(id: u32) -> CGRect;
    fn CGDisplayIsInMirrorSet(display: u32) -> u32;
    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGConfigureDisplayMirrorOfDisplay(config: *mut c_void, display: u32, master: u32) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: u32) -> i32;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    // Canonical Input-Monitoring APIs: requesting access is what registers the
    // app in System Settings → Privacy & Security → Input Monitoring so the
    // user can toggle it on. `request_type` 1 = kIOHIDRequestTypeListenEvent.
    fn IOHIDRequestAccess(request_type: u32) -> bool;
}
const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: CFMachPortRef, order: i64) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, src: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    static kCFRunLoopCommonModes: CFStringRef;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(p: *const c_void);
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

// CGEventType values.
const ET_LEFT_DOWN: u32 = 1;
const ET_LEFT_UP: u32 = 2;
const ET_RIGHT_DOWN: u32 = 3;
const ET_RIGHT_UP: u32 = 4;
const ET_MOVED: u32 = 5;
const ET_LEFT_DRAG: u32 = 6;
const ET_RIGHT_DRAG: u32 = 7;
const ET_KEY_DOWN: u32 = 10;
const ET_KEY_UP: u32 = 11;
const ET_FLAGS: u32 = 12;
const ET_SCROLL: u32 = 22;
const ET_OTHER_DOWN: u32 = 25;
const ET_OTHER_UP: u32 = 26;
const ET_OTHER_DRAG: u32 = 27;
const ET_TAP_DISABLED_TIMEOUT: u32 = 0xFFFFFFFE;
const ET_TAP_DISABLED_INPUT: u32 = 0xFFFFFFFF;

// CGEventField values.
const F_MOUSE_CLICK_STATE: u32 = 1;
const F_MOUSE_BUTTON: u32 = 3;
const F_MOUSE_DELTA_X: u32 = 4;
const F_MOUSE_DELTA_Y: u32 = 5;
const F_KEYCODE: u32 = 9;
const F_SCROLL_AXIS1: u32 = 11; // vertical, line units
const F_SCROLL_AXIS2: u32 = 12; // horizontal, line units

// CGEventFlags masks.
const FLAG_CAPS: u64 = 0x0001_0000;
const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CTRL: u64 = 0x0004_0000;
const FLAG_ALT: u64 = 0x0008_0000;
const FLAG_CMD: u64 = 0x0010_0000;

const TAP_HID: u32 = 0; // kCGHIDEventTap
const TAP_HEAD_INSERT: u32 = 0;
const TAP_OPT_DEFAULT: u32 = 0;
const SCROLL_UNIT_LINE: u32 = 1;
const SOURCE_HID_STATE: i32 = 1; // kCGEventSourceStateHIDSystemState

// ------------------------------------------------------------- helpers ----

/// Every physical display, in global top-left-origin coordinates.
pub fn monitors() -> Vec<Rect> {
    unsafe {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        if CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) != 0 || count == 0 {
            return vec![cgrect_to_rect(CGDisplayBounds(CGMainDisplayID()))];
        }
        ids[..count as usize].iter().map(|&id| cgrect_to_rect(CGDisplayBounds(id))).collect()
    }
}

pub fn desktop_bounds() -> Rect {
    let mons = monitors();
    let min_x = mons.iter().map(|m| m.x).min().unwrap_or(0);
    let min_y = mons.iter().map(|m| m.y).min().unwrap_or(0);
    let max_x = mons.iter().map(|m| m.right()).max().unwrap_or(1920);
    let max_y = mons.iter().map(|m| m.bottom()).max().unwrap_or(1080);
    Rect { x: min_x, y: min_y, w: max_x - min_x, h: max_y - min_y }
}

fn cgrect_to_rect(b: CGRect) -> Rect {
    Rect { x: b.origin.x as i32, y: b.origin.y as i32, w: b.size.width as i32, h: b.size.height as i32 }
}

fn permissions_ok() -> bool {
    unsafe { AXIsProcessTrusted() && CGPreflightListenEventAccess() && CGPreflightPostEventAccess() }
}

/// Show the native macOS permission dialog for Accessibility (registers the
/// app in the Settings list and offers an "Open System Settings" button).
fn prompt_accessibility() {
    unsafe {
        let key = kAXTrustedCheckOptionPrompt as *const c_void;
        let val = kCFBooleanTrue;
        let dict = CFDictionaryCreate(
            std::ptr::null(),
            &key,
            &val,
            1,
            &kCFTypeDictionaryKeyCallBacks as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const c_void,
        );
        AXIsProcessTrustedWithOptions(dict);
        if !dict.is_null() {
            CFRelease(dict);
        }
    }
}

/// Fresh-install flow: trigger the native permission prompts and wait until
/// the user approves, then continue automatically — no manual Settings
/// spelunking, no restart required in the common case.
pub fn init() {} // nothing to do on macOS

// --------------------------------------------------------------- DDC/CI ----
// Apple Silicon DDC goes through IOAVService, which the `m1ddc` helper wraps.
// We shell out to it (kept simple and robust); `kayiver doctor` warns if absent.

fn m1ddc_path() -> Option<&'static str> {
    for p in ["/opt/homebrew/bin/m1ddc", "/usr/local/bin/m1ddc"] {
        if std::path::Path::new(p).exists() {
            return Some(p);
        }
    }
    None
}

/// (index, name, current input-source VCP value) for each external display.
pub fn displays() -> Vec<(u32, String, Option<u16>)> {
    let Some(m) = m1ddc_path() else { return vec![] };
    let out = match std::process::Command::new(m).args(["display", "list"]).output() {
        Ok(o) => o,
        Err(_) => return vec![],
    };
    let list = String::from_utf8_lossy(&out.stdout);
    let mut result = Vec::new();
    for line in list.lines() {
        // "[1] LC32G5xT (UUID)"
        if let Some(rest) = line.trim().strip_prefix('[') {
            if let Some((idx, name)) = rest.split_once(']') {
                if let Ok(i) = idx.trim().parse::<u32>() {
                    let cur = std::process::Command::new(m)
                        .args(["display", &i.to_string(), "get", "input"])
                        .output()
                        .ok()
                        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u16>().ok());
                    result.push((i, name.trim().to_string(), cur));
                }
            }
        }
    }
    result
}

/// Active display IDs in the SAME order as `monitors()` (CGGetActiveDisplayList),
/// so a shared-monitor index means the same physical display in the editor and
/// in enable/disable. (Mirrored displays stay in this list, so an index is
/// stable across a disable/enable cycle.)
fn online_display_ids() -> Vec<u32> {
    unsafe {
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        if CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) != 0 {
            return vec![];
        }
        ids[..count as usize].to_vec()
    }
}

/// "Disable" (true=enable) an external display for cursor purposes: disabling
/// mirrors it onto the main display, so it leaves the extended desktop and the
/// pointer can no longer wander onto a panel that is physically showing the
/// other machine. Fully reversible. `index` is 1-based (matches m1ddc/list).
pub fn set_display_enabled(index: u32, expect: Option<Rect>, enabled: bool) -> anyhow::Result<()> {
    let ids = online_display_ids();
    let target = *ids
        .get(index.saturating_sub(1) as usize)
        .ok_or_else(|| anyhow::anyhow!("display index {index} out of range"))?;
    let main = unsafe { CGMainDisplayID() };
    anyhow::ensure!(target != main, "refusing to disable the main display");
    // Safety: only ever disable the exact monitor we mean.
    if !enabled {
        if let Some(exp) = expect.filter(|e| e.w != 0 && e.h != 0) {
            let got = cgrect_to_rect(unsafe { CGDisplayBounds(target) });
            anyhow::ensure!(
                kayiver_core::proto::rects_match(got, exp),
                "safety: display {index} is {got:?}, expected shared panel {exp:?} — refusing to disable the wrong monitor"
            );
        }
    }
    unsafe {
        let mut config: *mut c_void = std::ptr::null_mut();
        anyhow::ensure!(CGBeginDisplayConfiguration(&mut config) == 0, "begin config failed");
        // master = main to mirror (disable); 0 (kCGNullDirectDisplay) to restore.
        let master = if enabled { 0 } else { main };
        CGConfigureDisplayMirrorOfDisplay(config, target, master);
        // option 2 = kCGConfigurePermanently.
        anyhow::ensure!(CGCompleteDisplayConfiguration(config, 2) == 0, "complete config failed");
    }
    Ok(())
}

/// Is a display currently "disabled" (mirrored away by `set_display_enabled`)?
/// `index` is 1-based, matching `kayiver display list`. None = can't tell.
pub fn display_disabled(index: u32) -> Option<bool> {
    let ids = online_display_ids();
    let target = *ids.get(index.saturating_sub(1) as usize)?;
    let main = unsafe { CGMainDisplayID() };
    if target == main {
        return Some(false);
    }
    Some(unsafe { CGDisplayIsInMirrorSet(target) } != 0)
}

pub fn ensure_permissions() -> Result<()> {
    if permissions_ok() {
        return Ok(());
    }
    unsafe {
        // Each of these pops the corresponding system dialog (once) and
        // registers this binary in the right Privacy & Security list.
        if !AXIsProcessTrusted() {
            prompt_accessibility();
        }
        if !CGPreflightListenEventAccess() {
            // Both requests register the app in the Input Monitoring list;
            // IOHIDRequestAccess is the canonical one and is what reliably
            // makes "Kayıver" appear there as a toggle.
            IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT);
            CGRequestListenEventAccess();
        }
        if !CGPreflightPostEventAccess() {
            CGRequestPostEventAccess();
        }
    }
    eprintln!();
    eprintln!("kayiver needs two macOS permissions: Accessibility and Input Monitoring.");
    eprintln!("Approve the dialogs that just appeared — kayiver will continue by itself.");

    let start = Instant::now();
    let mut opened_settings = false;
    let mut last_reminder = Instant::now();
    // Wait patiently (up to 15 min): granting these takes a trip through
    // System Settings and there is no reason to rush the user.
    while start.elapsed() < Duration::from_secs(900) {
        if permissions_ok() {
            eprintln!("permissions granted — continuing.");
            return Ok(());
        }
        // If nothing happened after a few seconds the dialogs were probably
        // dismissed; open the exact Settings panes as a fallback.
        if !opened_settings && start.elapsed() > Duration::from_secs(6) {
            opened_settings = true;
            eprintln!("opening System Settings at the right panes — enable kayiver (or Terminal) in BOTH:");
            eprintln!("  • Privacy & Security → Accessibility");
            eprintln!("  • Privacy & Security → Input Monitoring");
            let _ = std::process::Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                .spawn();
            let _ = std::process::Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
                .spawn();
        }
        if last_reminder.elapsed() > Duration::from_secs(30) {
            last_reminder = Instant::now();
            eprintln!("still waiting for permissions… (kayiver continues automatically once granted)");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    anyhow::bail!(
        "permissions still missing — enable kayiver in System Settings → Privacy & Security → \
         Accessibility and Input Monitoring, then run `kayiver run` again"
    )
}

pub fn doctor_permissions() {
    unsafe {
        println!("  accessibility   : {}", if AXIsProcessTrusted() { "granted" } else { "MISSING (System Settings -> Privacy & Security -> Accessibility)" });
        println!("  input monitoring: {}", if CGPreflightListenEventAccess() { "granted" } else { "MISSING (System Settings -> Privacy & Security -> Input Monitoring)" });
        println!("  event posting   : {}", if CGPreflightPostEventAccess() { "granted" } else { "MISSING (usually granted with Accessibility)" });
    }
}

pub fn warp_cursor(x: i32, y: i32) {
    unsafe {
        CGWarpMouseCursorPosition(CGPoint { x: x as f64, y: y as f64 });
    }
}

/// Warp the cursor AND immediately cancel macOS's post-warp local-events
/// suppression (~0.25 s, during which physical mouse moves are swallowed).
/// Re-associating right after the warp defeats that window, so control feels
/// instant when it returns to this machine. Use ONLY when regaining local
/// control — the plain `warp_cursor` (cursor parked while forwarding) must not
/// re-associate, or it would reconnect the frozen pointer mid-session.
pub fn warp_cursor_settled(x: i32, y: i32) {
    unsafe {
        CGWarpMouseCursorPosition(CGPoint { x: x as f64, y: y as f64 });
        CGAssociateMouseAndMouseCursorPosition(1);
    }
}

#[allow(dead_code)]
pub fn cursor_pos() -> (i32, i32) {
    unsafe {
        let e = CGEventCreate(std::ptr::null_mut());
        let p = CGEventGetLocation(e);
        CFRelease(e);
        (p.x as i32, p.y as i32)
    }
}

pub fn set_forwarding_visuals(on: bool) {
    // CGDisplayHideCursor/ShowCursor are REFERENCE-COUNTED: two hides need two
    // shows or the cursor stays hidden (and, being app-scoped, only reappears
    // when kayiver isn't frontmost). Crossing onto the tablet used to hide twice
    // — once from the capture thread, once from tablet control — but restore
    // once. Guard the hide/show so it's idempotent: exactly one of each.
    static HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    unsafe {
        if on {
            if !HIDDEN.swap(true, Ordering::SeqCst) {
                CGDisplayHideCursor(CGMainDisplayID());
            }
            CGAssociateMouseAndMouseCursorPosition(0);
        } else {
            CGAssociateMouseAndMouseCursorPosition(1);
            if HIDDEN.swap(false, Ordering::SeqCst) {
                CGDisplayShowCursor(CGMainDisplayID());
            }
        }
    }
}

// ------------------------------------------------------------- capture ----

struct CaptureState {
    ctl: Arc<CaptureCtl>,
    tx: UnboundedSender<Captured>,
    tap: CFMachPortRef,
    /// Modifier keycodes currently held (for flagsChanged press/release).
    mods_down: Vec<u16>,
    esc_downs: [Option<Instant>; 2],
    /// While a dwell is configured: which portal edge the cursor is currently
    /// resting against, and since when. Cleared when it leaves the edge.
    edge_pending: Option<(Edge, Instant)>,
    /// Where the local cursor is pinned while forwarding. Every swallowed
    /// pointer event warps back here, so the local pointer is rock-solid even
    /// if the OS re-associates the mouse under us.
    park: CGPoint,
}

pub fn start_capture(ctl: Arc<CaptureCtl>, tx: UnboundedSender<Captured>) -> Result<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();

    std::thread::Builder::new()
        .name("kayiver-capture".into())
        .spawn(move || unsafe {
            let state = Box::into_raw(Box::new(CaptureState {
                ctl,
                tx,
                tap: std::ptr::null_mut(),
                mods_down: Vec::new(),
                esc_downs: [None, None],
                edge_pending: None,
                park: CGPoint { x: 0.0, y: 0.0 },
            }));

            let mask: u64 = [
                ET_LEFT_DOWN, ET_LEFT_UP, ET_RIGHT_DOWN, ET_RIGHT_UP, ET_MOVED, ET_LEFT_DRAG,
                ET_RIGHT_DRAG, ET_KEY_DOWN, ET_KEY_UP, ET_FLAGS, ET_SCROLL, ET_OTHER_DOWN,
                ET_OTHER_UP, ET_OTHER_DRAG,
            ]
            .iter()
            .fold(0u64, |m, t| m | (1u64 << t));

            let tap = CGEventTapCreate(TAP_HID, TAP_HEAD_INSERT, TAP_OPT_DEFAULT, mask, tap_callback, state as *mut c_void);
            if tap.is_null() {
                let _ = ready_tx.send(Err(anyhow::anyhow!(
                    "CGEventTapCreate failed — grant Accessibility & Input Monitoring permissions (see `kayiver doctor`)"
                )));
                return;
            }
            (*state).tap = tap;

            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            let _ = ready_tx.send(Ok(()));
            CFRunLoopRun();
        })?;

    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| anyhow::anyhow!("capture thread did not start"))?
}

unsafe extern "C" fn tap_callback(_proxy: *mut c_void, etype: u32, event: CGEventRef, user: *mut c_void) -> CGEventRef {
    let state = &mut *(user as *mut CaptureState);

    if etype == ET_TAP_DISABLED_TIMEOUT || etype == ET_TAP_DISABLED_INPUT {
        // macOS disables taps it thinks are too slow; recover immediately.
        CGEventTapEnable(state.tap, true);
        return event;
    }

    // Shared-monitor hotkey (Cmd+Alt+M) works in both modes: the tap sees
    // every physical key regardless of where the cursor currently lives.
    if etype == ET_KEY_DOWN && state.ctl.shared_hotkey.load(Ordering::Relaxed) {
        const VK_M: i64 = 46;
        let flags = CGEventGetFlags(event);
        if CGEventGetIntegerValueField(event, F_KEYCODE) == VK_M
            && flags & FLAG_CMD != 0
            && flags & FLAG_ALT != 0
        {
            let _ = state.tx.send(Captured::SharedHotkey);
            return std::ptr::null_mut(); // swallow the keystroke
        }
    }

    // Tablet-control hotkey (Cmd+Alt+T): toggle control of the Android tablet.
    if etype == ET_KEY_DOWN {
        const VK_T: i64 = 17;
        let flags = CGEventGetFlags(event);
        if CGEventGetIntegerValueField(event, F_KEYCODE) == VK_T
            && flags & FLAG_CMD != 0
            && flags & FLAG_ALT != 0
        {
            let _ = state.tx.send(Captured::TabletHotkey);
            return std::ptr::null_mut();
        }
    }

    let forwarding = state.ctl.forwarding.load(Ordering::SeqCst);

    if !forwarding {
        // Local mode: watch for portal edge hits on motion, touch nothing else.
        if etype == ET_MOVED || etype == ET_LEFT_DRAG || etype == ET_RIGHT_DRAG || etype == ET_OTHER_DRAG {
            let p = CGEventGetLocation(event);
            maybe_enter_portal(state, p.x as i32, p.y as i32);
        }
        return event;
    }

    // Forwarding mode: translate, ship, swallow.
    let captured = match etype {
        ET_MOVED | ET_LEFT_DRAG | ET_RIGHT_DRAG | ET_OTHER_DRAG => {
            let dx = CGEventGetIntegerValueField(event, F_MOUSE_DELTA_X) as i32;
            let dy = CGEventGetIntegerValueField(event, F_MOUSE_DELTA_Y) as i32;
            // Pin the local pointer: warp it back to the park spot every move.
            // Belt-and-braces on top of the association disconnect, so the
            // local cursor never wanders while you're driving the other screen.
            CGWarpMouseCursorPosition(state.park);
            Some(InputEvent::MouseMove { dx, dy })
        }
        ET_LEFT_DOWN => Some(InputEvent::MouseButton { button: MouseButton::Left, pressed: true }),
        ET_LEFT_UP => Some(InputEvent::MouseButton { button: MouseButton::Left, pressed: false }),
        ET_RIGHT_DOWN => Some(InputEvent::MouseButton { button: MouseButton::Right, pressed: true }),
        ET_RIGHT_UP => Some(InputEvent::MouseButton { button: MouseButton::Right, pressed: false }),
        ET_OTHER_DOWN | ET_OTHER_UP => {
            let n = CGEventGetIntegerValueField(event, F_MOUSE_BUTTON);
            let button = match n {
                2 => MouseButton::Middle,
                3 => MouseButton::X1,
                4 => MouseButton::X2,
                _ => MouseButton::Middle,
            };
            Some(InputEvent::MouseButton { button, pressed: etype == ET_OTHER_DOWN })
        }
        ET_SCROLL => {
            let dy = CGEventGetIntegerValueField(event, F_SCROLL_AXIS1) as i32;
            let dx = CGEventGetIntegerValueField(event, F_SCROLL_AXIS2) as i32;
            Some(InputEvent::Wheel { dx: dx * 120, dy: dy * 120 })
        }
        ET_KEY_DOWN | ET_KEY_UP => {
            let vk = CGEventGetIntegerValueField(event, F_KEYCODE) as u16;
            let pressed = etype == ET_KEY_DOWN;
            if pressed && check_panic(state, vk) {
                return std::ptr::null_mut();
            }
            keymap::native_to_hid(vk).map(|key| InputEvent::Key { key, pressed })
        }
        ET_FLAGS => {
            // Modifier press/release arrives as flagsChanged; track held set.
            let vk = CGEventGetIntegerValueField(event, F_KEYCODE) as u16;
            if let Some(key) = keymap::native_to_hid(vk) {
                let pressed = if let Some(i) = state.mods_down.iter().position(|&m| m == vk) {
                    state.mods_down.remove(i);
                    false
                } else {
                    state.mods_down.push(vk);
                    true
                };
                Some(InputEvent::Key { key, pressed })
            } else {
                None
            }
        }
        _ => None,
    };

    if let Some(ev) = captured {
        let _ = state.tx.send(Captured::Input(ev));
    }
    std::ptr::null_mut() // swallow
}

unsafe fn maybe_enter_portal(state: &mut CaptureState, x: i32, y: i32) {
    if Instant::now() < *state.ctl.cooldown_until.lock().unwrap() {
        return;
    }
    let bounds = state.ctl.bounds;
    let portals = state.ctl.portals.read().unwrap().clone();
    let dwell = state.ctl.edge_dwell_ms.load(Ordering::Relaxed);
    for edge in portals {
        if touches_edge(bounds, edge, x, y) {
            // Optional dwell: require the cursor to rest against this edge for
            // `dwell` ms before crossing, so a quick brush doesn't jump screens.
            if dwell > 0 {
                match state.edge_pending {
                    Some((e, since)) if e == edge => {
                        if since.elapsed() < Duration::from_millis(dwell) {
                            return; // still charging up at this edge
                        }
                    }
                    _ => {
                        state.edge_pending = Some((edge, Instant::now()));
                        return; // just arrived at the edge; start the timer
                    }
                }
            }
            state.edge_pending = None;
            // Flip into forwarding *now*, inside the callback: the very next
            // event is already swallowed. Then tell the router.
            // Park a little inside the edge we're leaving through, so the
            // pinned pointer is off the boundary (won't re-trigger on return).
            let park_x = (x as f64).clamp(bounds.x as f64 + 4.0, bounds.right() as f64 - 5.0);
            let park_y = (y as f64).clamp(bounds.y as f64 + 4.0, bounds.bottom() as f64 - 5.0);
            state.park = CGPoint { x: park_x, y: park_y };
            state.ctl.forwarding.store(true, Ordering::SeqCst);
            set_forwarding_visuals(true);
            CGWarpMouseCursorPosition(state.park);
            let ratio = ratio_on_edge(bounds, edge, x, y);
            let _ = state.tx.send(Captured::EdgeHit { edge, ratio });
            return;
        }
    }
    // Not touching any portal edge — reset the dwell timer.
    state.edge_pending = None;
}

/// Triple-Esc within 900ms yanks input back to the host even if the remote
/// side is wedged.
unsafe fn check_panic(state: &mut CaptureState, vk: u16) -> bool {
    const ESC_VK: u16 = 53;
    if vk != ESC_VK {
        return false;
    }
    let now = Instant::now();
    let window = Duration::from_millis(900);
    let hit = matches!(
        (state.esc_downs[0], state.esc_downs[1]),
        (Some(a), Some(b)) if now.duration_since(a) < window && now.duration_since(b) < window
    );
    state.esc_downs[0] = state.esc_downs[1];
    state.esc_downs[1] = Some(now);
    if hit {
        state.ctl.forwarding.store(false, Ordering::SeqCst);
        set_forwarding_visuals(false);
        state.esc_downs = [None, None];
        let _ = state.tx.send(Captured::Panic);
    }
    hit
}

// ------------------------------------------------------------ injector ----

pub struct Injector {
    source: CGEventSourceRef,
    /// Native mouse button state, for choosing drag vs move event types.
    left_down: bool,
    right_down: bool,
    other_down: bool,
    flags: u64,
    down_keys: Vec<u16>, // HID
    down_buttons: Vec<MouseButton>,
    last_pos: CGPoint,
    last_click: Option<(Instant, CGPoint, i64)>,
}

// CGEventSourceRef/CGEventRef are used from the single client session task.
unsafe impl Send for Injector {}

impl Injector {
    pub fn new() -> Result<Self> {
        let source = unsafe { CGEventSourceCreate(SOURCE_HID_STATE) };
        if source.is_null() {
            bail!("CGEventSourceCreate failed — check Accessibility permission (`kayiver doctor`)");
        }
        Ok(Injector {
            source,
            left_down: false,
            right_down: false,
            other_down: false,
            flags: 0,
            down_keys: Vec::new(),
            down_buttons: Vec::new(),
            last_pos: CGPoint { x: 0.0, y: 0.0 },
            last_click: None,
        })
    }

    fn post(&self, e: CGEventRef) {
        unsafe {
            if !e.is_null() {
                CGEventSetFlags(e, self.flags);
                CGEventPost(TAP_HID, e);
                CFRelease(e);
            }
        }
    }

    pub fn mouse_to(&mut self, x: i32, y: i32, dx: i32, dy: i32) {
        let pos = CGPoint { x: x as f64, y: y as f64 };
        self.last_pos = pos;
        let (ty, button) = if self.left_down {
            (ET_LEFT_DRAG, 0)
        } else if self.right_down {
            (ET_RIGHT_DRAG, 1)
        } else if self.other_down {
            (ET_OTHER_DRAG, 2)
        } else {
            (ET_MOVED, 0)
        };
        unsafe {
            let e = CGEventCreateMouseEvent(self.source, ty, pos, button);
            // Preserve raw deltas for apps that read them (games, 3D tools).
            CGEventSetIntegerValueField(e, F_MOUSE_DELTA_X, dx as i64);
            CGEventSetIntegerValueField(e, F_MOUSE_DELTA_Y, dy as i64);
            self.post(e);
        }
    }

    pub fn button(&mut self, b: MouseButton, pressed: bool) {
        let (ty, num) = match (b, pressed) {
            (MouseButton::Left, true) => (ET_LEFT_DOWN, 0),
            (MouseButton::Left, false) => (ET_LEFT_UP, 0),
            (MouseButton::Right, true) => (ET_RIGHT_DOWN, 1),
            (MouseButton::Right, false) => (ET_RIGHT_UP, 1),
            (MouseButton::Middle, p) => (if p { ET_OTHER_DOWN } else { ET_OTHER_UP }, 2),
            (MouseButton::X1, p) => (if p { ET_OTHER_DOWN } else { ET_OTHER_UP }, 3),
            (MouseButton::X2, p) => (if p { ET_OTHER_DOWN } else { ET_OTHER_UP }, 4),
        };
        match b {
            MouseButton::Left => self.left_down = pressed,
            MouseButton::Right => self.right_down = pressed,
            _ => self.other_down = pressed,
        }
        if pressed {
            if !self.down_buttons.contains(&b) {
                self.down_buttons.push(b);
            }
        } else {
            self.down_buttons.retain(|&x| x != b);
        }

        // Double-click detection: macOS relies on the click-state field.
        let click_state = if pressed {
            let now = Instant::now();
            let state = match self.last_click {
                Some((t, p, c))
                    if now.duration_since(t) < Duration::from_millis(500)
                        && (p.x - self.last_pos.x).abs() < 5.0
                        && (p.y - self.last_pos.y).abs() < 5.0 =>
                {
                    c + 1
                }
                _ => 1,
            };
            self.last_click = Some((now, self.last_pos, state));
            state
        } else {
            self.last_click.map(|(_, _, c)| c).unwrap_or(1)
        };

        unsafe {
            let e = CGEventCreateMouseEvent(self.source, ty, self.last_pos, num);
            CGEventSetIntegerValueField(e, F_MOUSE_CLICK_STATE, click_state);
            self.post(e);
        }
    }

    pub fn wheel(&mut self, dx: i32, dy: i32) {
        let lines_y = to_lines(dy);
        let lines_x = to_lines(dx);
        if lines_x == 0 && lines_y == 0 {
            return;
        }
        unsafe {
            let e = CGEventCreateScrollWheelEvent2(self.source, SCROLL_UNIT_LINE, 2, lines_y, lines_x, 0);
            self.post(e);
        }
    }

    pub fn key(&mut self, hid: u16, pressed: bool) {
        let Some(vk) = keymap::hid_to_native(hid) else { return };
        // Synthetic events need explicit modifier flags.
        let mask = match hid {
            keymap::HID_LSHIFT | keymap::HID_RSHIFT => FLAG_SHIFT,
            keymap::HID_LCTRL | keymap::HID_RCTRL => FLAG_CTRL,
            keymap::HID_LALT | keymap::HID_RALT => FLAG_ALT,
            keymap::HID_LGUI | keymap::HID_RGUI => FLAG_CMD,
            keymap::HID_CAPSLOCK => FLAG_CAPS,
            _ => 0,
        };
        if mask != 0 {
            if hid == keymap::HID_CAPSLOCK {
                if pressed {
                    self.flags ^= mask; // caps lock toggles
                }
            } else if pressed {
                self.flags |= mask;
            } else {
                self.flags &= !mask;
            }
        }
        if pressed {
            if !self.down_keys.contains(&hid) {
                self.down_keys.push(hid);
            }
        } else {
            self.down_keys.retain(|&k| k != hid);
        }
        unsafe {
            let e = CGEventCreateKeyboardEvent(self.source, vk, pressed);
            self.post(e);
        }
    }

    /// Belt-and-braces: release anything still held (host also sends explicit
    /// releases on focus change, but a dropped connection skips those).
    pub fn release_all(&mut self) {
        for hid in std::mem::take(&mut self.down_keys) {
            self.key(hid, false);
        }
        for b in std::mem::take(&mut self.down_buttons) {
            self.button(b, false);
        }
        self.flags = 0;
    }
}

fn to_lines(v: i32) -> i32 {
    if v == 0 {
        0
    } else {
        let lines = (v as f32 / 120.0).round() as i32;
        if lines == 0 {
            v.signum()
        } else {
            lines
        }
    }
}

// ------------------------------------------------------ clipboard / urls ----

/// Read the general clipboard as text (via `pbpaste`).
pub fn get_clipboard() -> Option<String> {
    let out = std::process::Command::new("pbpaste").output().ok()?;
    if out.status.success() {
        String::from_utf8(out.stdout).ok()
    } else {
        None
    }
}

/// Replace the general clipboard with `text` (via `pbcopy`).
pub fn set_clipboard(text: &str) {
    use std::io::Write;
    if let Ok(mut child) = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// If a link is currently being dragged, return its URL. Reads the system drag
/// pasteboard, which browsers populate with `public.url` when you drag a link
/// or a tab. `None` when nothing URL-like is on it.
pub fn drag_url() -> Option<String> {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;
    unsafe {
        let name = NSString::from_str("Apple CFPasteboard drag");
        let pb = NSPasteboard::pasteboardWithName(&name);
        for ty in ["public.url", "public.utf8-plain-text"] {
            let t = NSString::from_str(ty);
            if let Some(s) = pb.stringForType(&t) {
                let st = s.to_string();
                let trimmed = st.trim();
                if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }
}

/// Open a URL in the default browser.
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

/// Monotonic clipboard change counter (cheap; avoids reading the whole
/// clipboard every poll). Bumps on any change by any app.
pub fn clipboard_seq() -> u64 {
    use objc2_app_kit::NSPasteboard;
    unsafe { NSPasteboard::generalPasteboard().changeCount() as u64 }
}
