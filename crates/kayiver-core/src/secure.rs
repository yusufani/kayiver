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
use tokio::io::AsyncWriteExt;

use crate::proto::Msg;
use crate::wire::{read_frame, read_frame_into, write_frame, MAX_FRAME};
use crate::{Error, Result};

const PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";

fn builder(psk: &[u8; 32]) -> Result<snow::Builder<'_>> {
    let params = PATTERN.parse().map_err(|_| Error::Protocol("bad noise params".into()))?;
    Ok(snow::Builder::new(params).psk(0, psk)?)
}

pub struct SecureReader {
    cipher: Vec<u8>,
    plain: Vec<u8>,
    half: OwnedReadHalf,
    state: Arc<snow::StatelessTransportState>,
    nonce: u64,
}

pub struct SecureWriter {
    cipher: Vec<u8>,
    plain: Vec<u8>,
    half: OwnedWriteHalf,
    state: Arc<snow::StatelessTransportState>,
    nonce: u64,
}

impl SecureReader {
    pub async fn recv(&mut self) -> Result<Msg> {
        read_frame_into(&mut self.half, &mut self.cipher).await?;
        self.plain.resize(self.cipher.len(), 0);
        let n = self.state.read_message(self.nonce, &self.cipher, &mut self.plain)?;
        self.nonce += 1;
        Msg::decode(&self.plain[..n])
    }
}

impl SecureWriter {
    pub async fn send(&mut self, msg: &Msg) -> Result<()> {
        self.plain.clear();
        self.plain = postcard::to_extend(msg, std::mem::take(&mut self.plain))?;
        if self.plain.len() > MAX_FRAME - 16 {
            return Err(Error::Protocol("encrypted frame exceeds Noise/u16 limit".into()));
        }
        self.cipher.resize(self.plain.len() + 18, 0);
        let n = self.state.write_message(self.nonce, &self.plain, &mut self.cipher[2..])?;
        self.nonce += 1;
        self.cipher[..2].copy_from_slice(&(n as u16).to_be_bytes());
        // Same wire bytes, one socket write instead of separate length/data
        // writes. No event batching, coalescing or altered nonce ordering.
        self.half.write_all(&self.cipher[..n + 2]).await?;
        Ok(())
    }
}

async fn finish(stream: TcpStream, hs: snow::HandshakeState) -> Result<(SecureReader, SecureWriter)> {
    let state = Arc::new(hs.into_stateless_transport_mode()?);
    let (r, w) = stream.into_split();
    Ok((
        SecureReader { half: r, state: state.clone(), nonce: 0, cipher: Vec::with_capacity(256), plain: Vec::with_capacity(256) },
        SecureWriter { half: w, state, nonce: 0, cipher: Vec::with_capacity(256), plain: Vec::with_capacity(256) },
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
    async fn reused_buffers_remain_compatible_with_legacy_framing_and_nonce_order() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let psk = [11u8; 32];
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut r, mut w) = handshake_responder(stream, &psk).await.unwrap();
            // Deliberately retain the old allocating reader/two-write writer
            // to prove byte and cipher compatibility in both directions.
            for _ in 0..1000 {
                let frame = read_frame(&mut r.half).await.unwrap();
                let mut plain = vec![0; frame.len()];
                let n = r.state.read_message(r.nonce, &frame, &mut plain).unwrap();
                r.nonce += 1;
                let msg = Msg::decode(&plain[..n]).unwrap();
                let plain = msg.encode().unwrap();
                let mut cipher = vec![0; plain.len() + 16];
                let n = w.state.write_message(w.nonce, &plain, &mut cipher).unwrap();
                w.nonce += 1;
                write_frame(&mut w.half, &cipher[..n]).await.unwrap();
            }
        });
        let stream = TcpStream::connect(addr).await.unwrap();
        stream.set_nodelay(true).unwrap();
        let (mut r, mut w) = handshake_initiator(stream, &psk).await.unwrap();
        // Rejection must not consume a cipher nonce or corrupt the next frame.
        assert!(w.send(&Msg::Clipboard { text: "x".repeat(MAX_FRAME) }).await.is_err());
        assert_eq!(w.nonce, 0);
        let mut warm_capacities = None;
        for n in 0..1000 {
            let msg = if n % 10 == 0 {
                Msg::ClipboardImageChunk { id: 99, offset: u32::MAX, data: vec![n as u8; 32768] }
            } else { Msg::Input(InputEvent::MouseMove { dx: if n % 2 == 0 {100} else {-100}, dy: -4 }) };
            w.send(&msg).await.unwrap();
            assert_eq!(r.recv().await.unwrap(), msg);
            if n == 0 { warm_capacities = Some((w.plain.capacity(), w.cipher.capacity(), r.plain.capacity(), r.cipher.capacity())); }
            assert_eq!(warm_capacities.unwrap(), (w.plain.capacity(), w.cipher.capacity(), r.plain.capacity(), r.cipher.capacity()));
        }
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
