//! `kayiver pair` / `kayiver join` — one-time device pairing.

use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};

use anyhow::{bail, Context, Result};
use kayiver_core::config::{Config, Mode, Peer};
use kayiver_core::layout::{Edge, Link};
use kayiver_core::pairing::{self, PairInfo, Role};
use kayiver_core::proto::Intro;
use kayiver_core::wire::read_frame;
use kayiver_core::DEFAULT_PORT;
use tokio::net::{TcpListener, TcpStream};

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread().enable_all().build()?)
}

/// Best-effort local LAN IP, for display purposes only.
pub fn local_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    Some(sock.local_addr().ok()?.ip())
}

/// Complete a SPAKE2 Display-role pairing handshake on an already-accepted
/// stream that just sent `Intro::Pair`, and persist the resulting peer & link.
pub async fn accept_pair_stream(
    stream: &mut TcpStream,
    addr: SocketAddr,
    code: &str,
) -> Result<String> {
    let mut cfg = Config::load_or_init()?;
    cfg.mode = Mode::Host;
    let my_info = PairInfo {
        name: cfg.name.clone(),
        port: cfg.port,
    };
    let (psk, theirs) = pairing::exchange(stream, code, Role::Display, &my_info)
        .await
        .map_err(|e| anyhow::anyhow!("pairing failed: {e}. Generate a fresh PIN and try again."))?;
    let mut peer = Peer {
        name: theirs.name.clone(),
        psk: String::new(),
        addr: Some(SocketAddr::new(addr.ip(), theirs.port).to_string()),
        addrs: vec![],
        last_good: None,
        screens: vec![],
        os: None,
    };
    peer.set_psk(&psk);
    cfg.upsert_peer(peer);
    ensure_layout_link(&mut cfg, &theirs.name);
    cfg.save()?;
    Ok(theirs.name)
}

/// Programmatic client-side pairing with an explicit 6-digit PIN (used by both
/// `kayiver join` CLI and the zero-terminal GUI pairing modal).
pub fn join_with_code(address: &str, code: &str) -> Result<String> {
    let code = code.trim();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        bail!("the code must be 6 digits");
    }
    let addr = parse_addr(address.trim())?;
    let mut cfg = Config::load_or_init()?;
    if cfg.peers.is_empty() {
        cfg.mode = Mode::Client;
    }

    runtime()?.block_on(async {
        let mut stream = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            TcpStream::connect(addr),
        )
        .await
        .with_context(|| format!("connect to {addr} timed out"))?
        .with_context(|| format!("cannot reach {addr} — is pairing active on that machine?"))?;
        kayiver_core::wire::write_frame(&mut stream, &Intro::Pair.encode()?).await?;
        let my_info = PairInfo {
            name: cfg.name.clone(),
            port: cfg.port,
        };
        let (psk, theirs) = pairing::exchange(&mut stream, code, Role::Input, &my_info).await?;

        let mut peer = Peer {
            name: theirs.name.clone(),
            psk: String::new(),
            addr: Some(SocketAddr::new(addr.ip(), theirs.port).to_string()),
            addrs: vec![],
            last_good: None,
            screens: vec![],
            os: None,
        };
        peer.set_psk(&psk);
        cfg.upsert_peer(peer);
        ensure_layout_link(&mut cfg, &theirs.name);
        cfg.save()?;
        Ok(theirs.name)
    })
}

/// Run on the machine that keeps the physical keyboard/mouse.
pub fn pair_as_display() -> Result<()> {
    let mut cfg = Config::load_or_init()?;
    cfg.mode = Mode::Host;
    let code = pairing::generate_code();

    runtime()?.block_on(async {
        let listener = TcpListener::bind(("0.0.0.0", cfg.port)).await.with_context(|| {
            format!(
                "cannot listen on port {} — if `kayiver run` is active, stop it and retry",
                cfg.port
            )
        })?;

        println!();
        println!("  Pairing code:  {code}");
        println!();
        if let Some(ip) = local_ip() {
            println!("  On the other machine run:  kayiver join {ip}");
        } else {
            println!("  On the other machine run:  kayiver join <this-machine-ip>");
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
            let theirs_name = accept_pair_stream(&mut stream, addr, &code).await?;
            println!("  Paired with '{}' ({}).", theirs_name, addr.ip());
            println!("  Layout: check `{}` — default places '{}' to the right.", Config::path().display(), theirs_name);
            println!("  Now start both sides with `kayiver run` (or `kayiver autostart enable`).");
            return Ok(());
        }
    })
}

/// Run on the new screen-only machine.
pub fn join(address: &str) -> Result<()> {
    print!("Pairing code shown on the other machine: ");
    std::io::stdout().flush()?;
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let theirs_name = join_with_code(address, &code)?;
    println!("Paired with host '{theirs_name}'.");
    println!("Start with `kayiver run` (or `kayiver autostart enable`).");
    Ok(())
}

fn parse_addr(s: &str) -> Result<SocketAddr> {
    let with_port = if s.contains(':') { s.to_string() } else { format!("{s}:{DEFAULT_PORT}") };
    with_port
        .to_socket_addrs()?
        .next()
        .with_context(|| format!("cannot resolve {s}"))
}

pub fn ensure_layout_link(cfg: &mut Config, peer: &str) {
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
