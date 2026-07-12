//! Encrypted session transport: Noise `NNpsk0` over TCP.
//!
//! Both sides already share a 32-byte PSK established during pairing
//! (see [`crate::pairing`]), so `NNpsk0` gives mutual authentication and
//! forward-secret encryption in a single round trip — the cheapest handshake
//! Noise offers, which matters because clients reconnect after every
//! sleep/wake or network blip.
//!
//! After the handshake the `TransportState` is converted to a
//! `StatelessTransportState` so the read and write halves of the socket can
//! be driven from independent tasks, each keeping its own nonce counter.

use std::sync::Arc;

use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

use crate::proto::Msg;
use crate::wire::{read_frame, write_frame};
use crate::{Error, Result};

const PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";

fn builder(psk: &[u8; 32]) -> Result<snow::Builder<'_>> {
    let params = PATTERN.parse().map_err(|_| Error::Protocol("bad noise params".into()))?;
    Ok(snow::Builder::new(params).psk(0, psk)?)
}

pub struct SecureReader {
    half: OwnedReadHalf,
    state: Arc<snow::StatelessTransportState>,
    nonce: u64,
}

pub struct SecureWriter {
    half: OwnedWriteHalf,
    state: Arc<snow::StatelessTransportState>,
    nonce: u64,
}

impl SecureReader {
    pub async fn recv(&mut self) -> Result<Msg> {
        let frame = read_frame(&mut self.half).await?;
        let mut plain = vec![0u8; frame.len()];
        let n = self.state.read_message(self.nonce, &frame, &mut plain)?;
        self.nonce += 1;
        Msg::decode(&plain[..n])
    }
}

impl SecureWriter {
    pub async fn send(&mut self, msg: &Msg) -> Result<()> {
        let plain = msg.encode()?;
        let mut cipher = vec![0u8; plain.len() + 16];
        let n = self.state.write_message(self.nonce, &plain, &mut cipher)?;
        self.nonce += 1;
        write_frame(&mut self.half, &cipher[..n]).await?;
        Ok(())
    }
}

async fn finish(stream: TcpStream, hs: snow::HandshakeState) -> Result<(SecureReader, SecureWriter)> {
    let state = Arc::new(hs.into_stateless_transport_mode()?);
    let (r, w) = stream.into_split();
    Ok((
        SecureReader { half: r, state: state.clone(), nonce: 0 },
        SecureWriter { half: w, state, nonce: 0 },
    ))
}

/// Client side: initiates the handshake.
pub async fn handshake_initiator(mut stream: TcpStream, psk: &[u8; 32]) -> Result<(SecureReader, SecureWriter)> {
    let mut hs = builder(psk)?.build_initiator()?;
    let mut buf = vec![0u8; 128];

    let n = hs.write_message(&[], &mut buf)?; // -> e (+psk mix)
    write_frame(&mut stream, &buf[..n]).await?;

    let frame = read_frame(&mut stream).await?; // <- e, ee
    let mut payload = vec![0u8; frame.len()];
    hs.read_message(&frame, &mut payload)?;

    finish(stream, hs).await
}

/// Host side: responds to the handshake.
pub async fn handshake_responder(mut stream: TcpStream, psk: &[u8; 32]) -> Result<(SecureReader, SecureWriter)> {
    let mut hs = builder(psk)?.build_responder()?;
    let mut buf = vec![0u8; 128];

    let frame = read_frame(&mut stream).await?;
    let mut payload = vec![0u8; frame.len()];
    hs.read_message(&frame, &mut payload)?;

    let n = hs.write_message(&[], &mut buf)?;
    write_frame(&mut stream, &buf[..n]).await?;

    finish(stream, hs).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::InputEvent;

    #[tokio::test]
    async fn handshake_and_roundtrip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let psk = [7u8; 32];

        let server = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let (mut r, mut w) = handshake_responder(s, &psk).await.unwrap();
            let msg = r.recv().await.unwrap();
            w.send(&msg).await.unwrap(); // echo
        });

        let s = TcpStream::connect(addr).await.unwrap();
        let (mut r, mut w) = handshake_initiator(s, &psk).await.unwrap();
        let sent = Msg::Input(InputEvent::MouseMove { dx: 11, dy: -4 });
        w.send(&sent).await.unwrap();
        assert_eq!(r.recv().await.unwrap(), sent);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn wrong_psk_fails() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let _ = handshake_responder(s, &[1u8; 32]).await;
        });

        let s = TcpStream::connect(addr).await.unwrap();
        assert!(handshake_initiator(s, &[2u8; 32]).await.is_err());
        let _ = server.await;
    }
}
