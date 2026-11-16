//! Receive-only companion. It deliberately has no engine, clipboard, input
//! hooks, injection, display-rearrangement or autostart module.
#![allow(dead_code)]
use anyhow::{Context, Result};
use clap::Parser;
mod display;
#[path = "display/receiver_platform.rs"]
mod platform;

// The shared manager sends topology notifications to the KVM editor when
// embedded there. This companion has no KVM engine or HTTP server to notify.
mod ui {
    pub enum UiCmd { DisplayChanged }
    pub fn send_cmd(_: UiCmd) -> anyhow::Result<()> { Ok(()) }
    pub fn peer_remote_addr(_: &str) -> Option<String> { None }
}

#[derive(Parser)]
#[command(about="Use a Windows monitor as an extra Mac display (receive only)")]
struct Options {
    #[arg(long)] list: bool,
    #[arg(long)] peer: Option<String>,
    #[arg(long)] monitor_id: Option<String>,
}

fn main() -> Result<()> {
    platform::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "screen-native") {
        anyhow::ensure!(args.get(1).is_some_and(|s| matches!(s.as_str(),"sink"|"test-sink")),"receiver cannot capture or encode a display");
        return display::native(args[1..].to_vec());
    }
    if args.first().is_some_and(|s| s == "screen-worker") {
        anyhow::ensure!(args.get(1).is_some_and(|s| s == "receive"),"receiver cannot provide a desktop");
        return display::worker(args[1..].to_vec());
    }
    let options = Options::parse();
    anyhow::ensure!(cfg!(target_os="windows"),"this companion runs on Windows");
    if options.list {
        println!("{}",display::api_status());
        return Ok(());
    }
    let peer=options.peer.context("select --peer and --monitor-id, or use --list")?;
    let monitor=options.monitor_id.context("select --monitor-id using --list")?;
    let cfg=kayiver_core::config::Config::load_or_init()?;
    let guard_port=cfg.port.checked_add(display::PORT_OFFSET+1).context("receiver guard port overflows")?;
    // Bind only to loopback, with no accepted connections or control API.
    // A second explicit launch must not compete for the same physical panel.
    let _instance=std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,guard_port))
        .context("an extra-display receiver is already open on this Windows session")?;
    // Explicit launch accepts this paired Mac/physical monitor for this process
    // lifetime only. The saved configuration is never changed.
    display::action(serde_json::to_string(&serde_json::json!({"action":"receive","peer":peer,"monitor_id":monitor}))?.as_bytes())?;
    let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        let mut previous=String::new();
        loop {
            let status=display::status();
            let next=serde_json::to_string(&status)?;
            if next!=previous {println!("{next}");previous=next;}
            if matches!(status.phase.as_str(),"off"|"error") {
                anyhow::ensure!(status.error.is_none(),"{}",status.error.unwrap_or_default());
                break;
            }
            tokio::select! {
                signal=tokio::signal::ctrl_c()=>{signal?;display::stop();},
                _=tokio::time::sleep(std::time::Duration::from_millis(250))=>{}
            }
        }
        Ok(())
    })
}
