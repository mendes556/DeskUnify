// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use super::manifest::invalid;
use serde::{Serialize, de::DeserializeOwned};
use std::{io, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

pub(super) const CHUNK: usize = 1024 * 1024;
pub(super) const IDLE: Duration = Duration::from_secs(60);
const MAX_JSON: usize = 16 * 1024 * 1024;

pub(super) async fn packet_length<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<usize> {
    let mut bytes = [0; 4];
    read_exact(stream, &mut bytes).await?;
    Ok(u32::from_be_bytes(bytes) as usize)
}

pub(super) async fn write_packet<S: AsyncWrite + Unpin>(
    stream: &mut S,
    bytes: &[u8],
) -> io::Result<()> {
    write_all(stream, &(bytes.len() as u32).to_be_bytes()).await?;
    write_all(stream, bytes).await?;
    if bytes.is_empty() {
        timeout(IDLE, stream.flush())
            .await
            .map_err(|_| invalid("文件心跳刷新超时"))??;
    }
    Ok(())
}

pub(super) async fn read_exact<S: AsyncRead + Unpin>(
    stream: &mut S,
    bytes: &mut [u8],
) -> io::Result<()> {
    timeout(IDLE, stream.read_exact(bytes))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "文件连接 60 秒没有进展"))??;
    Ok(())
}

pub(super) async fn write_all<S: AsyncWrite + Unpin>(
    stream: &mut S,
    bytes: &[u8],
) -> io::Result<()> {
    timeout(IDLE, stream.write_all(bytes))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "文件连接 60 秒没有进展"))?
}

pub(super) async fn write_json<S: AsyncWrite + Unpin, T: Serialize>(
    stream: &mut S,
    value: &T,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_JSON {
        return Err(invalid("文件清单过大"));
    }
    write_all(stream, &(bytes.len() as u32).to_be_bytes()).await?;
    write_all(stream, &bytes).await?;
    timeout(IDLE, stream.flush())
        .await
        .map_err(|_| invalid("文件连接刷新超时"))?
}

pub(super) async fn read_json<S: AsyncRead + Unpin, T: DeserializeOwned>(
    stream: &mut S,
) -> io::Result<T> {
    let mut length = [0; 4];
    read_exact(stream, &mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_JSON {
        return Err(invalid("文件清单长度非法"));
    }
    let mut bytes = vec![0; length];
    read_exact(stream, &mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
