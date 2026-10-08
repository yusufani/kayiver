//! macOS app shell: menu-bar (tray) icon + native editor window.
//!
//! The tao event loop must own the main thread (NSApplication), so the
//! engine (host router + capture + embedded editor server) moves to a
//! background thread. The editor window is a WKWebView (wry) pointed at the
//! embedded server — no external browser involved.
//!
//! The app stays an `Accessory`: the menu bar and editor are available
//! without a Dock icon. A launcher reopen brings the editor to the front.

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
    OpenEditor,
    Menu(MenuEvent),
    /// Periodic status summary from the local API for the tray.
    Status { line: String, warn: bool },
    /// ~60 fps overlay pump: live cursor position + a one-shot crossing flash.
    Overlay { x: i32, y: i32, flash: u8 },
    /// Quick share prompt (URL or file).
    QuickShare(crate::ui::ActiveQuickShare),
    QuickShareDismiss,
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
        return ("Engine starting / waiting for permissions…".into(), true);
    };
    if !v["running"].as_bool().unwrap_or(false) {
        return ("Engine not running (possibly waiting for permissions)".into(), true);
    }
    let peers = v["peers"].as_object().cloned().unwrap_or_default();
    if peers.is_empty() {
        return ("Waiting for a peer…".into(), true);
    }
    let mut parts = Vec::new();
    let mut any_down = false;
    for (name, p) in peers {
        if p["connected"].as_bool().unwrap_or(false) {
            match p["rtt_ms"].as_f64() {
                Some(rtt) => parts.push(format!("{name}: connected ({rtt:.1} ms)")),
                None => parts.push(format!("{name}: connected")),
            }
        } else {
            any_down = true;
            parts.push(format!("{name}: offline"));
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
        if let Err(e) = crate::platform::wait_for_gui_permissions().and_then(|_| crate::engine::host::run(cfg)) {
            tracing::error!("kayiver engine exited: {e:#}");
            crate::ui::set_link_error(Some(format!("Engine could not start: {e:#}")));
            // Keep the GUI alive so the user can read the error / retry.
        }
    })?;
    // Finder/Spotlight launch without a subcommand should show the editor.
    // Login agents explicitly invoke `run` and stay in the menu bar.
    run_shell(std::env::args_os().len() == 1)
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

fn run_shell(open_window_now: bool) -> Result<()> {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // Stay a menu-bar app even while the editor is open.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let editor_proxy=event_loop.create_proxy();
    crate::ui::register_editor_notifier(move || editor_proxy.send_event(UserEvent::OpenEditor).is_ok());
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    // Feed the tray a status summary every few seconds (connection state,
    // latency, warnings) so problems are visible without opening the editor.
    let status_proxy = event_loop.create_proxy();
    std::thread::Builder::new().name("kayiver-tray-status".into()).spawn(move || loop {
        if crate::ui::take_editor_request() {let _=status_proxy.send_event(UserEvent::OpenEditor);}
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

    // Register Quick Share notifier to deliver events to the GUI event loop.
    let qs_proxy = event_loop.create_proxy();
    crate::ui::register_quickshare_notifier(move |qs| {
        let _ = qs_proxy.send_event(UserEvent::QuickShare(qs));
    });

    let bubble_proxy = event_loop.create_proxy();

    let mut editor: Option<(Window, WebView)> = None;
    let mut overlay: Option<(Window, WebView, (i32, i32))> = None;
    let mut quick_share: Option<(Window, WebView)> = None;
    #[cfg(not(feature = "sim"))]
    let mut passive_notice = None;
    #[cfg(not(feature = "sim"))]
    let mut rescue_tick = std::time::Instant::now();
    let mut open_pending = open_window_now;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;

        let ui_ready=!open_pending || std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127,0,0,1],crate::ui::UI_PORT)),std::time::Duration::from_millis(25)).is_ok();
        if open_pending && !ui_ready {
            *control_flow=ControlFlow::WaitUntil(std::time::Instant::now()+std::time::Duration::from_millis(100));
        }
        if open_pending && ui_ready {
            open_pending = false;
            match open_editor_window(target) {
                Ok(w) => {
                    focus_editor(target, &w.0);
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
            Event::Reopen { .. } | Event::UserEvent(UserEvent::OpenEditor) => present_editor(target, &mut editor),
            Event::UserEvent(UserEvent::Overlay { x, y, flash }) => {
                #[cfg(not(feature = "sim"))]
                {
                    crate::platform::passive_macos::Notice::update(&mut passive_notice);
                    if rescue_tick.elapsed() >= std::time::Duration::from_secs(1) {
                        rescue_tick = std::time::Instant::now();
                        if let (Some(notice), Some((window, _))) = (&passive_notice, &editor) {
                            notice.rescue_editor(window);
                        }
                    }
                }
                if let Some((_, wv, origin)) = &overlay {
                    let _ = wv.evaluate_script(&format!(
                        "window.tick&&tick({},{},{flash})",
                        x - origin.0,
                        y - origin.1
                    ));
                }
            }
            Event::UserEvent(UserEvent::QuickShare(qs)) => {
                if qs.status == "pending" {
                    if let Some((w, _)) = quick_share.take() {
                        w.set_visible(false);
                    }
                    match open_quick_share_bubble(target, &bubble_proxy, &qs) {
                        Ok(w) => quick_share = Some(w),
                        Err(e) => eprintln!("quick share bubble failed: {e:#}"),
                    }
                } else if let Some((_, wv)) = &quick_share {
                    let msg = qs.message.as_deref().unwrap_or("");
                    let _ = wv.evaluate_script(&format!(
                        "window.updateStatus&&updateStatus('{}', '{}')",
                        qs.status, msg
                    ));
                }
            }
            Event::UserEvent(UserEvent::QuickShareDismiss) => {
                if let Some((w, _)) = quick_share.take() {
                    w.set_visible(false);
                }
            }
            Event::UserEvent(UserEvent::Menu(m)) => {
                if m.id == ids.open {
                    present_editor(target, &mut editor);
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
                if let Some((window, _)) = editor.as_ref() { window.set_visible(false); }
                editor = None;
                target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
                target.set_dock_visibility(false);
            }
            _ => {}
        }
    });
}

