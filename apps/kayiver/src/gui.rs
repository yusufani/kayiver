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
    ActivationPolicy, EventLoopExtMacOS, EventLoopWindowTargetExtMacOS,
};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};
use wry::WebView;

/// Template icon (black + alpha), retina size; macOS tints it to the bar.
const MENUBAR_ICON: &[u8] = include_bytes!("../../../assets/icons/menubarTemplate@2x.png");

#[derive(Debug)]
enum UserEvent {
    Menu(MenuEvent),
}

struct MenuIds {
    open: tray_icon::menu::MenuId,
    toggle_shared: tray_icon::menu::MenuId,
    quit: tray_icon::menu::MenuId,
}

/// `kayiver run` (host mode): engine on a background thread, tray + window
/// shell on the main thread. Never returns.
pub fn run_host(cfg: Config) -> Result<()> {
    // Permissions are waited on here (not on the main thread) so the tray +
    // window appear immediately; the editor just shows "not running" until the
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

    let (_tray, ids) = build_tray()?;

    let mut editor: Option<(Window, WebView)> = None;
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
        }

        match event {
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

fn build_tray() -> Result<(TrayIcon, MenuIds)> {
    let menu = Menu::new();
    let open = MenuItem::new("Kayıver'ı Aç", true, None);
    let toggle_shared = MenuItem::new("Ortak Monitörü Değiştir\t⌘⌥M", true, None);
    let quit = MenuItem::new("Kayıver'dan Çık", true, None);
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
    Ok((tray, ids))
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
