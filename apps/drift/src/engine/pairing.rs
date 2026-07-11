//! `drift pair` / `drift join` — one-time device pairing.

use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};

use anyhow::{bail, Context, Result};
use drift_core::config::{Config, Mode, Peer};
use drift_core::layout::{Edge, Link};
use drift_core::pairing::{self, PairInfo, Role};
use drift_core::proto::Intro;
use drift_core::wire::read_frame;
use drift_core::DEFAULT_PORT;
use tokio::net::{TcpListener, TcpStream};

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
}

/// Best-effort local LAN IP, for display purposes only.
fn local_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    Some(sock.local_addr().ok()?.ip())
}

/// Run on the machine that keeps the physical keyboard/mouse.
pub fn pair_as_display() -> Result<()> {
    let mut cfg = Config::load_or_init()?;
    cfg.mode = Mode::Host;
    let code = pairing::generate_code();

    runtime()?.block_on(async {
        let listener = TcpListener::bind(("0.0.0.0", cfg.port)).await.with_context(|| {
            format!(
                "cannot listen on port {} — if `drift run` is active, stop it and retry",
                cfg.port
            )
        })?;

        println!();
        println!("  Pairing code:  {code}");
        println!();
        if let Some(ip) = local_ip() {
            println!("  On the other machine run:  drift join {ip}");
        } else {
            println!("  On the other machine run:  drift join <this-machine-ip>");
        }
        println!("  Waiting for it to connect (Ctrl-C to abort)...");

        loop {
            let (mut stream, addr) = listener.accept().await?;
            let intro = match read_frame(&mut stream).await.map_err(anyhow::Error::from).and_then(|f| Ok(Intro::decode(&f)?)) {
                Ok(i) => i,
                Err(_) => continue,
            };
            if intro != Intro::Pair {
                continue; // a session attempt from an already-paired device
            }
            let my_info = PairInfo { name: cfg.name.clone(), port: cfg.port };
            match pairing::exchange(&mut stream, &code, Role::Display, &my_info).await {
                Ok((psk, theirs)) => {
                    let mut peer = Peer { name: theirs.name.clone(), psk: String::new(), addr: None, screens: vec![] };
                    peer.set_psk(&psk);
                    cfg.upsert_peer(peer);
                    ensure_layout_link(&mut cfg, &theirs.name);
                    cfg.save()?;
                    println!("  Paired with '{}' ({}).", theirs.name, addr.ip());
                    println!("  Layout: check `{}` — default places '{}' to the right.", Config::path().display(), theirs.name);
                    println!("  Now start both sides with `drift run` (or `drift autostart enable`).");
                    return Ok(());
                }
                Err(e) => {
                    // One wrong PIN burns this code: SPAKE2 allows exactly one
                    // online guess, so a fresh `drift pair` is required.
                    bail!("pairing failed: {e}. Run `drift pair` again for a fresh code.");
                }
            }
        }
    })
}

/// Run on the new screen-only machine.
pub fn join(address: &str) -> Result<()> {
    let mut cfg = Config::load_or_init()?;
    cfg.mode = Mode::Client;

    let addr = parse_addr(address)?;
    print!("Pairing code shown on the other machine: ");
    std::io::stdout().flush()?;
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let code = code.trim().to_string();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        bail!("the code is 6 digits");
    }

    runtime()?.block_on(async {
        let mut stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("cannot reach {addr} — is `drift pair` running there?"))?;
        drift_core::wire::write_frame(&mut stream, &Intro::Pair.encode()?).await?;
        let my_info = PairInfo { name: cfg.name.clone(), port: cfg.port };
        let (psk, theirs) = pairing::exchange(&mut stream, &code, Role::Input, &my_info).await?;

        let mut peer = Peer {
            name: theirs.name.clone(),
            psk: String::new(),
            addr: Some(SocketAddr::new(addr.ip(), theirs.port).to_string()),
            screens: vec![],
        };
        peer.set_psk(&psk);
        cfg.upsert_peer(peer);
        cfg.save()?;
        println!("Paired with host '{}'.", theirs.name);
        println!("Start with `drift run` (or `drift autostart enable`).");
        Ok(())
    })
}

fn parse_addr(s: &str) -> Result<SocketAddr> {
    let with_port = if s.contains(':') { s.to_string() } else { format!("{s}:{DEFAULT_PORT}") };
    with_port
        .to_socket_addrs()?
        .next()
        .with_context(|| format!("cannot resolve {s}"))
}

fn ensure_layout_link(cfg: &mut Config, peer: &str) {
    let involved = cfg
        .layout
        .links
        .iter()
        .any(|l| l.from == peer || l.to == peer);
    if !involved {
        cfg.layout.links.push(Link {
            from: cfg.name.clone(),
            edge: Edge::Right,
            to: peer.to_string(),
        });
    }
}
