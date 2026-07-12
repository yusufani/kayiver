mod autostart;
mod engine;
mod keymap;
mod platform;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use drift_core::config::{Config, Mode};

#[derive(Parser)]
#[command(name = "drift", version, about = "Share one keyboard & mouse across your machines, seamlessly.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run drift with the configured role (default subcommand).
    Run,
    /// Pair a new device: run this on the machine WITH the keyboard/mouse.
    /// Displays a PIN and waits for the other machine to `drift join`.
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
    /// Start drift automatically at login.
    Autostart {
        #[arg(value_parser = ["enable", "disable"])]
        action: String,
    },
    /// Print the config file path.
    ConfigPath,
}

fn main() -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());
    // Optional unbuffered log file (`DRIFT_LOGFILE=/path`): useful for
    // troubleshooting a background/autostarted instance whose stderr is not
    // visible. Each event is flushed as it happens.
    match std::env::var("DRIFT_LOGFILE").ok().and_then(|p| {
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

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run) {
        Command::Run => run(),
        Command::Pair => engine::pairing::pair_as_display(),
        Command::Join { address } => engine::pairing::join(&address),
        Command::Ui { no_open } => ui::run(!no_open),
        Command::Doctor => doctor(),
        Command::Autostart { action } => autostart::apply(action == "enable"),
        Command::ConfigPath => {
            println!("{}", Config::path().display());
            Ok(())
        }
    }
}

fn run() -> Result<()> {
    let cfg = Config::load_or_init()?;
    if cfg.peers.is_empty() {
        // A host can run before its first pairing (useful to verify
        // permissions and capture stability); a client has nothing to do.
        eprintln!("No paired devices yet.");
        eprintln!("  On the machine with the keyboard/mouse:  drift pair");
        eprintln!("  On the other machine:                    drift join <that-machine-ip>");
        if cfg.mode == Mode::Client {
            std::process::exit(2);
        }
        eprintln!("Running as host anyway — input stays local until a device pairs.");
    }
    platform::ensure_permissions()?;
    match cfg.mode {
        Mode::Host => engine::host::run(cfg),
        Mode::Client => engine::client::run(cfg),
    }
}

fn doctor() -> Result<()> {
    let cfg = Config::load_or_init()?;
    println!("drift doctor");
    println!("  config file : {}", Config::path().display());
    println!("  machine name: {}", cfg.name);
    println!("  mode        : {:?}", cfg.mode);
    println!("  port        : {}", cfg.port);
    let b = platform::desktop_bounds();
    println!("  desktop     : {}x{} at ({}, {})", b.w, b.h, b.x, b.y);
    if cfg.peers.is_empty() {
        println!("  peers       : none (run `drift pair` / `drift join`)");
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
