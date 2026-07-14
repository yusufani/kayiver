//! macOS app shell: menu-bar (tray) icon + native editor window.
//!
//! The tao event loop must own the main thread (NSApplication), so the
//! engine (host router + capture + embedded editor server) moves to a
//! background thread. The editor window is a WKWebView (wry) pointed at the
//! embedded server — no external browser involved.
//!
//! Dock behaviour: while the editor window is open the app is a normal
//! `Regular` app (Dock icon + app switcher); when the window is closed it
//! drops to `Accessory` so only the menu-bar icon remains.

#![cfg(target_os = "macos")]

use anyhow::Result;
use kayiver_core::config::Config;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopWindowTarget};
use tao::platform::macos::{
    ActivationPolicy, EventLoopExtMacOS, EventLoopWindowTargetExtMacOS, WindowExtMacOS,
};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};
use wry::WebView;

/// Template icon (black + alpha), retina size; macOS tints it to the bar.
const MENUBAR_ICON: &[u8] = include_bytes!("../../../assets/icons/menubarTemplate@2x.png");

/// The transparent overlay page: a canvas drawing a blue halo around the live
/// cursor and a ~3 s blue glow along the edge the cursor just crossed.
const OVERLAY_HTML: &str = r#"<!doctype html><meta charset=utf-8>
<style>html,body{margin:0;height:100%;background:transparent;overflow:hidden}canvas{display:block}</style>
<canvas id=c></canvas>
<script>
const cv=document.getElementById('c'),g=cv.getContext('2d');
let W,H; function rs(){W=cv.width=innerWidth;H=cv.height=innerHeight} rs(); addEventListener('resize',rs);
let cx=-9999,cy=-9999,seen=0,flashEdge=0,flashT=0;
window.tick=(x,y,fl)=>{ cx=x;cy=y;seen=performance.now(); if(fl){flashEdge=fl;flashT=performance.now();} };
function draw(){
  g.clearRect(0,0,W,H); const now=performance.now();
  if(now-seen<1600){
    const b=8+4*Math.sin(now/160), p=(now%900)/900;
    g.beginPath();g.arc(cx,cy,16+b,0,7);g.lineWidth=3;g.strokeStyle='rgba(96,165,250,.85)';g.stroke();
    g.beginPath();g.arc(cx,cy,16+b+22*p,0,7);g.lineWidth=2;g.strokeStyle='rgba(59,130,246,'+(.55*(1-p))+')';g.stroke();
  }
  if(flashEdge){
    const e=(now-flashT)/3000;
    if(e>=1){flashEdge=0;}else{
      const a=Math.sin(Math.min(e,1)*Math.PI)*0.85, T=Math.round(H*0.16);
      let gr;
      if(flashEdge==1){gr=g.createLinearGradient(0,0,80,0);grStops(gr,a);g.fillStyle=gr;g.fillRect(0,0,80,H);}
      if(flashEdge==2){gr=g.createLinearGradient(W,0,W-80,0);grStops(gr,a);g.fillStyle=gr;g.fillRect(W-80,0,80,H);}
      if(flashEdge==3){gr=g.createLinearGradient(0,0,0,80);grStops(gr,a);g.fillStyle=gr;g.fillRect(0,0,W,80);}
      if(flashEdge==4){gr=g.createLinearGradient(0,H,0,H-80);grStops(gr,a);g.fillStyle=gr;g.fillRect(0,H-80,W,80);}
    }
  }
  requestAnimationFrame(draw);
}
function grStops(gr,a){gr.addColorStop(0,'rgba(59,130,246,'+a+')');gr.addColorStop(1,'rgba(59,130,246,0)');}
draw();
</script>"#;

#[derive(Debug)]
enum UserEvent {
    Menu(MenuEvent),
    /// Periodic status summary from the local API for the tray.
    Status { line: String, warn: bool },
    /// ~60 fps overlay pump: live cursor position + a one-shot crossing flash.
    Overlay { x: i32, y: i32, flash: u8 },
}

struct MenuIds {
    open: tray_icon::menu::MenuId,
    toggle_shared: tray_icon::menu::MenuId,
    quit: tray_icon::menu::MenuId,
}

