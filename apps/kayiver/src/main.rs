mod android;
mod autostart;
mod engine;
#[cfg(target_os = "macos")]
mod gui;
mod keymap;
mod platform;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use kayiver_core::config::{Config, Mode};

#[derive(Parser)]
#[command(name = "kayiver", version, about = "Share one keyboard & mouse across your machines, seamlessly.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum DisplayAction {
    /// List this machine's displays.
    List,
    /// Remove a display from this machine's desktop (mirror it away on macOS,
    /// detach on Windows) so the cursor can't wander onto a panel that's
    /// physically showing the other machine.
    Disable { index: u32 },
    /// Re-add a previously disabled display to the desktop.
    Enable { index: u32 },
}

#[derive(Subcommand)]
enum Command {
    /// Run kayiver with the configured role (default subcommand).
    Run {
        /// Headless: skip the menu-bar icon / native window (macOS only).
        #[arg(long)]
        no_gui: bool,
    },
    /// Pair a new device: run this on the machine WITH the keyboard/mouse.
    /// Displays a PIN and waits for the other machine to `kayiver join`.
    Pair,
    /// Join a host: run this on the new screen-only machine.
    Join {
        /// Host address, e.g. 192.168.1.20 or 192.168.1.20:24817
        address: String,
    },
    /// Open the visual layout editor (drag & drop your screens).
    Ui {
        /// Don't open the browser automatically.
        #[arg(long)]
        no_open: bool,
    },
    /// Check permissions, config and screen geometry.
    Doctor,
    /// Start kayiver automatically at login.
    Autostart {
        #[arg(value_parser = ["enable", "disable"])]
        action: String,
    },
    /// List monitors, or detach/re-attach one from this machine's desktop
    /// (the shared-monitor mechanism; see `kayiver monitor`).
    Display {
        #[command(subcommand)]
        action: Option<DisplayAction>,
    },
    /// Shared monitor: show who owns the panel, or hand it to a machine.
    /// `kayiver monitor <machine>` / `kayiver monitor toggle` — enables the
    /// display on that machine and detaches it on the other one.
    Monitor {
        /// Machine name or "toggle". Omit to print the current state.
        target: Option<String>,
    },
    /// LAN API for the mobile companion app: enable/disable/status.
    Remote {
        #[arg(value_parser = ["enable", "disable", "status"])]
        action: String,
    },
    /// Print the config file path.
    ConfigPath,
    /// Hidden: inject an absolute cursor move to verify injection reaches the
    /// visible desktop (used to diagnose service/scheduled-task launches).
    #[command(hide = true)]
    InjectTest,
    /// Hidden (Windows): relaunch `kayiver run` in the active console session on
    /// the visible desktop. Only works when invoked as SYSTEM.
    #[command(hide = true)]
    LaunchSession,
}

fn main() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());
    // Optional unbuffered log file (`KAYIVER_LOGFILE=/path`): useful for
    // troubleshooting a background/autostarted instance whose stderr is not
    // visible. Each event is flushed as it happens.
    match std::env::var("KAYIVER_LOGFILE").ok().and_then(|p| {
        std::fs::OpenOptions::new().create(true).append(true).open(&p).ok()
    }) {
        Some(file) => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .init();
        }
        None => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
        }
    }

    // Panics must reach the log file: a background/autostarted instance dies
    // silently otherwise (stderr goes nowhere) and all we ever see is "the
    // process is gone".
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default_panic(info);
    }));

    // Platform init before anything reads screen geometry (Windows DPI).
    platform::init();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run { no_gui: false }) {
        Command::Run { no_gui } => run(no_gui),
        Command::Pair => engine::pairing::pair_as_display(),
        Command::Join { address } => engine::pairing::join(&address),
        Command::Ui { no_open } => {
            #[cfg(target_os = "macos")]
            if !no_open {
                return gui::run_editor();
            }
            ui::run(!no_open)
        }
        Command::Doctor => doctor(),
        Command::Autostart { action } => autostart::apply(action == "enable"),
        Command::Display { action } => display_cmd(action),
        Command::Monitor { target } => monitor_cmd(target),
        Command::Remote { action } => remote_cmd(&action),
        Command::ConfigPath => {
            println!("{}", Config::path().display());
            Ok(())
        }
        Command::InjectTest => inject_test(),
        Command::LaunchSession => {
            #[cfg(target_os = "windows")]
            {
                platform::launch_in_active_session()?;
                println!("launched kayiver run in the active session");
                Ok(())
            }
            #[cfg(not(target_os = "windows"))]
            {
                anyhow::bail!("launch-session is Windows-only")
            }
        }
    }
}

