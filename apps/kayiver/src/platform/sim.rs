//! Virtual-machine backend (`--features sim`).
//!
//! Everything the OS normally provides — monitors, the physical cursor,
//! input injection — is a scriptable in-process world, while the whole engine
//! above it stays REAL: real router, real Noise handshake, real TCP session.
//! A test spawns two `kayiver run` processes (each with its own
//! `KAYIVER_CONFIG_DIR`, so each acts like its own machine), then drives and
//! observes them over a JSON-lines control socket:
//!
//!   KAYIVER_SIM_MONITORS="0,0,2560,1440;2560,0,2560,1440"  initial displays
//!   KAYIVER_SIM_CTL=7101                                   control port
//!
//! Control ops (one JSON object per line, one JSON reply per line):
//!   {"op":"warp","x":..,"y":..}        physically move the virtual cursor
//!   {"op":"set_monitors","monitors":[[x,y,w,h],..]}   change the displays
//!   {"op":"edge","edge":"right","ratio":0.5}   hit an armed portal edge
//!   {"op":"input_move","dx":..,"dy":..}        forwarded motion (host side)
//!   {"op":"input_key","key":..,"pressed":..}   forwarded key
//!   {"op":"hotkey"}                            shared-monitor hotkey
//!   {"op":"state"}                             cursor/forwarding/portals/...
//!   {"op":"injected"}                          drain recorded injections

use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use kayiver_core::layout::Edge;
use kayiver_core::proto::{InputEvent, MouseButton, Rect};
use tokio::sync::mpsc::UnboundedSender;

use crate::engine::Captured;
use crate::platform::CaptureCtl;

struct SimWorld {
    monitors: Vec<Rect>,
    cursor: (i32, i32),
    parking: super::cursor_parking::CursorParking,
    /// Every injection the engine performed, as JSON for the harness.
    injected: Vec<serde_json::Value>,
    capture: Option<(Arc<CaptureCtl>, UnboundedSender<Captured>)>,
    clipboard: Option<String>,
    clip_seq: u64,
    handoff_delay_ms: u64,
    passive_notice: Option<Rect>,
    reject_injection: bool,
    reject_containment: bool,
    pending_capture: Vec<Captured>,
    last_warp_driven: bool,
}

static WORLD: OnceLock<Mutex<SimWorld>> = OnceLock::new();

fn world() -> &'static Mutex<SimWorld> {
    WORLD.get_or_init(|| {
        let monitors = std::env::var("KAYIVER_SIM_MONITORS")
            .ok()
            .map(|s| parse_monitors(&s))
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| vec![Rect { x: 0, y: 0, w: 2560, h: 1440 }]);
        Mutex::new(SimWorld {
            monitors,
            cursor: (100, 100),
            parking: super::cursor_parking::CursorParking::new(),
            injected: Vec::new(),
            capture: None,
            clipboard: None,
            clip_seq: 0,
            handoff_delay_ms: 0,
            passive_notice: None,
            reject_injection: false,
            reject_containment: false,
            pending_capture: Vec::new(),
            last_warp_driven: false,
        })
    })
}

pub fn show_passive_notice(state: Option<(Rect, String)>) {
    world().lock().unwrap().passive_notice = state.map(|(rect, _)| rect);
}

fn parse_monitors(s: &str) -> Vec<Rect> {
    s.split(';')
        .filter_map(|m| {
            let v: Vec<i32> = m.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            (v.len() == 4).then(|| Rect { x: v[0], y: v[1], w: v[2], h: v[3] })
        })
        .collect()
}

fn record(kind: &str, fields: serde_json::Value) {
    let mut w = world().lock().unwrap();
    let mut obj = serde_json::json!({ "kind": kind });
    if let (Some(o), Some(f)) = (obj.as_object_mut(), fields.as_object()) {
        for (k, v) in f {
            o.insert(k.clone(), v.clone());
        }
    }
    w.injected.push(obj);
}

// ------------------------------------------------------------ surface ----

