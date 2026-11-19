//! Frames exchanged with the RDP server's channel proxy. `docs/protocol.md`
//! describes the format.

use tokio::io::{AsyncRead, AsyncReadExt as _};

pub const FRAME_OPEN: u32 = 1;
pub const FRAME_OPENED: u32 = 2;
pub const FRAME_DATA: u32 = 3;
pub const FRAME_CLOSE: u32 = 4;
pub const FRAME_CLOSED: u32 = 5;

const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct Frame {
    pub kind: u32,
    pub channel: u32,
    pub payload: Vec<u8>,
}

pub fn encode(kind: u32, channel: u32, payload: &[u8]) -> Vec<u8> {
    let length = u32::try_from(payload.len()).expect("payload is within MAX_PAYLOAD");
    let mut frame = Vec::with_capacity(12 + payload.len());
    frame.extend_from_slice(&kind.to_ne_bytes());
    frame.extend_from_slice(&channel.to_ne_bytes());
    frame.extend_from_slice(&length.to_ne_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Returns `None` at end of stream.
pub async fn read(reader: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Option<Frame>> {
    let mut header = [0u8; 12];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let field = |i: usize| u32::from_ne_bytes(header[i..i + 4].try_into().expect("four bytes"));
    let length = field(8) as usize;
    if length > MAX_PAYLOAD {
        return Err(std::io::Error::other("proxy frame too large"));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(Frame {
        kind: field(0),
        channel: field(4),
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip() {
        let bytes = [encode(FRAME_DATA, 7, b"abc"), encode(FRAME_CLOSED, 8, b"")].concat();
        let mut reader = bytes.as_slice();

        let first = read(&mut reader).await.unwrap().unwrap();
        assert_eq!((first.kind, first.channel), (FRAME_DATA, 7));
        assert_eq!(first.payload, b"abc");
        let second = read(&mut reader).await.unwrap().unwrap();
        assert_eq!((second.kind, second.channel), (FRAME_CLOSED, 8));
        assert!(second.payload.is_empty());
        assert!(read(&mut reader).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn oversized_frames_are_rejected() {
        let mut bytes = encode(FRAME_DATA, 1, b"");
        bytes[8..12].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(read(&mut bytes.as_slice()).await.is_err());
    }
}
