//! Length-prefixed framing: `u16 (big endian) length` + payload.
//!
//! 64 KiB max frame fits the Noise message size limit; input events are a
//! few bytes so this never fragments in practice.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME: usize = u16::MAX as usize;

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    if data.len() > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "frame exceeds u16 length"));
    }
    let len = (data.len() as u16).to_be_bytes();
    w.write_all(&len).await?;
    w.write_all(data).await?;
    w.flush().await
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    read_frame_into(r, &mut buf).await?;
    Ok(buf)
}

/// Reuse bounded frame storage in long-lived encrypted sessions.
pub async fn read_frame_into<R: AsyncRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<()> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await?;
    buf.resize(u16::from_be_bytes(len) as usize, 0);
    r.read_exact(buf).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn oversized_frame_is_rejected_before_any_socket_write() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let result = write_frame(&mut writer, &vec![0; MAX_FRAME + 1]).await;
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }
}