pub fn desktop_bounds() -> Rect {
    let mons = monitors();
    let min_x = mons.iter().map(|m| m.x).min().unwrap_or(0);
    let min_y = mons.iter().map(|m| m.y).min().unwrap_or(0);
    let max_x = mons.iter().map(|m| m.right()).max().unwrap_or(1920);
    let max_y = mons.iter().map(|m| m.bottom()).max().unwrap_or(1080);
    Rect { x: min_x, y: min_y, w: max_x - min_x, h: max_y - min_y }
}

/// Windows-only: rescuing windows off a hidden monitor is a Win32 window
/// manager job. Elsewhere the OS keeps its own arrangement, so nothing to do.
pub fn rescue_windows_off(_blocked: Rect) -> usize {
    0
}

pub fn handoff_delay_ms() -> u64 { world().lock().unwrap().handoff_delay_ms }

pub fn monitors() -> Vec<Rect> {
    world().lock().unwrap().monitors.clone()
}

pub fn apply_arrangement(desired: &[Rect]) -> Result<bool> {
    let mut w = world().lock().unwrap();
    match kayiver_core::layout::arranged(&w.monitors, desired) {
        Some(m) => {
            w.monitors = m;
            Ok(true)
        }
        None => Ok(false),
    }
}

pub fn displays() -> Vec<(u32, String, Option<u16>)> {
    monitors()
        .iter()
        .enumerate()
        .map(|(i, m)| (i as u32, format!("SimDisplay {}x{}", m.w, m.h), None))
        .collect()
}

pub fn set_display_enabled(_index: u32, _expect: Option<Rect>, _enabled: bool) -> Result<()> {
    Ok(())
}

pub fn display_disabled(_index: u32) -> Option<bool> {
    None
}

pub fn init() {
    let _ = world(); // parse KAYIVER_SIM_MONITORS before anything reads geometry
    if let Ok(port) = std::env::var("KAYIVER_SIM_CTL") {
        if let Ok(port) = port.parse::<u16>() {
            std::thread::Builder::new()
                .name("kayiver-sim-ctl".into())
                .spawn(move || control_server(port))
                .ok();
        }
    }
}

pub fn ensure_permissions() -> Result<()> {
    Ok(())
}

pub fn doctor_permissions() {
    println!("  permissions : simulated");
}

pub fn start_capture(ctl: Arc<CaptureCtl>, tx: UnboundedSender<Captured>) -> Result<()> {
    world().lock().unwrap().capture = Some((ctl, tx));
    Ok(())
}

pub fn set_forwarding_visuals(on: bool) -> bool {
    let mut w=world().lock().unwrap();
    if on && w.reject_containment {return false;}
    let native=w.cursor;w.parking.set_remote(on,native);true
}

pub fn warp_cursor(x: i32, y: i32) {
    let mut w=world().lock().unwrap();
    w.last_warp_driven=w.capture.as_ref().is_some_and(|(ctl,_)|ctl.driven.load(Ordering::SeqCst));
    w.cursor = (x, y);
}

pub fn warp_cursor_settled(x: i32, y: i32) {
    warp_cursor(x, y);
}

pub fn cursor_pos() -> (i32, i32) {
    world().lock().unwrap().cursor
}

pub struct Injector;

impl Injector {
    pub fn new() -> Result<Self> {
        Ok(Injector)
    }
    // Buttons use the current native cursor on this backend.
    pub fn rebase_position(&mut self, _x: i32, _y: i32) {}
    pub fn mouse_to(&mut self, x: i32, y: i32, dx: i32, dy: i32) -> bool {
        if world().lock().unwrap().reject_injection {return false;}
        world().lock().unwrap().cursor = (x, y);
        record("mouse_to", serde_json::json!({ "x": x, "y": y, "dx": dx, "dy": dy }));
        true
    }
    pub fn button(&mut self, b: MouseButton, pressed: bool) {
        record("button", serde_json::json!({ "button": format!("{b:?}"), "pressed": pressed }));
    }
    pub fn wheel(&mut self, dx: i32, dy: i32) {
        record("wheel", serde_json::json!({ "dx": dx, "dy": dy }));
    }
    pub fn key(&mut self, hid: u16, pressed: bool) {
        record("key", serde_json::json!({ "key": hid, "pressed": pressed }));
    }
    pub fn release_all(&mut self) {
        record("release_all", serde_json::json!({}));
    }
}

