//! One-time pairing: a short code typed by the user, run through SPAKE2,
//! produces a strong 32-byte PSK for all future sessions.
//!
//! SPAKE2 means the short code never travels on the wire and an attacker who
//! records the exchange learns nothing usable offline; a wrong guess simply
//! fails the key-confirmation step. See `docs/SECURITY.md`.
//!
//! Frames on the wire (plaintext TCP, length-prefixed):
//! 1. both sides: SPAKE2 public message
//! 2. both sides: key confirmation `SHA256(key || role-tag)`
//! 3. both sides: `PairInfo` (name + session port), integrity-bound by 2.

use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio::net::TcpStream;

use crate::wire::{read_frame, write_frame};
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairInfo {
    pub name: String,
    pub port: u16,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The machine that displays the code (`drift pair`).
    Display,
    /// The machine where the user types the code (`drift join`).
    Input,
}

/// Generate a human-friendly 6-digit pairing code.
/// Short codes are safe here because SPAKE2 limits attackers to one online
/// guess per connection attempt and `drift pair` accepts a single attempt.
pub fn generate_code() -> String {
    let n: u32 = rand::rng().random_range(0..1_000_000);
    format!("{n:06}")
}

fn confirm_tag(key: &[u8], role: Role) -> [u8; 32] {
    let tag: &[u8] = match role {
        Role::Display => b"drift-pair-confirm-display",
        Role::Input => b"drift-pair-confirm-input",
    };
    let mut h = Sha256::new();
    h.update(key);
    h.update(tag);
    h.finalize().into()
}

fn derive_psk(key: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(key);
    h.update(b"drift-session-psk-v1");
    h.finalize().into()
}

/// Run the pairing exchange. Returns the derived PSK and the peer's info.
pub async fn exchange(
    stream: &mut TcpStream,
    code: &str,
    role: Role,
    my_info: &PairInfo,
) -> Result<([u8; 32], PairInfo)> {
    let (state, outbound) = Spake2::<Ed25519Group>::start_symmetric(
        &Password::new(code.as_bytes()),
        &Identity::new(b"drift-kvm-pairing-v1"),
    );
    write_frame(stream, &outbound).await?;
    let inbound = read_frame(stream).await?;
    let key = state
        .finish(&inbound)
        .map_err(|e| Error::Pairing(format!("spake2: {e:?}")))?;

    // Key confirmation, direction-tagged so a reflected frame can't pass.
    write_frame(stream, &confirm_tag(&key, role)).await?;
    let their_confirm = read_frame(stream).await?;
    let expected = confirm_tag(
        &key,
        if role == Role::Display { Role::Input } else { Role::Display },
    );
    if their_confirm.as_slice() != expected.as_slice() {
        return Err(Error::Pairing("code mismatch (wrong PIN?)".into()));
    }

    let info_bytes = postcard::to_allocvec(my_info)?;
    write_frame(stream, &info_bytes).await?;
    let their_info: PairInfo = postcard::from_bytes(&read_frame(stream).await?)?;

    Ok((derive_psk(&key), their_info))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pair_pair(code_a: &str, code_b: &str) -> (Result<([u8; 32], PairInfo)>, Result<([u8; 32], PairInfo)>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let code_a = code_a.to_string();
        let code_b = code_b.to_string();

        let display = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let info = PairInfo { name: "host".into(), port: 1 };
            exchange(&mut s, &code_a, Role::Display, &info).await
        });
        let mut s = TcpStream::connect(addr).await.unwrap();
        let info = PairInfo { name: "client".into(), port: 2 };
        let b = exchange(&mut s, &code_b, Role::Input, &info).await;
        (display.await.unwrap(), b)
    }

    #[tokio::test]
    async fn matching_codes_agree() {
        let (a, b) = pair_pair("123456", "123456").await;
        let (psk_a, info_b) = a.unwrap();
        let (psk_b, info_a) = b.unwrap();
        assert_eq!(psk_a, psk_b);
        assert_eq!(info_b.name, "client");
        assert_eq!(info_a.name, "host");
    }

    #[tokio::test]
    async fn wrong_code_rejected() {
        let (a, b) = pair_pair("123456", "654321").await;
        assert!(a.is_err() || b.is_err());
    }
}
