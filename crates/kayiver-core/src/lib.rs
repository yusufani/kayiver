//! kayiver-core: platform-agnostic building blocks for the kayiver software KVM.
//!
//! This crate contains everything that does not touch OS input APIs:
//! - [`proto`]: wire messages and input event model (HID-usage based key codes)
//! - [`layout`]: the virtual screen arrangement and edge/portal math
//! - [`config`]: on-disk configuration
//! - [`pairing`]: SPAKE2 PIN-based pairing that derives a per-peer PSK
//! - [`secure`]: Noise (NNpsk0) encrypted session transport over TCP
//! - [`discovery`]: mDNS host advertisement and lookup
//! - [`wire`]: low-level length-prefixed framing

pub mod config;
pub mod discovery;
pub mod layout;
pub mod pairing;
pub mod proto;
pub mod secure;
pub mod wire;

pub use proto::PROTOCOL_VERSION;

/// Default TCP port for both sessions and pairing.
pub const DEFAULT_PORT: u16 = 24817;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode/decode: {0}")]
    Codec(#[from] postcard::Error),
    #[error("noise: {0}")]
    Noise(#[from] snow::Error),
    #[error("config: {0}")]
    Config(String),
    #[error("pairing failed: {0}")]
    Pairing(String),
    #[error("protocol: {0}")]
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, Error>;
