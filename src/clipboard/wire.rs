// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use super::state::{MAX_TEXT_BYTES, Node, Revision, Update};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

// Independent clipboard protocol v1; the input/IPC formats stay unchanged.
const MAGIC: &[u8; 4] = b"LBC1";

pub(super) async fn write_update(
    stream: &mut (impl AsyncWrite + Unpin),
    update: &Update,
) -> io::Result<()> {
    if update.text.len() > MAX_TEXT_BYTES {
        return Err(invalid("clipboard text exceeds 1 MiB"));
    }
    stream.write_all(MAGIC).await?;
    stream.write_u64(update.revision.counter).await?;
    stream.write_all(&update.revision.origin).await?;
    stream.write_u32(update.text.len() as u32).await?;
    stream.write_all(update.text.as_bytes()).await?;
    stream.flush().await
}

pub(super) async fn read_update(
    stream: &mut (impl AsyncRead + Unpin),
    peer: Node,
) -> io::Result<Update> {
    let mut magic = [0; 4];
    stream.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Err(invalid("unsupported clipboard protocol"));
    }
    let counter = stream.read_u64().await?;
    let mut origin = [0; 32];
    stream.read_exact(&mut origin).await?;
    if origin != peer || counter == 0 || counter == u64::MAX {
        return Err(invalid("invalid clipboard revision or sender identity"));
    }
    let len = stream.read_u32().await? as usize;
    if len > MAX_TEXT_BYTES {
        return Err(invalid("clipboard text exceeds 1 MiB"));
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("invalid UTF-8 clipboard text"))?;
    Ok(Update {
        revision: Revision { counter, origin },
        text,
    })
}

pub(super) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unicode_multiline_and_empty_text_roundtrip() {
        for text in ["中文\nEnglish\r\n😀", ""] {
            let update = Update {
                revision: Revision {
                    counter: 1,
                    origin: [7; 32],
                },
                text: text.into(),
            };
            let mut bytes = Vec::new();
            write_update(&mut bytes, &update).await.unwrap();
            assert_eq!(
                read_update(&mut bytes.as_slice(), [7; 32]).await.unwrap(),
                update
            );
        }
    }

    #[tokio::test]
    async fn rejects_spoofed_sender_and_truncated_payload() {
        let update = Update {
            revision: Revision {
                counter: 1,
                origin: [7; 32],
            },
            text: "test".into(),
        };
        let mut bytes = Vec::new();
        write_update(&mut bytes, &update).await.unwrap();
        assert!(read_update(&mut bytes.as_slice(), [8; 32]).await.is_err());
        bytes.pop();
        assert!(read_update(&mut bytes.as_slice(), [7; 32]).await.is_err());
    }

    #[tokio::test]
    async fn rejects_oversize_before_allocating_payload() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1u64.to_be_bytes());
        bytes.extend_from_slice(&[7; 32]);
        bytes.extend_from_slice(&((MAX_TEXT_BYTES + 1) as u32).to_be_bytes());
        assert_eq!(
            read_update(&mut bytes.as_slice(), [7; 32])
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn rejects_unknown_version_invalid_utf8_and_invalid_counter() {
        let update = Update {
            revision: Revision {
                counter: 1,
                origin: [7; 32],
            },
            text: "a".into(),
        };
        let mut bytes = Vec::new();
        write_update(&mut bytes, &update).await.unwrap();
        let mut malformed = bytes.clone();
        malformed[3] = b'2';
        assert!(
            read_update(&mut malformed.as_slice(), [7; 32])
                .await
                .is_err()
        );
        let mut malformed = bytes.clone();
        *malformed.last_mut().unwrap() = 0xff;
        assert!(
            read_update(&mut malformed.as_slice(), [7; 32])
                .await
                .is_err()
        );
        for counter in [0, u64::MAX] {
            bytes[4..12].copy_from_slice(&counter.to_be_bytes());
            assert!(read_update(&mut bytes.as_slice(), [7; 32]).await.is_err());
        }
    }
}
