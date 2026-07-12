//! macOS app shell: menu-bar (tray) icon + native editor window.
//!
//! The tao event loop must own the main thread (NSApplication), so the
//! engine (host router + capture + embedded editor server) moves to a
//! background thread. The editor window is a WKWebView (wry) pointed at the
//! embedded server — no external browser involved.

#![cfg(target_os = "macos")]

use anyhow::Result;
use kayiver_core::config::Config;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
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
    std::thread::Builder::new().name("kayiver-engine".into()).spawn(move || {
        if let Err(e) = crate::engine::host::run(cfg) {
            eprintln!("kayiver engine exited: {e:#}");
            std::process::exit(1);
        }
    })?;
    run_shell(false)
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
    // Menu-bar app: no Dock icon, no app switcher entry.
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    let (_tray, ids) = build_tray()?;

    let mut editor: Option<(Window, WebView)> = None;
    let mut open_pending = open_window_now;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;

        if open_pending {
            open_pending = false;
            match open_editor_window(target) {
                Ok(w) => editor = Some(w),
                Err(e) => eprintln!("editor window failed: {e:#}"),
            }
        }

        match event {
            Event::UserEvent(UserEvent::Menu(m)) => {
                if m.id == ids.open {
                    if editor.is_none() {
                        match open_editor_window(target) {
                            Ok(w) => editor = Some(w),
                            Err(e) => eprintln!("editor window failed: {e:#}"),
                        }
                    } else if let Some((w, _)) = &editor {
                        w.set_focus();
                    }
                } else if m.id == ids.toggle_shared {
                    // The running host owns the logic; go through the local API.
                    let _ = crate::ui::local_api("POST", "/api/shared", Some(r#"{"owner":"toggle"}"#));
                } else if m.id == ids.quit {
                    std::process::exit(0);
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                editor = None; // drop window + webview; tray keeps the app alive
            }
            _ => {}
        }
    });
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