/// Summarize /api/status into one tray line + a warning flag.
fn status_summary() -> (String, bool) {
    let parsed = crate::ui::local_api("GET", "/api/status", None)
        .ok()
        .and_then(|(code, body)| if code == 200 { serde_json::from_str::<serde_json::Value>(&body).ok() } else { None });
    let Some(v) = parsed else {
        return ("Motor başlatılıyor / izin bekleniyor…".into(), true);
    };
    if !v["running"].as_bool().unwrap_or(false) {
        return ("Motor çalışmıyor (izin bekleniyor olabilir)".into(), true);
    }
    let peers = v["peers"].as_object().cloned().unwrap_or_default();
    if peers.is_empty() {
        return ("Eş bekleniyor…".into(), true);
    }
    let mut parts = Vec::new();
    let mut any_down = false;
    for (name, p) in peers {
        if p["connected"].as_bool().unwrap_or(false) {
            match p["rtt_ms"].as_f64() {
                Some(rtt) => parts.push(format!("{name}: bağlı ({rtt:.1} ms)")),
                None => parts.push(format!("{name}: bağlı")),
            }
        } else {
            any_down = true;
            parts.push(format!("{name}: çevrimdışı"));
        }
    }
    (parts.join(" · "), any_down)
}

/// `kayiver run` (host mode): engine on a background thread, tray + window
/// shell on the main thread. Never returns.
pub fn run_host(cfg: Config) -> Result<()> {
    // Serve the editor right away (independent of permissions) so the window
    // has content immediately; it shows "not running" until the host is up.
    // The host's own serve_forever then just fails to re-bind (harmless).
    std::thread::Builder::new().name("kayiver-ui".into()).spawn(|| {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _ = rt.block_on(crate::ui::serve_forever());
    })?;

    // Permissions are waited on here (not on the main thread) so the tray +
    // window appear immediately; the editor shows "not running" until the
    // permissions are granted and the host comes up.
    std::thread::Builder::new().name("kayiver-engine".into()).spawn(move || {
        if let Err(e) = crate::platform::ensure_permissions().and_then(|_| crate::engine::host::run(cfg)) {
            eprintln!("kayiver engine exited: {e:#}");
            // Keep the GUI alive so the user can read the error / retry.
        }
    })?;
    run_shell(true)
}

/// `kayiver ui`: no engine here. If a running kayiver already serves the
/// editor we just open a window onto it; otherwise serve it ourselves.
pub fn run_editor() -> Result<()> {
    if std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", crate::ui::UI_PORT).parse().unwrap(),
        std::time::Duration::from_millis(400),
    )
    .is_err()
    {
        std::thread::Builder::new().name("kayiver-ui-server".into()).spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let _ = rt.block_on(crate::ui::serve_forever());
        })?;
    }
    run_shell(true)
}

fn run_shell(_open_window_now: bool) -> Result<()> {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // Start as a menu-bar app (no Dock icon); we flip to Regular whenever a
    // window is open so it also shows in the Dock.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    // Feed the tray a status summary every few seconds (connection state,
    // latency, warnings) so problems are visible without opening the editor.
    let status_proxy = event_loop.create_proxy();
    std::thread::Builder::new().name("kayiver-tray-status".into()).spawn(move || loop {
        let (line, warn) = status_summary();
        if status_proxy.send_event(UserEvent::Status { line, warn }).is_err() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    })?;

    // ~60 fps overlay pump: pushes the live cursor position + any pending
    // crossing flash to the overlay canvas.
    let overlay_proxy = event_loop.create_proxy();
    std::thread::Builder::new().name("kayiver-overlay".into()).spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(16));
        let (x, y) = crate::platform::cursor_pos();
        let flash = crate::ui::take_cross_flash();
        if overlay_proxy.send_event(UserEvent::Overlay { x, y, flash }).is_err() {
            return;
        }
    })?;

    let (tray, ids, status_item) = build_tray()?;

    let mut editor: Option<(Window, WebView)> = None;
    let mut overlay: Option<(Window, WebView, (i32, i32))> = None;
    // Open the window once on launch so the app is visible (and in the Dock);
    // closing it later drops back to menu-bar-only.
    let mut open_pending = true;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;

        if open_pending {
            open_pending = false;
            match open_editor_window(target) {
                Ok(w) => {
                    show_in_dock(target, &w.0);
                    editor = Some(w);
                }
                Err(e) => eprintln!("editor window failed: {e:#}"),
            }
            // Overlay is OFF: the transparent window rendered opaque white and
            // covered a whole monitor. Disabled until it's verified see-through
            // on-screen. `KAYIVER_OVERLAY=1` opts in for testing.
            if std::env::var("KAYIVER_OVERLAY").as_deref() == Ok("1") {
                overlay = open_overlay(target).map_err(|e| eprintln!("overlay failed: {e:#}")).ok();
            }
        }

        match event {
            Event::UserEvent(UserEvent::Overlay { x, y, flash }) => {
                if let Some((_, wv, origin)) = &overlay {
                    let _ = wv.evaluate_script(&format!(
                        "window.tick&&tick({},{},{flash})",
                        x - origin.0,
                        y - origin.1
                    ));
                }
            }
            Event::UserEvent(UserEvent::Menu(m)) => {
                if m.id == ids.open {
                    match &editor {
                        Some((w, _)) => {
                            show_in_dock(target, w);
                            w.set_focus();
                        }
                        None => match open_editor_window(target) {
                            Ok(w) => {
                                show_in_dock(target, &w.0);
                                editor = Some(w);
                            }
                            Err(e) => eprintln!("editor window failed: {e:#}"),
                        },
                    }
                } else if m.id == ids.toggle_shared {
                    // The running host owns the logic; go through the local API.
                    let _ = crate::ui::local_api("POST", "/api/shared", Some(r#"{"owner":"toggle"}"#));
                } else if m.id == ids.quit {
                    std::process::exit(0);
                }
            }
            Event::UserEvent(UserEvent::Status { line, warn }) => {
                status_item.set_text(line.clone());
                let _ = tray.set_tooltip(Some(format!("Kayıver — {line}")));
                // A "⚠" next to the menu-bar icon whenever something is off
                // (engine down, peer offline) — visible at a glance.
                let _ = tray.set_title(if warn { Some("⚠") } else { None });
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                // Window closed → drop it and retreat to the menu bar only.
                editor = None;
                target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
                target.set_dock_visibility(false);
            }
            _ => {}
        }
    });
}

