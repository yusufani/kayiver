//! `kayiver ui` — the layout editor.
//!
//! A deliberately tiny, dependency-free HTTP server bound to localhost only,
//! serving one embedded page (`index.html`). The page arranges machines by
//! drag & drop and POSTs the resulting edge links, which are written to
//! config.toml; a running host hot-reloads the layout within ~2 s.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use kayiver_core::config::Config;
use kayiver_core::layout::Link;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::warn;

const INDEX_HTML: &str = include_str!("index.html");
pub const UI_PORT: u16 = 24818;

/// Live status the running host publishes for the editor to render
/// (connection dots, latency, which machine currently has the cursor).
#[derive(Default, Clone)]
pub struct PeerLive {
    pub connected: bool,
    pub rtt_ms: Option<f64>,
}

/// Commands the editor (or `kayiver monitor`) sends to the running host router.
pub enum UiCmd {
    SetSharedOwner(String),
}

#[derive(Default)]
pub struct LiveState {
    pub running: bool,
    pub focus: Option<String>,
    pub peers: HashMap<String, PeerLive>,
    /// Shared-monitor live state (host only).
    pub shared_configured: bool,
    pub shared_peer: Option<String>,
    pub shared_owner: Option<String>,
    pub shared_error: Option<String>,
    /// Channel into the router; present while a host is running.
    pub cmd: Option<tokio::sync::mpsc::UnboundedSender<UiCmd>>,
}

static LIVE: OnceLock<Mutex<LiveState>> = OnceLock::new();

fn live() -> &'static Mutex<LiveState> {
    LIVE.get_or_init(|| Mutex::new(LiveState::default()))
}

/// Called by the host once, to mark that a live session router exists.
pub fn mark_running() {
    live().lock().unwrap().running = true;
}

pub fn set_connected(peer: &str, connected: bool) {
    let mut s = live().lock().unwrap();
    let e = s.peers.entry(peer.to_string()).or_default();
    e.connected = connected;
    if !connected {
        e.rtt_ms = None;
    }
}

pub fn set_rtt(peer: &str, rtt_ms: f64) {
    let mut s = live().lock().unwrap();
    s.peers.entry(peer.to_string()).or_default().rtt_ms = Some(rtt_ms);
}

pub fn set_focus(focus: Option<String>) {
    live().lock().unwrap().focus = focus;
}

pub fn set_cmd_sender(tx: tokio::sync::mpsc::UnboundedSender<UiCmd>) {
    live().lock().unwrap().cmd = Some(tx);
}

/// Update shared-monitor config-derived state. `owner: None` keeps the
/// current owner untouched (used on config hot-reload).
pub fn set_shared_state(configured: bool, peer: Option<String>, owner: Option<String>) {
    let mut s = live().lock().unwrap();
    s.shared_configured = configured;
    s.shared_peer = peer;
    if owner.is_some() {
        s.shared_owner = owner;
    }
}

pub fn set_shared_owner(owner: Option<String>) {
    live().lock().unwrap().shared_owner = owner;
}

pub fn set_shared_error(err: Option<String>) {
    live().lock().unwrap().shared_error = err;
}

pub fn url() -> String {
    format!("http://127.0.0.1:{UI_PORT}")
}

/// Serve the editor forever. Used both by `kayiver ui` and by a running
/// `kayiver run` (which embeds the editor so it is always one click away).
pub async fn serve_forever() -> Result<()> {
    // Localhost only: the editor writes to the local config and must not
    // be reachable from the network.
    let listener = TcpListener::bind(("127.0.0.1", UI_PORT))
        .await
        .with_context(|| format!("ui port {UI_PORT} busy"))?;
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle(stream).await {
                warn!("ui request: {e:#}");
            }
        });
    }
}

pub fn run(open_browser: bool) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let url = url();
        // If a running `kayiver run` already serves the editor, just open it.
        if TcpStream::connect(("127.0.0.1", UI_PORT)).await.is_ok() {
            println!("kayiver layout editor (served by the running kayiver): {url}");
            if open_browser {
                open_in_browser(&url);
            }
            return Ok(());
        }
        let server = tokio::spawn(serve_forever());
        println!("kayiver layout editor: {url}  (Ctrl-C to quit)");
        if open_browser {
            open_in_browser(&url);
        }
        server.await??;
        Ok(())
    })
}

