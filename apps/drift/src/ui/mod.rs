//! `drift ui` — the layout editor.
//!
//! A deliberately tiny, dependency-free HTTP server bound to localhost only,
//! serving one embedded page (`index.html`). The page arranges machines by
//! drag & drop and POSTs the resulting edge links, which are written to
//! config.toml; a running host hot-reloads the layout within ~2 s.

use anyhow::{Context, Result};
use drift_core::config::Config;
use drift_core::layout::Link;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::warn;

const INDEX_HTML: &str = include_str!("index.html");
pub const UI_PORT: u16 = 24818;

pub fn url() -> String {
    format!("http://127.0.0.1:{UI_PORT}")
}

/// Serve the editor forever. Used both by `drift ui` and by a running
/// `drift run` (which embeds the editor so it is always one click away).
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
        // If a running `drift run` already serves the editor, just open it.
        if TcpStream::connect(("127.0.0.1", UI_PORT)).await.is_ok() {
            println!("drift layout editor (served by the running drift): {url}");
            if open_browser {
                open_in_browser(&url);
            }
            return Ok(());
        }
        let server = tokio::spawn(serve_forever());
        println!("drift layout editor: {url}  (Ctrl-C to quit)");
        if open_browser {
            open_in_browser(&url);
        }
        server.await??;
        Ok(())
    })
}

fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
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
        ("POST", "/api/layout") => match api_save_layout(body) {
            Ok(()) => ("200 OK", "text/plain", b"ok".to_vec()),
            Err(e) => ("400 Bad Request", "text/plain", e.to_string().into_bytes()),
        },
        _ => ("404 Not Found", "text/plain", b"not found".to_vec()),
    }
}

fn api_state() -> Result<String> {
    let cfg = Config::load_or_init()?;
    let peers: Vec<&str> = cfg.peers.iter().map(|p| p.name.as_str()).collect();
    Ok(serde_json::to_string(&serde_json::json!({
        "name": cfg.name,
        "peers": peers,
        "links": cfg.layout.links,
    }))?)
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
        assert!(String::from_utf8_lossy(&body).contains("drift"));
    }

    #[test]
    fn rejects_unknown_path() {
        let (status, _, _) = route("GET /etc/passwd HTTP/1.1", b"");
        assert_eq!(status, "404 Not Found");
    }
}