/// Spotlight, launchers and the tray all reuse the same editor window.
fn present_editor(target: &EventLoopWindowTarget<UserEvent>, editor: &mut Option<(Window, WebView)>) {
    if let Some((window, _)) = editor.as_ref() {
        window.set_visible(true);
        window.set_minimized(false);
        focus_editor(target, window);
    } else {
        match open_editor_window(target) {
            Ok(window) => {
                focus_editor(target, &window.0);
                *editor = Some(window);
            }
            Err(e) => eprintln!("editor window failed: {e:#}"),
        }
    }
}

/// Focus the editor while remaining a menu-bar accessory.
fn focus_editor(target: &EventLoopWindowTarget<UserEvent>, window: &Window) {
    target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
    target.set_dock_visibility(false);
    target.show_application();
    // Unhiding alone does not activate an accessory app; launchers need the
    // window to come forward even when another application owns focus.
    #[allow(deprecated)]
    unsafe {
        let mtm = objc2_foundation::MainThreadMarker::new_unchecked();
        objc2_app_kit::NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }
    window.set_focus();
}

fn build_tray() -> Result<(TrayIcon, MenuIds, MenuItem)> {
    let menu = Menu::new();
    let status = MenuItem::new("Fetching status…", false, None);
    let open = MenuItem::new("Kayıver'ı Aç", true, None);
    let toggle_shared = MenuItem::new("Toggle Shared Monitor\t⌘⌥M", true, None);
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
        .with_min_inner_size(tao::dpi::LogicalSize::new(600.0, 400.0))
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

const QUICKSHARE_TEMPLATE: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<style>
* { box-sizing: border-box; margin: 0; padding: 0; user-select: none; }
html, body {
  width: 100%; height: 100%; overflow: hidden;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  background: transparent;
}
.card {
  position: relative;
  display: flex;
  align-items: center;
  gap: 12px;
  width: 100%;
  height: 100%;
  padding: 10px 14px;
  background: rgba(24, 24, 28, 0.96);
  backdrop-filter: blur(24px);
  -webkit-backdrop-filter: blur(24px);
  border: 1px solid rgba(255, 255, 255, 0.16);
  border-radius: 14px;
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.5);
  color: #fff;
}
.icon-box {
  width: 44px;
  height: 44px;
  border-radius: 50%;
  background: linear-gradient(135deg, rgba(37, 99, 235, 0.35), rgba(29, 78, 216, 0.20));
  border: 1.5px solid rgba(96, 165, 250, 0.45);
  box-shadow: 0 0 16px rgba(37, 99, 235, 0.35);
  display: flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}
