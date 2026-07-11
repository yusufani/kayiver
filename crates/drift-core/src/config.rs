//! On-disk configuration: `<config_dir>/drift/config.toml`
//! (macOS: `~/Library/Application Support/drift/`, Windows: `%APPDATA%\drift\`).

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
    /// this is the path that keeps drift working across VPNs or networks
    /// where multicast is filtered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    /// Last known physical displays of this peer (cached from its Hello so
    /// the layout editor can draw real monitor shapes while it's offline).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screens: Vec<crate::proto::Rect>,
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
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("drift")
            .join("config.toml")
    }

    /// Load the config, creating a default one on first run.
    pub fn load_or_init() -> Result<Config> {
        let path = Self::path();
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
        };
        let mut peer = Peer { name: "win".into(), psk: String::new(), addr: Some("10.0.0.5:24817".into()), screens: vec![] };
        peer.set_psk(&[9u8; 32]);
        cfg.peers.push(peer);

        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.name, "mac-studio");
        assert_eq!(back.peer("win").unwrap().psk_bytes().unwrap(), [9u8; 32]);
        assert_eq!(back.layout.target("win", Edge::Left).unwrap().0, "mac-studio");
    }
}