pub fn get_clipboard() -> Option<String> {
    world().lock().unwrap().clipboard.clone()
}

pub fn set_clipboard(text: &str) {
    let mut w = world().lock().unwrap();
    w.clipboard = Some(text.to_string());
    w.clip_seq += 1;
}

pub fn clipboard_seq() -> u64 {
    world().lock().unwrap().clip_seq
}

pub fn drag_url() -> Option<String> {
    None
}

pub fn open_url(url: &str) {
    record("open_url", serde_json::json!({ "url": url }));
}

pub fn reveal_path(path: &str) {
    record("reveal_path", serde_json::json!({ "path": path }));
}

pub fn get_clipboard_file() -> Option<String> {
    None
}

// ----------------------------------------------------- control server ----

fn control_server(port: u16) {
    let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) else {
        tracing::error!("sim ctl: port {port} busy");
        return;
    };
    tracing::info!("sim ctl listening on 127.0.0.1:{port}");
    for stream in listener.incoming().flatten() {
        let _ = stream.set_nodelay(true);
        let mut out = match stream.try_clone() {
            Ok(o) => o,
            Err(_) => continue,
        };
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(cmd) => handle(cmd),
                Err(e) => serde_json::json!({ "ok": false, "error": format!("bad json: {e}") }),
            };
            if writeln!(out, "{reply}").is_err() {
                break;
            }
        }
    }
}

fn parse_edge(s: &str) -> Option<Edge> {
    match s {
        "left" => Some(Edge::Left),
        "right" => Some(Edge::Right),
        "top" => Some(Edge::Top),
        "bottom" => Some(Edge::Bottom),
        _ => None,
    }
}

fn capture_handles() -> Option<(Arc<CaptureCtl>, UnboundedSender<Captured>)> {
    world().lock().unwrap().capture.clone()
}

