//! Placeholder backend for platforms without an implementation yet
//! (Linux is planned — see docs/ROADMAP.md).

use std::sync::Arc;

use anyhow::{bail, Result};
use drift_core::proto::{MouseButton, Rect};
use tokio::sync::mpsc::UnboundedSender;

use crate::engine::Captured;
use crate::platform::CaptureCtl;

pub fn desktop_bounds() -> Rect {
    Rect { x: 0, y: 0, w: 1920, h: 1080 }
}

pub fn monitors() -> Vec<Rect> {
    vec![desktop_bounds()]
}

pub fn displays() -> Vec<(u32, String, Option<u16>)> {
    vec![]
}

pub fn set_display_input(_index: u32, _value: u16) -> anyhow::Result<()> {
    anyhow::bail!("display switching not supported on this platform")
}

pub fn set_display_enabled(_index: u32, _enabled: bool) -> anyhow::Result<()> {
    anyhow::bail!("display enable/disable not supported on this platform")
}

pub fn init() {}

pub fn ensure_permissions() -> Result<()> {
    Ok(())
}

pub fn doctor_permissions() {
    println!("  platform    : unsupported (input capture/injection unavailable)");
}

pub fn start_capture(_ctl: Arc<CaptureCtl>, _tx: UnboundedSender<Captured>) -> Result<()> {
    bail!("input capture is not implemented on this platform")
}

pub fn set_forwarding_visuals(_on: bool) {}

pub fn warp_cursor(_x: i32, _y: i32) {}

#[allow(dead_code)]
pub fn cursor_pos() -> (i32, i32) {
    (0, 0)
}

pub struct Injector;

impl Injector {
    pub fn new() -> Result<Self> {
        bail!("input injection is not implemented on this platform")
    }
    pub fn mouse_to(&mut self, _x: i32, _y: i32, _dx: i32, _dy: i32) {}
    pub fn button(&mut self, _b: MouseButton, _pressed: bool) {}
    pub fn wheel(&mut self, _dx: i32, _dy: i32) {}
    pub fn key(&mut self, _hid: u16, _pressed: bool) {}
    pub fn release_all(&mut self) {}
}
