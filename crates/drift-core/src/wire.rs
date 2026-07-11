//! Length-prefixed framing: `u16 (big endian) length` + payload.
//!
//! 64 KiB max frame fits the Noise message size limit; input events are a
//! few bytes so this never fragments in practice.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME: usize = u16::MAX as usize;

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    debug_assert!(data.len() <= MAX_FRAME);
    let len = (data.len() as u16).to_be_bytes();
    w.write_all(&len).await?;
    w.write_all(data).await?;
    w.flush().await
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await?;
    let len = u16::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}