fn handle(cmd: serde_json::Value) -> serde_json::Value {
    let ok = serde_json::json!({ "ok": true });
    let op = cmd.get("op").and_then(|o| o.as_str()).unwrap_or("");
    match op {
        "warp" => {
            let (x, y) = (
                cmd["x"].as_i64().unwrap_or(0) as i32,
                cmd["y"].as_i64().unwrap_or(0) as i32,
            );
            let previous=world().lock().unwrap().cursor;
            world().lock().unwrap().cursor=(x,y);
            if let Some((ctl,tx))=capture_handles() {
                let _gate=ctl.motion_gate.lock().unwrap();
                super::route_motion(&ctl,&tx,(x,y),x-previous.0,y-previous.1);
            }
            ok
        }
        "native_drift" => {
            let native=(cmd["x"].as_i64().unwrap() as i32,cmd["y"].as_i64().unwrap() as i32);
            let (dx,dy)=(cmd["dx"].as_i64().unwrap() as i32,cmd["dy"].as_i64().unwrap() as i32);
            world().lock().unwrap().cursor=native;
            let Some((ctl,tx))=capture_handles() else {return serde_json::json!({"ok":false});};
            let _gate=ctl.motion_gate.lock().unwrap();
            super::route_motion(&ctl,&tx,native,dx,dy);
            if ctl.forwarding.load(Ordering::SeqCst) {
                let mut w=world().lock().unwrap();if let Some(anchor)=w.parking.correction(native) {w.cursor=anchor;}
            }
            ok
        }
        "arrange" => {
            // The editor's Save for one machine: {"machine": "...", "monitors": [[x,y,w,h], ...]}
            let mons: Vec<Rect> = cmd["monitors"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|m| {
                            let v: Vec<i32> =
                                m.as_array()?.iter().filter_map(|n| n.as_i64().map(|n| n as i32)).collect();
                            (v.len() == 4).then(|| Rect { x: v[0], y: v[1], w: v[2], h: v[3] })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let machine = cmd["machine"].as_str().unwrap_or_default().to_string();
            if mons.is_empty() || machine.is_empty() {
                return serde_json::json!({ "ok": false, "error": "need machine + monitors" });
            }
            if crate::ui::send_cmd(crate::ui::UiCmd::Arrange { machine, monitors: mons }) {
                ok
            } else {
                serde_json::json!({ "ok": false, "error": "router not running" })
            }
        }
        "set_monitors" => {
            let mons: Vec<Rect> = cmd["monitors"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|m| {
                            let v: Vec<i32> = m
                                .as_array()?
                                .iter()
                                .filter_map(|n| n.as_i64().map(|n| n as i32))
                                .collect();
                            (v.len() == 4).then(|| Rect { x: v[0], y: v[1], w: v[2], h: v[3] })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if mons.is_empty() {
                return serde_json::json!({ "ok": false, "error": "no monitors" });
            }
            world().lock().unwrap().monitors = mons;
            ok
        }
        "edge" => {
            let Some(edge)=cmd["edge"].as_str().and_then(parse_edge) else {return serde_json::json!({"ok":false});};
            let ratio=cmd["ratio"].as_f64().unwrap_or(0.5) as f32;
            let Some((ctl,tx))=capture_handles() else {return serde_json::json!({"ok":false});};
            let (x,y)=kayiver_core::layout::point_on_edge(ctl.bounds(),edge,ratio,0);
            let (dx,dy)=match edge {Edge::Left=>(-2,0),Edge::Right=>(2,0),Edge::Top=>(0,-2),Edge::Bottom=>(0,2)};
            let previous=world().lock().unwrap().cursor;
            let _gate=ctl.motion_gate.lock().unwrap();
            world().lock().unwrap().cursor=(x,y);
            super::route_motion(&ctl,&tx,(x,y),x-previous.0,y-previous.1);
            super::route_motion(&ctl,&tx,(x+dx,y+dy),dx,dy);
            ok
        }
        "delay_shared_return" => {
            world().lock().unwrap().handoff_delay_ms = cmd["ms"].as_u64().unwrap_or(0);
            ok
        }
        "capture_motion" => {
            // A native callback's position and raw delta can disagree just
            // after a warp. Exercise that same portal predicate as macOS.
            let Some((ctl, tx)) = capture_handles() else {
                return serde_json::json!({ "ok": false, "error": "no capture" });
            };
            let x = cmd["x"].as_i64().unwrap_or(0) as i32;
            let y = cmd["y"].as_i64().unwrap_or(0) as i32;
            let dx = cmd["dx"].as_i64().unwrap_or(0) as i32;
            let dy = cmd["dy"].as_i64().unwrap_or(0) as i32;
            let _gate=ctl.motion_gate.lock().unwrap();
            let intervened=super::route_motion(&ctl,&tx,(x,y),dx,dy);
            serde_json::json!({"ok":true,"intervened":intervened})
        }
        "defer_motion" => {
            // Reproduce capture claiming control before the router selects its
            // queued Motion event. No alternate geometry or routing behavior.
            let Some((ctl,_))=capture_handles() else {return serde_json::json!({"ok":false});};
            let current=world().lock().unwrap().cursor;
            let dx=cmd["dx"].as_i64().unwrap_or(0) as i32;
            let dy=cmd["dy"].as_i64().unwrap_or(0) as i32;
            let (tx,mut rx)=tokio::sync::mpsc::unbounded_channel();
            let _gate=ctl.motion_gate.lock().unwrap();
            super::route_motion(&ctl,&tx,(current.0+dx,current.1+dy),dx,dy);
            while let Ok(ev)=rx.try_recv() {world().lock().unwrap().pending_capture.push(ev);}
            ok
        }
        "flush_capture" => {
            let Some((_,tx))=capture_handles() else {return serde_json::json!({"ok":false});};
            let pending=std::mem::take(&mut world().lock().unwrap().pending_capture);
            for ev in pending {let _=tx.send(ev);}
            ok
        }
        "input_move" | "queued_move" => {
            let (dx, dy) = (
                cmd["dx"].as_i64().unwrap_or(0) as i32,
                cmd["dy"].as_i64().unwrap_or(0) as i32,
            );
            let Some((ctl, tx)) = capture_handles() else {
                return serde_json::json!({ "ok": false, "error": "no capture" });
            };
            let current=world().lock().unwrap().cursor;
            let _gate=ctl.motion_gate.lock().unwrap();
            if !super::route_motion(&ctl,&tx,(current.0+dx,current.1+dy),dx,dy) {
                let mut w=world().lock().unwrap();
                w.cursor=super::clamp_monitor_move(&w.monitors,current,(current.0+dx,current.1+dy));
            }
            ok
        }
        "input_key" => {
            let key = cmd["key"].as_u64().unwrap_or(4) as u16;
            let pressed = cmd["pressed"].as_bool().unwrap_or(true);
            let Some((ctl, tx)) = capture_handles() else {
                return serde_json::json!({ "ok": false, "error": "no capture" });
            };
            let _gate=ctl.motion_gate.lock().unwrap();
            super::capture_input(&ctl,&tx,InputEvent::Key {key,pressed});
            ok
        }
        "reject_containment" => {world().lock().unwrap().reject_containment=cmd["enabled"].as_bool().unwrap_or(true);ok}
        "reject_injection" => {world().lock().unwrap().reject_injection=cmd["enabled"].as_bool().unwrap_or(true);ok}
        "programmatic_move" => {warp_cursor_settled(cmd["x"].as_i64().unwrap_or(0) as i32,cmd["y"].as_i64().unwrap_or(0) as i32);ok}
        "input_button" => {
            if let Some((ctl,tx))=capture_handles() {let _gate=ctl.motion_gate.lock().unwrap();super::capture_input(&ctl,&tx,InputEvent::MouseButton {button:MouseButton::Left,pressed:cmd["pressed"].as_bool().unwrap_or(false)});}
            ok
        }
        "hotkey" => {
            let Some((_, tx)) = capture_handles() else {
                return serde_json::json!({ "ok": false, "error": "no capture" });
            };
            let _ = tx.send(Captured::SharedHotkey);
            ok
        }
        "state" => {
            let w = world().lock().unwrap();
            let navigation = w.capture.as_ref().map(|(ctl,_)| { let nav=ctl.navigation.lock().unwrap(); serde_json::json!({"revision":nav.topology.revision,"surfaces":nav.topology.surfaces.len(),"layout":nav.topology,"seams":nav.topology.seams.len(),"control":match nav.control {super::navigation::Control::Local=>"local",super::navigation::Control::Remote(_)=>"remote",super::navigation::Control::Driven(_)=>"driven",super::navigation::Control::Recovering=>"recovering"}}) });
            let capture_bounds = w.capture.as_ref().map(|(ctl,_)| { let r=ctl.bounds(); [r.x,r.y,r.w,r.h] });
            let (forwarding, driven, portals, blocked) = match &w.capture {
                Some((ctl, _)) => (
                    ctl.forwarding.load(Ordering::SeqCst),
                    ctl.driven.load(Ordering::SeqCst),
                    ctl.portals.read().unwrap().iter().map(|e| format!("{e:?}")).collect::<Vec<_>>(),
                    ctl.blocked.read().unwrap().map(|r| [r.x, r.y, r.w, r.h]),
                ),
                None => (false, false, Vec::new(), None),
            };
            serde_json::json!({
                "ok": true,
                "cursor": [w.cursor.0, w.cursor.1],
                "monitors": w.monitors.iter().map(|m| [m.x, m.y, m.w, m.h]).collect::<Vec<_>>(),
                "forwarding": forwarding,
                "driven": driven,
                "portals": portals,
                "blocked": blocked,
                "passive_notice": w.passive_notice.map(|r| [r.x, r.y, r.w, r.h]),
                "injected_len": w.injected.len(),
                "last_warp_driven": w.last_warp_driven,
                "capture_bounds": capture_bounds,
                "navigation": navigation,
            })
        }
        "injected" => {
            let mut w = world().lock().unwrap();
            let events = std::mem::take(&mut w.injected);
            serde_json::json!({ "ok": true, "events": events })
        }
        other => serde_json::json!({ "ok": false, "error": format!("unknown op '{other}'") }),
    }
}
