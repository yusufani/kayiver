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
    /// Worst RTT sample over the trailing 10s — makes latency spikes (e.g.
    /// Wi-Fi radio wake-ups) visible even between status polls.
    pub rtt_max_ms: Option<f64>,
    rtt_max_at: Option<std::time::Instant>,
    /// This side's socket address for the live session ("ip:port").
    pub local_addr: Option<String>,
    /// The peer's socket address for the live session.
    pub remote_addr: Option<String>,
    /// Human label for the interface the session rides ("Wi-Fi (en0)",
    /// "USB 10/100/1000 LAN (en8) · kablo").
    pub link_label: Option<String>,
}

/// Commands the editor (or `kayiver monitor`) sends to the running host router.
pub enum UiCmd {
    SetSharedOwner(String),
    /// Enter (true) or leave (false) tablet control: forward the keyboard/mouse
    /// to the connected Android device instead of the local desktop.
    TabletControl(bool),
    /// Tell `peer` to reconnect to us at `addr` (path picked in the editor).
    UseAddr { peer: String, addr: String },
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
    /// Client: last connection failure details (None while connected).
    pub link_error: Option<String>,
    /// Channel into the router; present while a host is running.
    pub cmd: Option<tokio::sync::mpsc::UnboundedSender<UiCmd>>,
    /// Client: the host's editor view (StateSync payload). The client's
    /// editor serves this so both machines show the same map.
    pub synced_state: Option<String>,
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
        e.rtt_max_ms = None;
        e.rtt_max_at = None;
    }
}

pub fn set_rtt(peer: &str, rtt_ms: f64) {
    let mut s = live().lock().unwrap();
    let p = s.peers.entry(peer.to_string()).or_default();
    p.rtt_ms = Some(rtt_ms);
    let now = std::time::Instant::now();
    let stale = p.rtt_max_at.map_or(true, |t| now.duration_since(t) > std::time::Duration::from_secs(10));
    if stale || p.rtt_max_ms.map_or(true, |m| rtt_ms > m) {
        p.rtt_max_ms = Some(rtt_ms);
        p.rtt_max_at = Some(now);
    }
}

/// Record which socket pair a peer's live session rides, so the editor can
/// show the actual path (Wi-Fi vs cable) instead of leaving the user to guess
/// why it lags.
pub fn set_link(peer: &str, local: Option<std::net::SocketAddr>, remote: Option<std::net::SocketAddr>) {
    let label = local.map(|a| iface_label(a.ip()));
    let mut s = live().lock().unwrap();
    let p = s.peers.entry(peer.to_string()).or_default();
    p.local_addr = local.map(|a| a.to_string());
    p.remote_addr = remote.map(|a| a.to_string());
    p.link_label = label;
}

/// Human label for the local interface owning `ip`: "Wi-Fi (en0)",
/// "USB 10/100/1000 LAN (en8) · kablo". Falls back to the bare device name
/// (or nothing) where the platform can't say.
fn iface_label(ip: std::net::IpAddr) -> String {
    let dev = iface_for_ip(ip);
    #[cfg(target_os = "macos")]
    {
        if let Some(dev) = &dev {
            let ports = std::process::Command::new("networksetup")
                .arg("-listallhardwareports")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let mut port_name = None;
            let mut current = None;
            for line in ports.lines() {
                if let Some(p) = line.strip_prefix("Hardware Port: ") {
                    current = Some(p.trim().to_string());
                } else if let Some(d) = line.strip_prefix("Device: ") {
                    if d.trim() == dev {
                        port_name = current.clone();
                    }
                }
            }
            let mut label = match port_name {
                Some(p) => format!("{p} ({dev})"),
                None => dev.clone(),
            };
            if matches!(ip, std::net::IpAddr::V4(v) if v.is_link_local()) {
                label.push_str(" · direct cable");
            }
            return label;
        }
    }
    let mut label = dev.unwrap_or_default();
    if matches!(ip, std::net::IpAddr::V4(v) if v.is_link_local()) {
        if !label.is_empty() {
            label.push_str(" · ");
        }
        label.push_str("direct cable");
    }
    label
}

