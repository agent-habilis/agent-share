//! Small async read helpers for the framed wire protocol, vendored from
//! agent-habilis/swarm's `src/file/wire.rs` — trimmed to what mount uses.
//! All integers are little-endian, matching the manifest and ticket codecs.

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt};

pub(crate) async fn read_u32<R: AsyncRead + Unpin>(reader: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf).await?;
    Ok(u32::from_le_bytes(buf))
}
