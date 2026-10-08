//! Move standard macOS windows through Accessibility; never activate apps.
use std::ffi::{c_char, c_void};
use kayiver_core::{layout::relocate_off, proto::Rect};
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWorkspace;

type Ref = *const c_void;
#[repr(C)]
#[derive(Default)]
struct Point { x: f64, y: f64 }
#[repr(C)]
#[derive(Default)]
struct Size { w: f64, h: f64 }

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementCopyAttributeValue(element: Ref, attr: Ref, out: *mut Ref) -> i32;
    fn AXUIElementSetAttributeValue(element: Ref, attr: Ref, value: Ref) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, seconds: f32) -> i32;
    fn AXValueCreate(kind: u32, value: Ref) -> Ref;
    fn AXValueGetType(value: Ref) -> u32;
    fn AXValueGetValue(value: Ref, kind: u32, out: *mut c_void) -> bool;
    fn AXValueGetTypeID() -> usize;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: Ref);
    fn CFStringCreateWithCString(alloc: Ref, value: *const c_char, encoding: u32) -> Ref;
    fn CFArrayGetTypeID() -> usize;
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
    fn CFEqual(a: Ref, b: Ref) -> bool;
    static kCFBooleanTrue: Ref;
}

struct Owned(Ref);
impl Drop for Owned {
    fn drop(&mut self) { if !self.0.is_null() { unsafe { CFRelease(self.0) } } }
}
impl Owned {
    fn string(s: &'static [u8]) -> Self {
        Self(unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr().cast(), 0x08000100) })
    }
}
unsafe fn attribute(window: Ref, name: &Owned) -> Option<Owned> {
    let mut value = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(window, name.0, &mut value);
    let owned = Owned(value);
    if err == 0 && !value.is_null() { Some(owned) } else { None }
}
unsafe fn is_true(window: Ref, name: &Owned) -> bool {
    attribute(window, name).is_some_and(|v| CFEqual(v.0, kCFBooleanTrue))
}
unsafe fn geometry<T: Default>(window: Ref, name: &Owned, kind: u32) -> Option<T> {
    let value = attribute(window, name)?;
    if CFGetTypeID(value.0) != AXValueGetTypeID() || AXValueGetType(value.0) != kind { return None; }
    let mut out = T::default();
    AXValueGetValue(value.0, kind, (&mut out as *mut T).cast()).then_some(out)
}

pub fn rescue(blocked: Rect) -> usize {
    if !unsafe { AXIsProcessTrusted() } { return 0; }
    // Refuse to use a missing/stale panel or another view of the same panel.
    let monitors = super::macos::monitors();
    if !monitors.iter().any(|r| kayiver_core::proto::rects_match(*r, blocked)) { return 0; }
    let Some(mut target) = monitors.into_iter().find(|r|
        r.right() <= blocked.x || r.x >= blocked.right()
            || r.bottom() <= blocked.y || r.y >= blocked.bottom()
    ) else { return 0 };
    // Keep title bars below the menu bar, without resizing user windows.
    target.y += 32;
    target.h -= 32;
    if target.w <= 0 || target.h <= 0 { return 0; }
    autoreleasepool(|_| unsafe {
        let windows = Owned::string(b"AXWindows\0");
        let position = Owned::string(b"AXPosition\0");
        let size = Owned::string(b"AXSize\0");
        let minimized = Owned::string(b"AXMinimized\0");
        let fullscreen = Owned::string(b"AXFullScreen\0");
        let subrole = Owned::string(b"AXSubrole\0");
        let standard = Owned::string(b"AXStandardWindow\0");
        let mut moved = 0;
        for running in NSWorkspace::sharedWorkspace().runningApplications().iter() {
            // AX sets on our own NSWindow are dispatched synchronously on
            // this worker and AppKit asserts. The GUI rescues its own editor.
            if running.processIdentifier() == std::process::id() as i32
                || running.isHidden() || running.isTerminated() { continue; }
            let app = Owned(AXUIElementCreateApplication(running.processIdentifier()));
            if app.0.is_null() { continue; }
            // A hung application must not stall window rescue/the router.
            AXUIElementSetMessagingTimeout(app.0, 0.2);
            let Some(list) = attribute(app.0, &windows) else { continue };
            if CFGetTypeID(list.0) != CFArrayGetTypeID() { continue; }
            for i in 0..CFArrayGetCount(list.0) {
                let win = CFArrayGetValueAtIndex(list.0, i);
                let Some(role) = attribute(win, &subrole) else { continue };
                if !CFEqual(role.0, standard.0) || is_true(win, &minimized) || is_true(win, &fullscreen) { continue; }
                let Some(p) = geometry::<Point>(win, &position, 1) else { continue };
                let Some(s) = geometry::<Size>(win, &size, 2) else { continue };
                if !p.x.is_finite() || !p.y.is_finite() || !s.w.is_finite() || !s.h.is_finite() || s.w <= 0.0 || s.h <= 0.0 { continue; }
                let r = Rect { x: p.x.round() as i32, y: p.y.round() as i32, w: s.w.round() as i32, h: s.h.round() as i32 };
                let Some(to) = relocate_off(r, blocked, target) else { continue };
                let point = Point { x: to.x as f64, y: to.y as f64 };
                let value = Owned(AXValueCreate(1, (&point as *const Point).cast()));
                if !value.0.is_null() && AXUIElementSetAttributeValue(win, position.0, value.0) == 0 { moved += 1; }
            }
        }
        if moved > 0 { tracing::info!(moved, "rescued macOS windows off the passive shared panel"); }
        moved
    })
}