fn run(no_gui: bool) -> Result<()> {
    let _ = no_gui;
    // One engine per machine. A watchdog task racing a live instance (or a
    // double launch from the task scheduler) would double-inject every event;
    // the loser exits instantly instead. Held for the process lifetime.
    let guard = std::net::TcpListener::bind(("127.0.0.1", ui::UI_PORT + 3));
    match guard {
        Ok(l) => { Box::leak(Box::new(l)); }
        Err(_) => {
            tracing::info!("another kayiver instance is already running — exiting");
            return Ok(());
        }
    }
    // Windows Search finds apps through the Start Menu only; keep our
    // shortcut there so "kayiver" is always typeable into Start.
    autostart::ensure_start_menu_shortcut();
    let cfg = Config::load_or_init()?;
    if cfg.peers.is_empty() {
        // A host can run before its first pairing (useful to verify
        // permissions and capture stability); a client has nothing to do.
        eprintln!("No paired devices yet.");
        eprintln!("  On the machine with the keyboard/mouse:  kayiver pair");
        eprintln!("  On the other machine:                    kayiver join <that-machine-ip>");
        if cfg.mode == Mode::Client {
            std::process::exit(2);
        }
        eprintln!("Running as host anyway — input stays local until a device pairs.");
    }
    match cfg.mode {
        Mode::Host => {
            // macOS GUI host: start the menu-bar/window shell immediately and
            // let the engine thread wait for permissions, so the app is
            // visible right away instead of blocking on the permission prompt.
            #[cfg(target_os = "macos")]
            if !no_gui {
                return gui::run_host(cfg);
            }
            platform::ensure_permissions()?;
            engine::host::run(cfg)
        }
        Mode::Client => {
            platform::ensure_permissions()?;
            engine::client::run(cfg)
        }
    }
}

fn display_cmd(action: Option<DisplayAction>) -> Result<()> {
    match action.unwrap_or(DisplayAction::List) {
        DisplayAction::List => {
            let displays = platform::displays();
            let mut report = String::new();
            if displays.is_empty() {
                report.push_str("No displays found.\n");
                #[cfg(target_os = "macos")]
                report.push_str("(Install the helper: brew install m1ddc)\n");
            } else {
                report.push_str("displays:\n");
                for (idx, name, _input) in displays {
                    report.push_str(&format!("  [{idx}] {name}\n"));
                }
            }
            print!("{report}");
            // Also to a file so a session-launched run is inspectable.
            let _ = std::fs::write(Config::path().with_file_name("displays.txt"), report);
            Ok(())
        }
        DisplayAction::Disable { index } => {
            log_display_action("disable", index, platform::set_display_enabled(index, None, false))?;
            println!("display {index} removed from this machine's desktop");
            Ok(())
        }
        DisplayAction::Enable { index } => {
            log_display_action("enable", index, platform::set_display_enabled(index, None, true))?;
            println!("display {index} re-added to this machine's desktop");
            Ok(())
        }
    }
}

fn monitor_cmd(target: Option<String>) -> Result<()> {
    match target {
        None => {
            let (_, body) = ui::local_api("GET", "/api/status", None)?;
            let v: serde_json::Value = serde_json::from_str(&body)?;
            let sh = &v["shared"];
            if !sh["configured"].as_bool().unwrap_or(false) {
                println!("shared monitor: not configured");
                println!("  set it up in the editor ({}) or add [shared_monitor] to config.toml", ui::url());
                return Ok(());
            }
            println!(
                "shared monitor: showing {}",
                sh["owner"].as_str().unwrap_or("?")
            );
            if let Some(p) = sh["peer"].as_str() {
                println!("  shared with  : {p}");
            }
            if let Some(e) = sh["error"].as_str() {
                println!("  last error   : {e}");
            }
            Ok(())
        }
        Some(t) => {
            let body = serde_json::json!({ "owner": t }).to_string();
            let (status, payload) = ui::local_api("POST", "/api/shared", Some(&body))?;
            anyhow::ensure!(status == 200, "kayiver said: {payload}");
            println!("shared monitor -> {t}");
            Ok(())
        }
    }
}

