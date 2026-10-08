//! Native, non-activating notice. AppKit objects stay on the GUI main thread.
use std::sync::{Mutex, OnceLock};

use kayiver_core::proto::Rect;
use objc2::{rc::Retained, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSPanel, NSScreen, NSTextAlignment,
    NSTextField, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

type State = Option<(Rect, String)>;
static PENDING: OnceLock<Mutex<Option<State>>> = OnceLock::new();

pub fn show(state: State) {
    *PENDING.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(state);
}

pub struct Notice {
    panel: Retained<NSPanel>,
    blocked: Option<Rect>,
    title: Retained<NSTextField>,
    message: Retained<NSTextField>,
}

impl Notice {
    /// Called by the existing GUI tick; nested Option preserves explicit hides
    /// and retains an early engine update until the shell is ready.
    pub fn update(notice: &mut Option<Self>) {
        let state = PENDING.get_or_init(|| Mutex::new(None)).lock().unwrap().take();
        let Some(state) = state else { return };
        let Some((r, msg)) = state else {
            if let Some(n) = notice.as_mut() { n.blocked = None; n.panel.orderOut(None); }
            return;
        };
        let mtm = MainThreadMarker::new().expect("passive notice needs GUI thread");
        // CG/AX coordinates have a top-left origin. AppKit uses the bottom-left
        // of the primary screen, including when B is above/left of A.
        let screens = NSScreen::screens(mtm);
        let Some(primary) = screens.firstObject() else { return };
        let frame = NSRect::new(
            NSPoint::new(r.x as f64, primary.frame().size.height - (r.y + r.h) as f64),
            NSSize::new(r.w as f64, r.h as f64),
        );
        let n = notice.get_or_insert_with(|| Self::new(mtm, frame));
        n.blocked = Some(r);
        n.panel.setFrame_display(frame, true);
        let width = (r.w as f64 - 80.0).max(1.0);
        n.title.setFrame(NSRect::new(NSPoint::new(40.0, r.h as f64 / 2.0 + 12.0), NSSize::new(width, 52.0)));
        n.message.setFrame(NSRect::new(NSPoint::new(40.0, r.h as f64 / 2.0 - 110.0), NSSize::new(width, 100.0)));
        n.message.setStringValue(&NSString::from_str(&msg));
        n.panel.orderFrontRegardless();
    }

    /// AX cannot set our own windows from a worker; use AppKit on main.
    pub fn rescue_editor(&self, window: &tao::window::Window) {
        use tao::platform::macos::WindowExtMacOS;
        let Some(blocked) = self.blocked else { return };
        let Some(mut target) = super::monitors().into_iter().find(|r|
            r.right() <= blocked.x || r.x >= blocked.right()
                || r.bottom() <= blocked.y || r.y >= blocked.bottom()
        ) else { return };
        target.y += 32;
        target.h -= 32;
        if target.w <= 0 || target.h <= 0 { return; }
        let mtm = MainThreadMarker::new().expect("own window rescue needs GUI thread");
        let screens = NSScreen::screens(mtm);
        let Some(primary) = screens.firstObject() else { return };
        let height = primary.frame().size.height;
        unsafe {
            let Some(ns) = (window.ns_window() as *const objc2_app_kit::NSWindow).as_ref() else { return };
            if ns.isMiniaturized() || ns.styleMask().contains(NSWindowStyleMask::FullScreen) { return; }
            let f = ns.frame();
            let win = Rect { x: f.origin.x.round() as i32,
                y: (height - f.origin.y - f.size.height).round() as i32,
                w: f.size.width.round() as i32, h: f.size.height.round() as i32 };
            if let Some(to) = kayiver_core::layout::relocate_off(win, blocked, target) {
                ns.setFrameTopLeftPoint(NSPoint::new(to.x as f64, height - to.y as f64));
            }
        }
    }

    fn new(mtm: MainThreadMarker, frame: NSRect) -> Self {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm), frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered, false,
        );
        // This overlay must never capture clicks, activate Kayiver, or appear
        // in the window cycle. It follows B across Spaces, including full screen.
        panel.setIgnoresMouseEvents(true);
        panel.setHidesOnDeactivate(false);
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setLevel(25); // above normal windows, below system/security UI
        panel.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle
            | NSWindowCollectionBehavior::FullScreenAuxiliary);
        panel.setBackgroundColor(Some(&NSColor::colorWithCalibratedRed_green_blue_alpha(0.04, 0.05, 0.08, 1.0)));
        panel.setAlphaValue(0.94);
        let title = NSTextField::labelWithString(&NSString::from_str("Kayıver — Bu ekran pasiftir"), mtm);
        title.setFont(Some(&NSFont::boldSystemFontOfSize(30.0)));
        title.setTextColor(Some(&NSColor::colorWithCalibratedRed_green_blue_alpha(0.2, 0.75, 0.85, 1.0)));
        title.setAlignment(NSTextAlignment::Center);
        let message = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        message.setFont(Some(&NSFont::systemFontOfSize(18.0)));
        message.setTextColor(Some(&NSColor::lightGrayColor()));
        message.setAlignment(NSTextAlignment::Center);
        if let Some(view) = panel.contentView() {
            view.addSubview(&title);
            view.addSubview(&message);
        }
        Self { panel, blocked: None, title, message }
    }
}
