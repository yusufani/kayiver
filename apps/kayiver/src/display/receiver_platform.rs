//! Display inventory only; this binary cannot observe or inject user input.
use kayiver_core::proto::Rect;
#[cfg(target_os="windows")]
mod inventory {
use super::*;
const DD_ATTACHED_TO_DESKTOP: u32 = 0x0000_0001;
const DD_MIRRORING_DRIVER: u32 = 0x0000_0008;

/// All display adapters (`\\.\DISPLAYn`) with their state flags, in
/// enumeration order. Mirroring pseudo-devices are filtered out.
fn display_devices() -> Vec<(String, u32)> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW, DISPLAY_DEVICEW};
    let mut out = Vec::new();
    let mut i = 0u32;
    loop {
        let mut dd = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), i, &mut dd, 0) };
        if !ok.as_bool() {
            break;
        }
        i += 1;
        if dd.StateFlags.0 & DD_MIRRORING_DRIVER != 0 {
            continue;
        }
        let name = String::from_utf16_lossy(&dd.DeviceName)
            .trim_end_matches('\0')
            .to_string();
        out.push((name, dd.StateFlags.0));
    }
    out
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn attached_displays() -> Vec<(String, Rect)> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{EnumDisplaySettingsW, DEVMODEW, ENUM_CURRENT_SETTINGS};
    let mut out = Vec::new();
    for (name, flags) in display_devices() {
        if flags & DD_ATTACHED_TO_DESKTOP == 0 {
            continue;
        }
        let wname = to_wide(&name);
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        let ok = unsafe { EnumDisplaySettingsW(PCWSTR(wname.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) };
        if ok.as_bool() {
            let pos = unsafe { dm.Anonymous1.Anonymous2.dmPosition };
            out.push((name, Rect { x: pos.x, y: pos.y, w: dm.dmPelsWidth as i32, h: dm.dmPelsHeight as i32 }));
        }
    }
    out
}

/// Monitor interface path survives display-order/primary changes; adapters do not.
pub fn identified_monitors() -> Vec<(Option<String>,Rect)> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{EnumDisplayDevicesW,DISPLAY_DEVICEW};
    attached_displays().into_iter().map(|(name,r)| {
        let wide=to_wide(&name);
        let mut device=DISPLAY_DEVICEW {cb:std::mem::size_of::<DISPLAY_DEVICEW>() as u32,..Default::default()};
        let ok=unsafe {EnumDisplayDevicesW(PCWSTR(wide.as_ptr()),0,&mut device,1)};
        let id=String::from_utf16_lossy(&device.DeviceID).trim_end_matches('\0').to_string();
        (if ok.as_bool() && !id.is_empty() {Some(id)}else{None},r)
    }).collect()
}


}
#[cfg(target_os="windows")]
pub use inventory::identified_monitors;
#[cfg(not(target_os="windows"))]
pub fn identified_monitors()->Vec<(Option<String>,Rect)>{Vec::new()}
pub fn monitors()->Vec<Rect>{identified_monitors().into_iter().map(|(_,r)|r).collect()}
pub fn init(){
 #[cfg(target_os="windows")]
 unsafe {let _=windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);}
}
// These adapters are only used by the Mac source cleanup path. Keeping them
// fail-closed prevents the receive-only executable acquiring cursor/window APIs.
pub fn rescue_windows_off(_:Rect)->usize{unreachable!("receiver has no source windows")}
pub fn cursor_pos()->(i32,i32){unreachable!("receiver cannot inspect cursor")}
pub fn warp_cursor_settled(_:i32,_:i32){unreachable!("receiver cannot move cursor")}

#[cfg(target_os="macos")]
pub fn apply_arrangement(_: &[Rect])->anyhow::Result<bool>{anyhow::bail!("receiver cannot arrange a Mac desktop")}