/// Mirror the outcome to a file next to the config, so a session-launched
/// (headless) invocation can be inspected remotely.
fn log_display_action(what: &str, index: u32, res: Result<()>) -> Result<()> {
    let msg = match &res {
        Ok(()) => format!("{what} {index}: ok\n"),
        Err(e) => format!("{what} {index}: ERROR {e:#}\n"),
    };
    let _ = std::fs::write(Config::path().with_file_name("display_action.txt"), msg);
    res
}

fn remote_cmd(action: &str) -> Result<()> {
    use kayiver_core::config::RemoteApi;
    let mut cfg = Config::load_or_init()?;
    match action {
        "enable" => {
            cfg.remote.enabled = true;
            if cfg.remote.token.as_deref().unwrap_or("").is_empty() {
                cfg.remote.token = Some(RemoteApi::generate_token());
            }
            cfg.save()?;
            println!("remote api: ENABLED (restart kayiver to apply)");
            println!("  port : {} (all interfaces)", ui::UI_PORT + 1);
            println!("  token: {}", cfg.remote.token.as_deref().unwrap_or(""));
            println!("  companion app settings: host = this machine's LAN IP, port + token above");
        }
        "disable" => {
            cfg.remote.enabled = false;
            cfg.save()?;
            println!("remote api: disabled (restart kayiver to apply)");
        }
        _ => {
            println!(
                "remote api: {}",
                if cfg.remote.enabled { "enabled" } else { "disabled" }
            );
            if let Some(t) = &cfg.remote.token {
                println!("  port : {}", ui::UI_PORT + 1);
                println!("  token: {t}");
            }
        }
    }
    Ok(())
}

fn inject_test() -> Result<()> {
    // platform::init() already ran in main (DPI + input-desktop attach).
    let before = platform::cursor_pos();
    let bounds = platform::desktop_bounds();
    // Target the primary monitor (first in the list) center, which is always
    // a real visible point, rather than the whole-desktop bbox (which can land
    // on an offset/secondary monitor).
    let primary = platform::monitors().into_iter().next().unwrap_or(bounds);
    let target = (primary.x + primary.w / 2, primary.y + primary.h / 2);
    let mut inj = platform::Injector::new()?;
    inj.mouse_to(target.0, target.1, 0, 0);
    std::thread::sleep(std::time::Duration::from_millis(200));
    let after = platform::cursor_pos();
    let moved = after != before;
    let report = format!(
        "inject-test: bounds={bounds:?} target={target:?} before={before:?} after={after:?}\ncursor moved: {moved}\n"
    );
    print!("{report}");
    // Also write next to the config so a detached (session-launched) run can be
    // inspected from outside.
    let out = Config::path().with_file_name("injtest.txt");
    let _ = std::fs::write(out, report);
    Ok(())
}

fn doctor() -> Result<()> {
    let cfg = Config::load_or_init()?;
    println!("kayiver doctor");
    println!("  config file : {}", Config::path().display());
    println!("  machine name: {}", cfg.name);
    println!("  mode        : {:?}", cfg.mode);
    println!("  port        : {}", cfg.port);
    let b = platform::desktop_bounds();
    println!("  desktop     : {}x{} at ({}, {})", b.w, b.h, b.x, b.y);
    // Local addresses: what a peer's `addr`/`addrs` should point at.
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        let mut ips: Vec<String> = ifaces
            .into_iter()
            .filter(|i| !i.is_loopback() && i.ip().is_ipv4())
            .map(|i| format!("{} ({})", i.ip(), i.name))
            .collect();
        ips.sort();
        ips.dedup();
        println!("  local ip    : {}", if ips.is_empty() { "none".into() } else { ips.join(", ") });
    }
    if cfg.peers.is_empty() {
        println!("  peers       : none (run `kayiver pair` / `kayiver join`)");
    }
    for p in &cfg.peers {
        println!(
            "  peer        : {} (addr: {})",
            p.name,
            p.addr.as_deref().unwrap_or("mDNS discovery")
        );
    }
    for e in cfg.layout.portals(&cfg.name) {
        let (to, _) = cfg.layout.target(&cfg.name, e).unwrap();
        println!("  portal      : {} edge -> {}", e, to);
    }
    platform::doctor_permissions();
    Ok(())
}
