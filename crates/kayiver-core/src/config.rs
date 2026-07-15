//! On-disk configuration: `<config_dir>/kayiver/config.toml`
//! (macOS: `~/Library/Application Support/kayiver/`, Windows: `%APPDATA%\kayiver\`).

use std::path::PathBuf;

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::layout::Layout;
use crate::{Error, Result, DEFAULT_PORT};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The machine whose physical keyboard/mouse are shared.
    Host,
    /// A machine that receives input.
    Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub name: String,
    /// base64 of the 32-byte pairing-derived PSK.
    pub psk: String,
    /// Optional static address ("ip:port"). Tried before mDNS discovery —
    /// this is the path that keeps kayiver working across VPNs or networks
    /// where multicast is filtered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    /// Fallback addresses tried after `addr` (e.g. the host's Wi-Fi IP when
    /// the direct cable is the primary). Learned automatically: whenever a
    /// session succeeds via mDNS or a fallback, that address is remembered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addrs: Vec<String>,
    /// Address of the most recent successful session; tried first on
    /// reconnect. Kept separate from `addr` so the user's configured primary
    /// is never overwritten.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_good: Option<String>,
    /// Last known physical displays of this peer (cached from its Hello so
    /// the layout editor can draw real monitor shapes while it's offline).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screens: Vec<crate::proto::Rect>,
    /// Peer OS ("macos"/"windows"...), cached from its Hello. Used to map
    /// editor monitor picks to that platform's display indexing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
}

impl Peer {
    pub fn psk_bytes(&self) -> Result<[u8; 32]> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&self.psk)
            .map_err(|e| Error::Config(format!("peer {}: bad psk base64: {e}", self.name)))?;
        raw.try_into()
            .map_err(|_| Error::Config(format!("peer {}: psk must be 32 bytes", self.name)))
    }

    pub fn set_psk(&mut self, psk: &[u8; 32]) {
        self.psk = base64::engine::general_purpose::STANDARD.encode(psk);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// This machine's name in the layout. Defaults to the hostname.
    pub name: String,
    pub mode: Mode,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub peers: Vec<Peer>,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default)]
    pub shared_monitor: SharedMonitor,
    #[serde(default)]
    pub remote: RemoteApi,
    /// How long the cursor must rest against a portal edge before it crosses to
    /// the next machine, in milliseconds. 0 (default) = cross instantly. A small
    /// dwell (e.g. 2000) prevents accidental crossings when you just brush the
    /// edge.
    #[serde(default)]
    pub edge_dwell_ms: u64,
    /// Which desktop edge leads to the Android tablet ("left"/"right"/"top"/
    /// "bottom"), set by dragging the tablet tile in the editor. When set and a
    /// device is available, crossing that edge hands control to the tablet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tablet_edge: Option<String>,
}

/// Opt-in LAN exposure of the status/control API (used by the mobile
/// companion app). Off by default; when enabled, a second listener binds on
/// all interfaces and every request must carry `Authorization: Bearer <token>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RemoteApi {
    #[serde(default)]
    pub enabled: bool,
    /// Shared secret; generate one with `kayiver remote enable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// One physical panel cabled to both machines ("shared monitor"). kayiver keeps
/// the OS desktops honest about which machine the panel is currently showing:
/// the visible machine has the display attached, the hidden one detaches it so
/// its cursor can't wander onto a screen nobody can see. The user flips
/// ownership with the hotkey, the UI, or `kayiver monitor <machine>` — matching
/// the physical input switch on the monitor itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedMonitor {
    /// This machine's index for the shared panel (macOS: 1-based, matching
    /// `kayiver display list`; Windows: 0-based attached-display order).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_index: Option<u32>,
    /// This machine's shared monitor geometry — verified before detaching so an
    /// index slip can never turn off the wrong monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_rect: Option<crate::proto::Rect>,
    /// Peer sharing the panel. Defaults to the first paired peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// The peer's own index for the same panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_index: Option<u32>,
    /// The peer's shared monitor geometry (same safety check on its side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_rect: Option<crate::proto::Rect>,
    /// Toggle ownership with Cmd+Alt+M (macOS) / Ctrl+Alt+M (Windows).
    #[serde(default = "default_true")]
    pub hotkey: bool,
}

impl Default for SharedMonitor {
    fn default() -> Self {
        SharedMonitor {
            local_index: None,
            local_rect: None,
            peer: None,
            peer_index: None,
            peer_rect: None,
            hotkey: true,
        }
    }
}

impl RemoteApi {
    /// Random 128-bit hex token for the LAN API.
    pub fn generate_token() -> String {
        use rand::RngCore;
        let mut b = [0u8; 16];
        rand::rng().fill_bytes(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}

impl SharedMonitor {
    /// Fully configured (both sides' indices known)?
    pub fn configured(&self) -> bool {
        self.local_index.is_some() && self.peer_index.is_some()
    }
}

fn default_true() -> bool {
    true
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

pub fn machine_name() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .trim_end_matches(".local")
        .to_lowercase()
        .replace(' ', "-")
}

impl Default for Config {
    fn default() -> Self {
        Config {
            name: machine_name(),
            mode: Mode::Client,
            port: DEFAULT_PORT,
            peers: Vec::new(),
            layout: Layout::default(),
            shared_monitor: SharedMonitor::default(),
            remote: RemoteApi::default(),
            edge_dwell_ms: 0,
            tablet_edge: None,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("kayiver")
            .join("config.toml")
    }

    /// Load the config, creating a default one on first run. A config left
    /// behind by the app's old name ("drift") is migrated automatically.
    pub fn load_or_init() -> Result<Config> {
        let path = Self::path();
        if !path.exists() {
            let legacy = dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("drift")
                .join("config.toml");
            if legacy.exists() {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::copy(&legacy, &path)?;
            }
        }
        if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))
        } else {
            let cfg = Config::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| Error::Config(e.to_string()))?;
        std::fs::write(&path, text)?;
        Ok(())
    }

    pub fn peer(&self, name: &str) -> Option<&Peer> {
        self.peers.iter().find(|p| p.name == name)
    }

    /// Insert or replace a peer entry.
    pub fn upsert_peer(&mut self, peer: Peer) {
        if let Some(existing) = self.peers.iter_mut().find(|p| p.name == peer.name) {
            *existing = peer;
        } else {
            self.peers.push(peer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Edge, Link};

    #[test]
    fn toml_roundtrip() {
        let mut cfg = Config {
            name: "mac-studio".into(),
            mode: Mode::Host,
            port: DEFAULT_PORT,
            peers: vec![],
            layout: Layout {
                links: vec![Link { from: "mac-studio".into(), edge: Edge::Right, to: "win".into() }],
            },
            shared_monitor: SharedMonitor::default(),
            remote: RemoteApi::default(),
            edge_dwell_ms: 0,
            tablet_edge: None,
        };
        let mut peer = Peer { name: "win".into(), psk: String::new(), addr: Some("10.0.0.5:24817".into()), addrs: vec![], last_good: None, screens: vec![], os: None };
        peer.set_psk(&[9u8; 32]);
        cfg.peers.push(peer);

        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.name, "mac-studio");
        assert_eq!(back.peer("win").unwrap().psk_bytes().unwrap(), [9u8; 32]);
        assert_eq!(back.layout.target("win", Edge::Left).unwrap().0, "mac-studio");
    }
}
