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
//! Monitoring (listening). `drift doctor` reports both.

#![allow(non_snake_case, non_upper_case_globals, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use drift_core::layout::{ratio_on_edge, touches_edge};
use drift_core::proto::{InputEvent, MouseButton, Rect};
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
    fn CGDisplayBounds(id: u32) -> CGRect;
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
    fn CGRequestPostEventAccess() -> bool;
}

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
            CGRequestListenEventAccess();
        }
        if !CGPreflightPostEventAccess() {
            CGRequestPostEventAccess();
        }
    }
    eprintln!();
    eprintln!("drift needs two macOS permissions: Accessibility and Input Monitoring.");
    eprintln!("Approve the dialogs that just appeared — drift will continue by itself.");

    let start = Instant::now();
    let mut opened_settings = false;
    while start.elapsed() < Duration::from_secs(180) {
        if permissions_ok() {
            eprintln!("permissions granted — continuing.");
            return Ok(());
        }
        // If nothing happened after a while the dialogs were probably
        // dismissed earlier; open the exact Settings panes as a fallback.
        if !opened_settings && start.elapsed() > Duration::from_secs(10) {
            opened_settings = true;
            eprintln!("still waiting… opening System Settings at the right panes:");
            eprintln!("  enable your terminal (or drift) in BOTH lists, then come back here.");
            let _ = std::process::Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                .spawn();
            let _ = std::process::Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
                .spawn();
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    anyhow::bail!(
        "permissions still missing after 3 minutes — enable this app in System Settings → \
         Privacy & Security → Accessibility and Input Monitoring, then run `drift run` again"
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
    unsafe {
        if on {
            CGDisplayHideCursor(CGMainDisplayID());
            CGAssociateMouseAndMouseCursorPosition(0);
        } else {
            CGAssociateMouseAndMouseCursorPosition(1);
            CGDisplayShowCursor(CGMainDisplayID());
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
}

pub fn start_capture(ctl: Arc<CaptureCtl>, tx: UnboundedSender<Captured>) -> Result<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();

    std::thread::Builder::new()
        .name("drift-capture".into())
        .spawn(move || unsafe {
            let state = Box::into_raw(Box::new(CaptureState {
                ctl,
                tx,
                tap: std::ptr::null_mut(),
                mods_down: Vec::new(),
                esc_downs: [None, None],
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
                    "CGEventTapCreate failed — grant Accessibility & Input Monitoring permissions (see `drift doctor`)"
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
    for edge in portals {
        if touches_edge(bounds, edge, x, y) {
            // Flip into forwarding *now*, inside the callback: the very next
            // event is already swallowed. Then tell the router.
            state.ctl.forwarding.store(true, Ordering::SeqCst);
            set_forwarding_visuals(true);
            let ratio = ratio_on_edge(bounds, edge, x, y);
            let _ = state.tx.send(Captured::EdgeHit { edge, ratio });
            return;
        }
    }
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
            bail!("CGEventSourceCreate failed — check Accessibility permission (`drift doctor`)");
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