/// Device name (en0, en8, …) owning `ip`, via `ifconfig` on macOS.
fn iface_for_ip(ip: std::net::IpAddr) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ifconfig").arg("-a").output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let needle = format!("inet {ip} ");
        let mut dev = None;
        for line in text.lines() {
            if !line.starts_with(['\t', ' ']) {
                dev = line.split(':').next().map(|s| s.to_string());
            } else if line.trim_start().starts_with(&needle.trim_start().to_string()) {
                return dev;
            }
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ip;
        None
    }
}

/// All local IPv4 addresses a client could dial for this host, labeled by
/// interface — the editor's path picker. Excludes loopback.
pub fn host_candidate_addrs(port: u16) -> Vec<(String, String)> {
    #[cfg(target_os = "macos")]
    {
        let Some(out) = std::process::Command::new("ifconfig").arg("-a").output().ok() else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut dev: Option<String> = None;
        let mut found = Vec::new();
        for line in text.lines() {
            if !line.starts_with(['\t', ' ']) {
                dev = line.split(':').next().map(|s| s.to_string());
            } else if let Some(rest) = line.trim_start().strip_prefix("inet ") {
                if let Some(ips) = rest.split_whitespace().next() {
                    if let Ok(ip) = ips.parse::<std::net::Ipv4Addr>() {
                        if !ip.is_loopback() {
                            let _ = &dev;
                            found.push((
                                format!("{ip}:{port}"),
                                iface_label(std::net::IpAddr::V4(ip)),
                            ));
                        }
                    }
                }
            }
        }
        found
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = port;
        Vec::new()
    }
}

