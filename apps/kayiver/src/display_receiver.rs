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
    pub fn peer_remote_addr(peer: &str) -> Option<String> {
        use std::{io::{Read,Write},net::{TcpStream,SocketAddr},time::Duration};
        // Optional, bounded route hint from the existing local KVM. It is not
        // required for screen-only operation and never carries pairing keys.
        let address:SocketAddr="127.0.0.1:24818".parse().ok()?;
        let mut socket=TcpStream::connect_timeout(&address,Duration::from_millis(150)).ok()?;
        socket.set_read_timeout(Some(Duration::from_millis(200))).ok()?;
        socket.set_write_timeout(Some(Duration::from_millis(150))).ok()?;
        socket.write_all(b"GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:24818\r\nConnection: close\r\n\r\n").ok()?;
        let mut response=Vec::new();socket.take(65536).read_to_end(&mut response).ok()?;
        connected_address(&response,peer)
    }
    fn connected_address(response:&[u8],peer:&str)->Option<String> {
        if !response.starts_with(b"HTTP/1.1 200 "){return None;}
        let offset=response.windows(4).position(|v|v==b"\r\n\r\n")?+4;
        let status:serde_json::Value=serde_json::from_slice(&response[offset..]).ok()?;
        let entry=&status["peers"][peer];
        if entry["connected"].as_bool()!=Some(true){return None;}
        Some(entry["remote_addr"].as_str()?.parse::<std::net::SocketAddr>().ok()?.to_string())
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn only_a_live_matching_peer_can_supply_a_route_hint() {
            let response=b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"peers\":{\"mac\":{\"connected\":true,\"remote_addr\":\"10.99.0.1:24817\"}}}";
            assert_eq!(connected_address(response,"mac").as_deref(),Some("10.99.0.1:24817"));
            assert!(connected_address(response,"other-mac").is_none());
            let offline=String::from_utf8_lossy(response).replace("true","false");
            assert!(connected_address(offline.as_bytes(),"mac").is_none());
            let invalid=String::from_utf8_lossy(response).replace("10.99.0.1:24817","not-an-address");
            assert!(connected_address(invalid.as_bytes(),"mac").is_none());
            assert!(connected_address(b"HTTP/1.1 503 Unavailable\r\n\r\n{}","mac").is_none());
        }
    }
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
    let parent_exit=display::parent_lifetime()?;
    // Explicit launch accepts this paired Mac/physical monitor for this process
    // lifetime only. The saved configuration is never changed.
    display::action(serde_json::to_string(&serde_json::json!({"action":"receive","peer":peer,"monitor_id":monitor}))?.as_bytes())?;
    let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async {
        tokio::pin!(parent_exit);
        let mut stopping=false;
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
                _=&mut parent_exit, if !stopping=>{display::stop();stopping=true;},
                signal=tokio::signal::ctrl_c(), if !stopping=>{signal?;display::stop();stopping=true;},
                _=tokio::time::sleep(std::time::Duration::from_millis(250))=>{}
            }
        }
        Ok(())
    })
}