/// Bring the app into the Dock + app switcher and focus the window.
fn show_in_dock(target: &EventLoopWindowTarget<UserEvent>, window: &Window) {
    target.set_activation_policy_at_runtime(ActivationPolicy::Regular);
    target.set_dock_visibility(true);
    target.show_application();
    window.set_focus();
}

fn build_tray() -> Result<(TrayIcon, MenuIds, MenuItem)> {
    let menu = Menu::new();
    let status = MenuItem::new("Durum alınıyor…", false, None);
    let open = MenuItem::new("Kayıver'ı Aç", true, None);
    let toggle_shared = MenuItem::new("Ortak Monitörü Değiştir\t⌘⌥M", true, None);
    let quit = MenuItem::new("Kayıver'dan Çık", true, None);
    menu.append(&status)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&open)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&toggle_shared)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit)?;

    let ids = MenuIds {
        open: open.id().clone(),
        toggle_shared: toggle_shared.id().clone(),
        quit: quit.id().clone(),
    };

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Kayıver")
        .with_icon(load_menubar_icon()?)
        .with_icon_as_template(true)
        .build()?;
    Ok((tray, ids, status))
}

fn load_menubar_icon() -> Result<tray_icon::Icon> {
    let decoder = png::Decoder::new(std::io::Cursor::new(MENUBAR_ICON));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(44 * 44 * 4)];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    Ok(tray_icon::Icon::from_rgba(buf, info.width, info.height)?)
}

fn open_editor_window(target: &tao::event_loop::EventLoopWindowTarget<UserEvent>) -> Result<(Window, WebView)> {
    let window = WindowBuilder::new()
        .with_title("Kayıver")
        .with_inner_size(tao::dpi::LogicalSize::new(1060.0, 720.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(720.0, 520.0))
        .build(target)?;
    let webview = wry::WebViewBuilder::new()
        .with_url(crate::ui::url())
        .build(&window)?;
    Ok((window, webview))
}

/// A transparent, click-through, always-on-top overlay covering the primary
/// screen, rendering the crossing/cursor animation. Returns the origin (logical
/// top-left) so cursor coords can be mapped to window-local canvas space.
fn open_overlay(
    target: &tao::event_loop::EventLoopWindowTarget<UserEvent>,
) -> Result<(Window, WebView, (i32, i32))> {
    let mon = target.primary_monitor().or_else(|| target.available_monitors().next());
    let (pos, size) = match &mon {
        Some(m) => (m.position().to_logical::<f64>(m.scale_factor()), m.size().to_logical::<f64>(m.scale_factor())),
        None => (tao::dpi::LogicalPosition::new(0.0, 0.0), tao::dpi::LogicalSize::new(1440.0, 900.0)),
    };
    let window = WindowBuilder::new()
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top(true)
        .with_position(pos)
        .with_inner_size(size)
        .with_focused(false)
        .build(target)?;

    // Click-through + float above everything, and don't take part in window
    // cycling. Uses the underlying NSWindow directly.
    unsafe {
        use objc2::msg_send;
        let ns = window.ns_window() as *mut objc2::runtime::AnyObject;
        if !ns.is_null() {
            let _: () = msg_send![ns, setIgnoresMouseEvents: true];
            let _: () = msg_send![ns, setLevel: 2_147_483_631i64]; // ~ screen-saver level
            let _: () = msg_send![ns, setCollectionBehavior: 1u64 << 0 | 1u64 << 4]; // canJoinAllSpaces | stationary
        }
    }

    let webview = wry::WebViewBuilder::new()
        .with_transparent(true)
        .with_html(OVERLAY_HTML)
        .build(&window)?;
    Ok((window, webview, (pos.x as i32, pos.y as i32)))
}