/// Open the editor as a chromeless **app window** (not a browser tab) using
/// whichever Chromium browser is installed (`--app=URL`), so it feels like a
/// native panel with no address bar / tabs. Falls back to a normal browser
/// open if none is found. This keeps kayiver a single dependency-free binary
/// (no bundled webview runtime) while still presenting an app-like window.
fn open_in_browser(url: &str) {
    if try_app_window(url) {
        return;
    }
    // Fallback: ordinary browser.
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

#[cfg(target_os = "macos")]
fn try_app_window(url: &str) -> bool {
    // Prefer Chrome/Edge/Brave/Chromium app-mode; each runs chromeless.
    const BROWSERS: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    for b in BROWSERS {
        if std::path::Path::new(b).exists() {
            return std::process::Command::new(b)
                .arg(format!("--app={url}"))
                .arg("--window-size=980,680")
                .spawn()
                .is_ok();
        }
    }
    false
}

#[cfg(target_os = "windows")]
fn try_app_window(url: &str) -> bool {
    use std::path::Path;
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let candidates = [
        format!(r"{pf}\Google\Chrome\Application\chrome.exe"),
        format!(r"{pf86}\Google\Chrome\Application\chrome.exe"),
        format!(r"{pf86}\Microsoft\Edge\Application\msedge.exe"),
        format!(r"{pf}\Microsoft\Edge\Application\msedge.exe"),
        format!(r"{local}\Google\Chrome\Application\chrome.exe"),
    ];
    for c in candidates {
        if Path::new(&c).exists() {
            return std::process::Command::new(&c)
                .arg(format!("--app={url}"))
                .arg("--window-size=980,680")
                .spawn()
                .is_ok();
        }
    }
    false
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn try_app_window(_url: &str) -> bool {
    false
}

async fn handle(mut stream: TcpStream) -> Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];

    // Read until end of headers.
    let header_end = loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_headers_end(&buf) {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            anyhow::bail!("request too large");
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let content_length: usize = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);

    // Read the body.
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
        if body.len() > 256 * 1024 {
            anyhow::bail!("body too large");
        }
    }

    let (status, ctype, payload) = route(&request_line, &body);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn route(request_line: &str, body: &[u8]) -> (&'static str, &'static str, Vec<u8>) {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();

    match (method, path) {
        ("GET", "/") => ("200 OK", "text/html; charset=utf-8", INDEX_HTML.as_bytes().to_vec()),
        ("GET", "/api/state") => match api_state() {
            Ok(json) => ("200 OK", "application/json", json.into_bytes()),
            Err(e) => ("500 Internal Server Error", "text/plain", e.to_string().into_bytes()),
        },
        ("GET", "/api/status") => ("200 OK", "application/json", api_status().into_bytes()),
        ("POST", "/api/layout") => match api_save_layout(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("POST", "/api/shared") => match api_set_shared(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("POST", "/api/shared-config") => match api_shared_config(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        _ => ("404 Not Found", "text/plain", b"not found".to_vec()),
    }
}

/// POST /api/shared {"owner": "<machine>" | "toggle"} — hand the shared panel
/// to a machine. Forwarded to the running host router.
fn api_set_shared(body: &[u8]) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let owner = v.get("owner").and_then(|o| o.as_str()).context("missing 'owner'")?;
    let s = live().lock().unwrap();
    let tx = s.cmd.as_ref().context("host not running")?;
    tx.send(UiCmd::SetSharedOwner(owner.to_string())).ok().context("host router gone")?;
    Ok(())
}

/// POST /api/shared-config {"local_index":N,"peer":"name","peer_index":M,"hotkey":bool}
/// — persist which panel is shared. null/absent local_index clears the config.
fn api_shared_config(body: &[u8]) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let mut cfg = Config::load_or_init()?;
    cfg.shared_monitor.local_index = v.get("local_index").and_then(|x| x.as_u64()).map(|x| x as u32);
    cfg.shared_monitor.peer_index = v.get("peer_index").and_then(|x| x.as_u64()).map(|x| x as u32);
    cfg.shared_monitor.peer = v.get("peer").and_then(|x| x.as_str()).map(|s| s.to_string());
    if let Some(h) = v.get("hotkey").and_then(|x| x.as_bool()) {
        cfg.shared_monitor.hotkey = h;
    }
    if let Some(p) = &cfg.shared_monitor.peer {
        anyhow::ensure!(cfg.peers.iter().any(|x| &x.name == p), "unknown peer '{p}'");
    }
    cfg.save()?;
    Ok(())
}

fn api_state() -> Result<String> {
    let cfg = Config::load_or_init()?;
    let fallback = vec![kayiver_core::proto::Rect { x: 0, y: 0, w: 1920, h: 1080 }];
    let mut machines = vec![serde_json::json!({
        "name": cfg.name,
        "me": true,
        "monitors": crate::platform::monitors(),
    })];
    for p in &cfg.peers {
        machines.push(serde_json::json!({
            "name": p.name,
            "me": false,
            // Real shapes once the peer has connected at least once.
            "monitors": if p.screens.is_empty() { &fallback } else { &p.screens },
        }));
    }
    Ok(serde_json::to_string(&serde_json::json!({
        "machines": machines,
        "links": cfg.layout.links,
    }))?)
}

fn api_status() -> String {
    let s = live().lock().unwrap();
    let peers: HashMap<&String, serde_json::Value> = s
        .peers
        .iter()
        .map(|(name, p)| {
            (name, serde_json::json!({ "connected": p.connected, "rtt_ms": p.rtt_ms }))
        })
        .collect();
    serde_json::json!({
        "running": s.running,
        "focus": s.focus,
        "peers": peers,
        "shared": {
            "configured": s.shared_configured,
            "peer": s.shared_peer,
            "owner": s.shared_owner,
            "error": s.shared_error,
        },
    })
    .to_string()
}

fn api_save_layout(body: &[u8]) -> Result<()> {
    let links: Vec<Link> = serde_json::from_slice(body).context("invalid layout JSON")?;
    let mut cfg = Config::load_or_init()?;
    let known = |n: &str| n == cfg.name || cfg.peers.iter().any(|p| p.name == n);
    for l in &links {
        anyhow::ensure!(known(&l.from) && known(&l.to), "unknown machine in link {} -> {}", l.from, l.to);
        anyhow::ensure!(l.from != l.to, "self-link on {}", l.from);
    }
    cfg.layout.links = links;
    cfg.save()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_index() {
        let (status, ctype, body) = route("GET / HTTP/1.1", b"");
        assert_eq!(status, "200 OK");
        assert!(ctype.starts_with("text/html"));
        assert!(String::from_utf8_lossy(&body).contains("kayiver"));
    }

    #[test]
    fn rejects_unknown_path() {
        let (status, _, _) = route("GET /etc/passwd HTTP/1.1", b"");
        assert_eq!(status, "404 Not Found");
    }
}