.icon-box svg {
  width: 22px;
  height: 22px;
  display: block;
}
.icon-box.file {
  background: linear-gradient(135deg, rgba(16, 185, 129, 0.35), rgba(5, 150, 105, 0.20));
  border-color: rgba(52, 211, 153, 0.45);
  box-shadow: 0 0 16px rgba(16, 185, 129, 0.35);
}
.content {
  flex: 1;
  min-width: 0;
}
.title {
  font-size: 13px;
  font-weight: 600;
  color: #f4f4f5;
  margin-bottom: 2px;
}
.detail {
  font-size: 11px;
  color: #a1a1aa;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 160px;
}
.btn-action {
  background: #2563eb;
  color: #fff;
  border: none;
  outline: none;
  font-size: 12px;
  font-weight: 600;
  padding: 7px 13px;
  border-radius: 8px;
  cursor: pointer;
  transition: all 0.15s ease;
  white-space: nowrap;
}
.btn-action:hover {
  background: #1d4ed8;
}
.btn-action:active {
  transform: scale(0.96);
}
.btn-close {
  background: transparent;
  border: none;
  color: #71717a;
  font-size: 14px;
  cursor: pointer;
  padding: 4px;
  line-height: 1;
  border-radius: 4px;
}
.btn-close:hover {
  color: #fff;
}
.progress {
  position: absolute;
  bottom: 0;
  left: 0;
  height: 3px;
  width: 100%;
  background: linear-gradient(90deg, #3b82f6, #60a5fa);
  border-bottom-left-radius: 14px;
  border-bottom-right-radius: 14px;
}
</style>
</head>
<body>
<div class="card" id="card">
  <div class="icon-box ${ICON_CLASS}">${ICON}</div>
  <div class="content">
    <div class="title" id="title">${TITLE}</div>
    <div class="detail" id="detail">${DETAIL}</div>
  </div>
  <button class="btn-action" id="actBtn" data-is-file="${IS_FILE}" onclick="doAction()">${ACTION_TEXT}</button>
  <button class="btn-close" onclick="doClose()">✕</button>
  <div class="progress" id="prog"></div>
</div>
<script>
let totalMs = 8000;
let remaining = totalMs;
let lastTick = performance.now();
let paused = false;
let transferring = false;

const card = document.getElementById('card');
const prog = document.getElementById('prog');
const actBtn = document.getElementById('actBtn');
const titleEl = document.getElementById('title');
const detailEl = document.getElementById('detail');

card.addEventListener('mouseenter', () => paused = true);
card.addEventListener('mouseleave', () => {
  paused = false;
  lastTick = performance.now();
});

function tick() {
  const now = performance.now();
  if (!paused && !transferring) {
    remaining -= (now - lastTick);
    if (remaining <= 0) {
      doClose();
      return;
    }
    prog.style.width = ((remaining / totalMs) * 100) + '%';
  }
  lastTick = now;
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);

function doAction() {
  if (actBtn.getAttribute('data-is-file') === 'true') {
    transferring = true;
    actBtn.disabled = true;
    actBtn.innerText = 'Aktarılıyor…';
    actBtn.style.background = '#4b5563';
    prog.style.display = 'none';
  }
  window.ipc && window.ipc.postMessage('action');
}

function doClose() {
  window.ipc && window.ipc.postMessage('close');
}

window.updateStatus = function(status, msg) {
  if (status === 'done') {
    actBtn.innerText = '✓ İndirildi';
    actBtn.style.background = '#10b981';
    titleEl.innerText = 'Dosya Kaydedildi!';
    if (msg) detailEl.innerText = msg;
    setTimeout(() => doClose(), 2500);
  } else if (status === 'error') {
    actBtn.innerText = 'Hata';
    actBtn.style.background = '#ef4444';
    if (msg) detailEl.innerText = msg;
    setTimeout(() => doClose(), 3500);
  }
};
</script>
</body>
</html>
"#;

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_quickshare_html(qs: &crate::ui::ActiveQuickShare) -> String {
    let (icon, icon_class, title, detail, action_text, is_file) = match &qs.payload {
        kayiver_core::proto::QuickSharePayload::Url { url, title: _ } => {
            let domain = url
                .strip_prefix("https://")
                .or_else(|| url.strip_prefix("http://"))
                .unwrap_or(url);
            let svg = r##"<svg viewBox="0 0 24 24" fill="none" stroke="#60a5fa" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"></circle><line x1="2" y1="12" x2="22" y2="12"></line><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"></path></svg>"##;
            (svg, "url", "Web Bağlantısı", domain.to_string(), "Aç", false)
        }
        kayiver_core::proto::QuickSharePayload::File { name, size, .. } => {
            let detail = format!("{} · {}", name, crate::engine::quickshare::format_size(*size));
            let svg = r##"<svg viewBox="0 0 24 24" fill="none" stroke="#34d399" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"></path></svg>"##;
            (svg, "file", "Dosya Paylaşımı", detail, "Aktar", true)
        }
    };

    QUICKSHARE_TEMPLATE
        .replace("${ICON}", icon)
        .replace("${ICON_CLASS}", icon_class)
        .replace("${TITLE}", title)
        .replace("${DETAIL}", &html_escape(&detail))
        .replace("${ACTION_TEXT}", action_text)
        .replace("${IS_FILE}", if is_file { "true" } else { "false" })
}

fn open_quick_share_bubble(
    target: &tao::event_loop::EventLoopWindowTarget<UserEvent>,
    proxy: &tao::event_loop::EventLoopProxy<UserEvent>,
    qs: &crate::ui::ActiveQuickShare,
) -> Result<(Window, WebView)> {
    let mon = target.primary_monitor().or_else(|| target.available_monitors().next());
    let (w, h) = (350.0, 84.0);
    let pos = match &mon {
        Some(m) => {
            let size = m.size().to_logical::<f64>(m.scale_factor());
            let origin = m.position().to_logical::<f64>(m.scale_factor());
            tao::dpi::LogicalPosition::new(
                origin.x + size.width - w - 24.0,
                origin.y + size.height - h - 36.0,
            )
        }
        None => tao::dpi::LogicalPosition::new(800.0, 500.0),
    };

    let window = WindowBuilder::new()
        .with_title("Kayıver Quick Share")
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top(true)
        .with_inner_size(tao::dpi::LogicalSize::new(w, h))
        .with_position(pos)
        .with_resizable(false)
        .with_focused(false)
        .build(target)?;

    unsafe {
        use objc2::msg_send;
        let ns = window.ns_window() as *mut objc2::runtime::AnyObject;
        if !ns.is_null() {
            let _: () = msg_send![ns, setLevel: 2_147_483_631i64];
            let _: () = msg_send![ns, setCollectionBehavior: 1u64 << 0 | 1u64 << 4];
        }
    }

    let dismiss_proxy = proxy.clone();
    let peer_c = qs.peer.clone();
    let id = qs.id;
    let url_opt = match &qs.payload {
        kayiver_core::proto::QuickSharePayload::Url { url, .. } => Some(url.clone()),
        _ => None,
    };
    let is_file = matches!(qs.payload, kayiver_core::proto::QuickSharePayload::File { .. });

    let webview = wry::WebViewBuilder::new()
        .with_transparent(true)
        .with_html(&render_quickshare_html(qs))
        .with_ipc_handler(move |req| {
            let msg = req.body().as_str();
            match msg {
                "action" => {
                    if let Some(url) = &url_opt {
                        crate::platform::open_url(url);
                        let _ = dismiss_proxy.send_event(UserEvent::QuickShareDismiss);
                        let _ = crate::ui::send_cmd(crate::ui::UiCmd::QuickShareDismiss);
                    } else if is_file {
                        let _ = crate::ui::send_cmd(crate::ui::UiCmd::QuickShareAccept {
                            peer: peer_c.clone(),
                            id,
                        });
                    }
                }
                "close" => {
                    let _ = dismiss_proxy.send_event(UserEvent::QuickShareDismiss);
                    let _ = crate::ui::send_cmd(crate::ui::UiCmd::QuickShareDismiss);
                }
                _ => {}
            }
        })
        .build(&window)?;

    Ok((window, webview))
}
