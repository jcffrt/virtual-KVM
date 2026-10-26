//! Control stream framing: `u32` little-endian length followed by a
//! `postcard`-encoded [`Control`](crate::Control).

use crate::Control;

/// Upper bound for one control frame. Report descriptors are at most 4 KiB
/// each and a `DeviceList` carries a handful of them.
pub const MAX_FRAME: usize = 1 << 20;

#[derive(thiserror::Error, Debug)]
pub enum WireError {
    #[error("serialization failed: {0}")]
    Codec(#[from] postcard::Error),
    #[error("frame too large: {0} bytes")]
    TooLarge(usize),
    #[cfg(feature = "tokio")]
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Serialize a message without the length prefix.
pub fn encode(msg: &Control) -> Result<Vec<u8>, WireError> {
    Ok(postcard::to_allocvec(msg)?)
}

/// Deserialize a message without the length prefix.
pub fn decode(bytes: &[u8]) -> Result<Control, WireError> {
    Ok(postcard::from_bytes(bytes)?)
}

/// Serialize a message with its length prefix.
pub fn encode_frame(msg: &Control) -> Result<Vec<u8>, WireError> {
    let body = encode(msg)?;
    if body.len() > MAX_FRAME {
        return Err(WireError::TooLarge(body.len()));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(feature = "tokio")]
mod async_io {
    use super::*;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    /// Write one framed message.
    pub async fn write_frame<W: AsyncWrite + Unpin>(
        w: &mut W,
        msg: &Control,
    ) -> Result<(), WireError> {
        let frame = encode_frame(msg)?;
        w.write_all(&frame).await?;
        Ok(())
    }

    /// Read one framed message. Returns `Ok(None)` on a clean end of stream
    /// (no partial frame pending).
    pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Control>, WireError> {
        let mut len = [0u8; 4];
        match r.read_exact(&mut len).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_le_bytes(len) as usize;
        if len > MAX_FRAME {
            return Err(WireError::TooLarge(len));
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body).await?;
        Ok(Some(decode(&body)?))
    }
}

#[cfg(feature = "tokio")]
pub use async_io::{read_frame, write_frame};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_has_length_prefix() {
        let f = encode_frame(&Control::Ping(1)).unwrap();
        let len = u32::from_le_bytes([f[0], f[1], f[2], f[3]]) as usize;
        assert_eq!(len, f.len() - 4);
        assert_eq!(decode(&f[4..]).unwrap(), Control::Ping(1));
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn async_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &Control::Pong(9)).await.unwrap();
        write_frame(&mut a, &Control::Deactivate { keep: vec![] })
            .await
            .unwrap();
        drop(a);
        assert_eq!(read_frame(&mut b).await.unwrap(), Some(Control::Pong(9)));
        assert_eq!(
            read_frame(&mut b).await.unwrap(),
            Some(Control::Deactivate { keep: vec![] })
        );
        assert_eq!(read_frame(&mut b).await.unwrap(), None);
    }
}