/// Client: store the host's pushed editor view.
pub fn set_synced_state(state: String) {
    live().lock().unwrap().synced_state = Some(state);
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

/// Client-side link diagnostics: why the last connect attempt failed
/// (which addresses were tried). None = connected / not applicable.
pub fn set_link_error(err: Option<String>) {
    live().lock().unwrap().link_error = err;
}

pub fn url() -> String {
    format!("http://127.0.0.1:{UI_PORT}")
}

/// A one-shot crossing signal for the on-screen overlay animation: the edge the
/// cursor just crossed (1=left 2=right 3=top 4=bottom), 0 = none. Set by the
/// host router on a crossing; drained by the macOS overlay each frame.
pub static CROSS_FLASH: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_cross_flash(edge: kayiver_core::layout::Edge) {
    use kayiver_core::layout::Edge;
    let v = match edge {
        Edge::Left => 1,
        Edge::Right => 2,
        Edge::Top => 3,
        Edge::Bottom => 4,
    };
    CROSS_FLASH.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Take and clear the pending crossing flash (0 = none).
pub fn take_cross_flash() -> u8 {
    CROSS_FLASH.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Talk to a running kayiver's local API over plain TCP (no HTTP client dep).
/// Used by the CLI (`kayiver monitor`) and the macOS menu-bar shell.
pub fn local_api(method: &str, path: &str, body: Option<&str>) -> anyhow::Result<(u16, String)> {
    use std::io::{Read, Write};
    let addr = format!("127.0.0.1:{UI_PORT}");
    let mut stream = std::net::TcpStream::connect_timeout(
        &addr.parse().unwrap(),
        std::time::Duration::from_secs(2),
    )
    .map_err(|_| anyhow::anyhow!("kayiver is not running (nothing listening on {addr})"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
    let body = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes())?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp)?;
    let status: u16 = resp.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let payload = resp.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    Ok((status, payload))
}

/// Serve the editor forever. Used both by `kayiver ui` and by a running
/// `kayiver run` (which embeds the editor so it is always one click away).
pub async fn serve_forever() -> Result<()> {
    // Localhost: the editor writes to the local config and is not reachable
    // from the network by default.
    let listener = TcpListener::bind(("127.0.0.1", UI_PORT))
        .await
        .with_context(|| format!("ui port {UI_PORT} busy"))?;

    // Opt-in LAN listener (mobile companion): same routes, one port up, and
    // every request must present the bearer token from the config.
    if let Ok(cfg) = Config::load_or_init() {
        if cfg.remote.enabled {
            match cfg.remote.token.clone() {
                Some(token) if !token.is_empty() => {
                    tokio::spawn(async move {
                        let lan = match TcpListener::bind(("0.0.0.0", UI_PORT + 1)).await {
                            Ok(l) => l,
                            Err(e) => return warn!("remote api port {} busy: {e}", UI_PORT + 1),
                        };
                        tracing::info!("remote api listening on 0.0.0.0:{}", UI_PORT + 1);
                        loop {
                            let Ok((stream, _)) = lan.accept().await else { return };
                            let token = token.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle(stream, Some(token)).await {
                                    warn!("remote api request: {e:#}");
                                }
                            });
                        }
                    });
                }
                _ => warn!("remote.enabled is set but remote.token is empty — run `kayiver remote enable`"),
            }
        }
    }

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            if let Err(e) = handle(stream, None).await {
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

async fn handle(mut stream: TcpStream, required_token: Option<String>) -> Result<()> {
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
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);

    // LAN listener: reject anything without the right bearer token.
    if let Some(token) = &required_token {
        let ok = headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .map(|(_, v)| v == &format!("Bearer {token}"))
            .unwrap_or(false);
        if !ok {
            let body = b"unauthorized";
            let resp = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(resp.as_bytes()).await?;
            stream.write_all(body).await?;
            stream.flush().await?;
            return Ok(());
        }
    }

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
        ("GET", "/api/cursor") => ("200 OK", "application/json", api_cursor().into_bytes()),
        ("POST", "/api/layout") => match api_save_layout(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("POST", "/api/shared") => match api_set_shared(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("POST", "/api/link") => match api_use_addr(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("POST", "/api/shared-config") => match api_shared_config(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("GET", "/api/settings") => match api_get_settings() {
            Ok(json) => ("200 OK", "application/json", json.into_bytes()),
            Err(e) => ("500 Internal Server Error", "text/plain", e.to_string().into_bytes()),
        },
        ("POST", "/api/settings") => match api_set_settings(body) {
            Ok(json) => ("200 OK", "application/json", json.into_bytes()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("GET", "/api/android") => ("200 OK", "application/json", api_android_list().into_bytes()),
        ("POST", "/api/android/connect") => match api_android_connect(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("POST", "/api/android/control") => {
            send_cmd(UiCmd::TabletControl(true));
            ("200 OK", "text/plain", b"ok".to_vec())
        }
        ("POST", "/api/android/disconnect") => {
            send_cmd(UiCmd::TabletControl(false));
            crate::android::disconnect();
            ("200 OK", "text/plain", b"ok".to_vec())
        }
        ("POST", "/api/android/wireless") => match api_android_wireless(body) {
            Ok(json) => ("200 OK", "application/json", json.into_bytes()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("POST", "/api/android/place") => match api_android_place(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        ("POST", "/api/android/add") => match api_android_add(body) {
            Ok(json) => ("200 OK", "application/json", json.into_bytes()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        _ => ("404 Not Found", "text/plain", b"not found".to_vec()),
    }
}

/// GET /api/android — tool availability, devices, and live control state.
fn api_android_list() -> String {
    let devices: Vec<serde_json::Value> = crate::android::list_devices()
        .into_iter()
        .map(|d| {
            serde_json::json!({
                "serial": d.serial,
                "model": d.model,
                "connection": d.connection,
            })
        })
        .collect();
    serde_json::json!({
        "tools_ready": crate::android::tools_ready(),
        "devices": devices,
        "connected": crate::android::connected_serial(),
    })
    .to_string()
}

fn send_cmd(cmd: UiCmd) {
    if let Some(tx) = live().lock().unwrap().cmd.as_ref() {
        let _ = tx.send(cmd);
    }
}

/// POST /api/android/connect {"serial": "..."} — open the control session (get
/// it ready). Taking control is a separate step: the hotkey or an edge cross,
/// so a click doesn't yank the cursor away.
fn api_android_connect(body: &[u8]) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let serial = v.get("serial").and_then(|s| s.as_str()).context("missing 'serial'")?;
    crate::android::connect(serial)?;
    Ok(())
}

/// POST /api/android/place {"edge": "left"|"right"|"top"|"bottom"|null} — where
/// the tablet sits relative to the desktop, so crossing that edge controls it.
/// null clears the placement.
fn api_android_place(body: &[u8]) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let edge = v.get("edge").and_then(|e| e.as_str()).map(|s| s.to_string());
    if let Some(e) = &edge {
        anyhow::ensure!(["left", "right", "top", "bottom"].contains(&e.as_str()), "bad edge");
    }
    let mut cfg = Config::load_or_init()?;
    let has = edge.is_some();
    cfg.tablet_edge = edge;
    cfg.save()?;
    // Pre-establish the control session so the first crossing is instant.
    if has {
        std::thread::spawn(|| { crate::android::ensure_connected(); });
    }
    Ok(())
}

/// POST /api/android/add {"ip": "1.2.3.4[:port]"} — connect a wireless device.
fn api_android_add(body: &[u8]) -> Result<String> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let ip = v.get("ip").and_then(|s| s.as_str()).context("missing 'ip'")?;
    let addr = crate::android::add_wireless(ip.trim())?;
    Ok(serde_json::json!({ "addr": addr }).to_string())
}

/// POST /api/android/wireless {"serial": "..."} — arm wireless adb, return addr.
fn api_android_wireless(body: &[u8]) -> Result<String> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let serial = v.get("serial").and_then(|s| s.as_str()).context("missing 'serial'")?;
    let addr = crate::android::enable_wireless(serial)?;
    Ok(serde_json::json!({ "addr": addr }).to_string())
}

/// GET /api/settings — current app settings for the editor's settings panel.
fn api_get_settings() -> Result<String> {
    let cfg = Config::load_or_init()?;
    Ok(serde_json::json!({
        "name": cfg.name,
        "port": cfg.port,
        "mode": format!("{:?}", cfg.mode).to_lowercase(),
        "hotkey": cfg.shared_monitor.hotkey,
        "remote_enabled": cfg.remote.enabled,
        "remote_token": cfg.remote.token,
        "remote_port": UI_PORT + 1,
        "autostart": crate::autostart::is_enabled(),
        "edge_dwell_ms": cfg.edge_dwell_ms,
        "mac_shortcuts": cfg.mac_shortcuts,
        "win_mod_cmd": cfg.win_modifiers.cmd,
        "win_mod_opt": cfg.win_modifiers.opt,
        "win_mod_ctrl": cfg.win_modifiers.ctrl,
        "config_path": Config::path().display().to_string(),
    })
    .to_string())
}

/// POST /api/settings — update a subset of settings. Any field may be omitted.
/// {"hotkey":bool, "remote_enabled":bool, "autostart":bool}
fn api_set_settings(body: &[u8]) -> Result<String> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let mut cfg = Config::load_or_init()?;

    if let Some(h) = v.get("hotkey").and_then(|x| x.as_bool()) {
        cfg.shared_monitor.hotkey = h;
    }
    if let Some(en) = v.get("remote_enabled").and_then(|x| x.as_bool()) {
        cfg.remote.enabled = en;
        if en && cfg.remote.token.as_deref().unwrap_or("").is_empty() {
            cfg.remote.token = Some(kayiver_core::config::RemoteApi::generate_token());
        }
    }
    if let Some(ms) = v.get("edge_dwell_ms").and_then(|x| x.as_u64()) {
        cfg.edge_dwell_ms = ms.min(10_000); // cap at 10 s
    }
    if let Some(b) = v.get("mac_shortcuts").and_then(|x| x.as_bool()) {
        cfg.mac_shortcuts = b;
    }
    let valid = |s: &str| matches!(s, "ctrl" | "alt" | "win");
    if let Some(m) = v.get("win_mod_cmd").and_then(|x| x.as_str()).filter(|s| valid(s)) {
        cfg.win_modifiers.cmd = m.to_string();
    }
    if let Some(m) = v.get("win_mod_opt").and_then(|x| x.as_str()).filter(|s| valid(s)) {
        cfg.win_modifiers.opt = m.to_string();
    }
    if let Some(m) = v.get("win_mod_ctrl").and_then(|x| x.as_str()).filter(|s| valid(s)) {
        cfg.win_modifiers.ctrl = m.to_string();
    }
    cfg.save()?;

    // Autostart is applied immediately (writes a LaunchAgent / registry value).
    if let Some(on) = v.get("autostart").and_then(|x| x.as_bool()) {
        crate::autostart::apply(on)?;
    }
    api_get_settings()
}

/// POST /api/shared {"owner": "<machine>" | "toggle"} — hand the shared panel
/// to a machine. Forwarded to the running host router.
/// POST /api/link — {"peer":"name","addr":"ip:port"}: ask `peer` to reconnect
/// to this host at `addr` (the user picked a path in the editor).
fn api_use_addr(body: &[u8]) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let peer = v.get("peer").and_then(|x| x.as_str()).context("missing 'peer'")?.to_string();
    let addr = v.get("addr").and_then(|x| x.as_str()).context("missing 'addr'")?.to_string();
    addr.parse::<std::net::SocketAddr>().context("addr must be ip:port")?;
    let s = live().lock().unwrap();
    let tx = s.cmd.as_ref().context("host not running")?;
    tx.send(UiCmd::UseAddr { peer, addr }).ok().context("host router gone")?;
    Ok(())
}

fn api_set_shared(body: &[u8]) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let owner = v.get("owner").and_then(|o| o.as_str()).context("missing 'owner'")?;
    let s = live().lock().unwrap();
    let tx = s.cmd.as_ref().context("host not running")?;
    tx.send(UiCmd::SetSharedOwner(owner.to_string())).ok().context("host router gone")?;
    Ok(())
}

/// POST /api/shared-config — persist which panel is shared.
/// Body: {"local_monitor":N, "peer":"name", "peer_monitor":M, "hotkey":bool}
/// where the monitor fields are 0-based editor picks (order of the machine's
/// monitor list); they are converted to each platform's display indexing
/// (macOS display lists are 1-based, Windows attached order is 0-based).
/// Body {"clear":true} removes the configuration.
fn api_shared_config(body: &[u8]) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(body).context("invalid JSON")?;
    let mut cfg = Config::load_or_init()?;
    if v.get("clear").and_then(|x| x.as_bool()).unwrap_or(false) {
        cfg.shared_monitor.local_index = None;
        cfg.shared_monitor.local_rect = None;
        cfg.shared_monitor.peer_index = None;
        cfg.shared_monitor.peer_rect = None;
        cfg.shared_monitor.peer = None;
        cfg.save()?;
        return Ok(());
    }
    let local_pick = v.get("local_monitor").and_then(|x| x.as_u64()).context("missing local_monitor")? as u32;
    let peer_pick = v.get("peer_monitor").and_then(|x| x.as_u64()).context("missing peer_monitor")? as u32;
    let peer_name = v.get("peer").and_then(|x| x.as_str()).context("missing peer")?.to_string();
    // Capture both monitors' geometry now — it becomes the safety check that
    // prevents ever detaching the wrong monitor later.
    let local_rect = crate::platform::monitors().get(local_pick as usize).copied();
    let peer = cfg
        .peers
        .iter()
        .find(|x| x.name == peer_name)
        .with_context(|| format!("unknown peer '{peer_name}'"))?;
    let peer_rect = peer.screens.get(peer_pick as usize).copied();

    let to_platform_index = |os: &str, pick: u32| if os == "macos" { pick + 1 } else { pick };
    cfg.shared_monitor.local_index = Some(to_platform_index(std::env::consts::OS, local_pick));
    cfg.shared_monitor.local_rect = local_rect;
    cfg.shared_monitor.peer_index =
        Some(to_platform_index(peer.os.as_deref().unwrap_or("windows"), peer_pick));
    cfg.shared_monitor.peer_rect = peer_rect;
    cfg.shared_monitor.peer = Some(peer_name);
    if let Some(h) = v.get("hotkey").and_then(|x| x.as_bool()) {
        cfg.shared_monitor.hotkey = h;
    }
    cfg.save()?;
    Ok(())
}

/// The host's editor view as JSON — served over HTTP and pushed to clients
/// via `Msg::StateSync` so their editors mirror it.
pub fn state_json() -> Result<String> {
    api_state()
}

fn api_state() -> Result<String> {
    let cfg = Config::load_or_init()?;
    // Clients serve the HOST's synced view (same machines, real shapes,
    // links, shared panel) with only the "me" flags recomputed for this side.
    if cfg.mode == kayiver_core::config::Mode::Client {
        let synced = live().lock().unwrap().synced_state.clone();
        if let Some(raw) = synced {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(ms) = v.get_mut("machines").and_then(|m| m.as_array_mut()) {
                    for m in ms {
                        let me = m.get("name").and_then(|n| n.as_str()) == Some(cfg.name.as_str());
                        m["me"] = serde_json::Value::Bool(me);
                    }
                }
                // The shared-monitor block is written from the HOST's
                // perspective (local = host, peer = us). The editor reads
                // local_* as "me", so flip it for this side — otherwise the
                // panel resolves to two of OUR monitors and the map scrambles.
                let host_name = v["machines"]
                    .as_array()
                    .and_then(|ms| {
                        ms.iter()
                            .find(|m| m["name"].as_str() != Some(cfg.name.as_str()))
                            .and_then(|m| m["name"].as_str().map(String::from))
                    });
                let flip = v["shared_monitor"].as_object().is_some()
                    && v["shared_monitor"]["peer"].as_str() == Some(cfg.name.as_str());
                if let (true, Some(host)) = (flip, host_name) {
                    let sm = &mut v["shared_monitor"];
                    let host_local = sm["local_monitor"].clone();
                    let host_peer_mon = sm["peer_monitor"].clone();
                    sm["local_monitor"] = host_peer_mon;
                    sm["peer_monitor"] = host_local;
                    sm["peer"] = serde_json::Value::String(host);
                }
                return Ok(v.to_string());
            }
        }
    }
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
    // Editor-facing view of the shared-monitor config: raw platform indices
    // converted back to 0-based monitor picks.
    let from_platform_index = |os: &str, idx: u32| if os == "macos" { idx.saturating_sub(1) } else { idx };
    let sm = &cfg.shared_monitor;
    let shared = if sm.configured() {
        let peer_name = sm.peer.clone().or_else(|| cfg.peers.first().map(|p| p.name.clone()));
        let peer_os = peer_name
            .as_ref()
            .and_then(|n| cfg.peer(n))
            .and_then(|p| p.os.clone())
            .unwrap_or_else(|| "windows".into());
        serde_json::json!({
            "local_monitor": from_platform_index(std::env::consts::OS, sm.local_index.unwrap()),
            "peer": peer_name,
            "peer_monitor": from_platform_index(&peer_os, sm.peer_index.unwrap()),
            "hotkey": sm.hotkey,
        })
    } else {
        serde_json::Value::Null
    };
    Ok(serde_json::to_string(&serde_json::json!({
        "machines": machines,
        "links": cfg.layout.links,
        "shared_monitor": shared,
    }))?)
}

fn api_status() -> String {
    let s = live().lock().unwrap();
    let peers: HashMap<&String, serde_json::Value> = s
        .peers
        .iter()
        .map(|(name, p)| {
            (name, serde_json::json!({
                "connected": p.connected,
                "rtt_ms": p.rtt_ms,
                "rtt_max_ms": p.rtt_max_ms,
                "local_addr": p.local_addr,
                "remote_addr": p.remote_addr,
                "link_label": p.link_label,
            }))
        })
        .collect();
    let port = kayiver_core::config::Config::load_or_init().map(|c| c.port).unwrap_or(24817);
    let host_addrs: Vec<serde_json::Value> = host_candidate_addrs(port)
        .into_iter()
        .map(|(addr, label)| serde_json::json!({ "addr": addr, "label": label }))
        .collect();
    serde_json::json!({
        "running": s.running,
        "focus": s.focus,
        "peers": peers,
        "host_addrs": host_addrs,
        "link_error": s.link_error,
        "shared": {
            "configured": s.shared_configured,
            "peer": s.shared_peer,
            "owner": s.shared_owner,
            "error": s.shared_error,
        },
    })
    .to_string()
}

/// GET /api/cursor — the host's real cursor position (desktop coords) and which
/// machine currently has control, so the editor can show the live pointer.
fn api_cursor() -> String {
    let (x, y) = crate::platform::cursor_pos();
    let focus = live().lock().unwrap().focus.clone();
    serde_json::json!({ "x": x, "y": y, "focus": focus }).to_string()
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
        assert!(String::from_utf8_lossy(&body).contains("Kayıver"));
    }

    #[test]
    fn rejects_unknown_path() {
        let (status, _, _) = route("GET /etc/passwd HTTP/1.1", b"");
        assert_eq!(status, "404 Not Found");
    }
}
