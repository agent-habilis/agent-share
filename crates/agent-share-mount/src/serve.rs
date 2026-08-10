//! Answering the mount protocol, in one copy for both platforms.
//!
//! This was two `serve_stream`s — one in the CLI, one in the browser — matching
//! the same ops over the same streams. They had already drifted in small ways
//! by the time they were merged, which is the argument for merging them.

use agent_share_proto::auth::ShareAuth;
use agent_share_proto::framing::{
    MAX_READ_LEN, OP_CHUNK, OP_CHUNK_MAP, OP_HAVE, OP_MANIFEST, OP_READ, OP_WATCH,
    REQUEST_HEADER_LEN, SECRET_LEN, encode_chunk_map, encode_have,
};
use agent_share_proto::manifest::ReadStatus;
use anyhow::{Context as _, Result};
use fofoca::iroh::endpoint::{Connection, RecvStream, SendStream};
use fofoca_chunks::{ChunkHash, ChunkMap, Coverage, Root};
use std::future::Future;

/// A live `OP_WATCH` subscription.
///
/// A trait rather than a channel type because this is the one place the two
/// platforms genuinely differ: the CLI broadcasts `Arc<Vec<u8>>` over
/// `tokio::sync::broadcast`, the browser sends `Rc<Vec<u8>>` down a
/// `futures::mpsc`. Naming a concrete channel here would have meant depending
/// on one platform's runtime, which is exactly what this crate exists to avoid.
pub trait Watcher {
    /// Kept as the source's own smart pointer so a frame is never copied to
    /// cross this seam. On a large tree a frame is the whole manifest, and
    /// normalising to `Vec<u8>` would clone several MB per watcher per change.
    type Frame: std::ops::Deref<Target = Vec<u8>>;

    /// The next frame, or `None` when the source will send no more.
    fn recv(&mut self) -> impl Future<Output = Option<Self::Frame>>;
}

/// What a peer answers requests from.
///
/// Three implementations, sharing nothing but this trait: the CLI's producer
/// reads through to files on disk, the browser's producer reads through to
/// `FileSystemFileHandle`s, and a seeder of either kind reads from a chunk
/// store. [`serve_stream`] drives all of them.
/// `Clone + 'static` because a protocol handler builds one per accepted stream
/// and hands it to a spawned task. Every implementation is a handle over shared
/// state — an `Arc` or an `Rc` — so cloning is a refcount bump, never a copy of
/// what the peer holds.
pub trait ServeSource: Clone + 'static {
    type Watcher: Watcher;

    /// The body for `OP_MANIFEST`: `version ‖ signature ‖ manifest`.
    ///
    /// **Verbatim.** For a seeder this is the origin's envelope, never a
    /// re-wrap: the fingerprint is defined over the manifest inside it and the
    /// signature over both, and no seeder holds a key to re-sign with.
    ///
    /// `None` refuses the request — a seeder that has not synced yet has
    /// nothing to vouch for — which closes the stream rather than inventing an
    /// answer.
    fn manifest_envelope(&self) -> Option<Vec<u8>>;

    /// Register a watcher: the opening frame, then the update stream.
    ///
    /// A seeder's stream only moves when its own snapshot does. It follows the
    /// origin while the origin lives and freezes when it dies; it never
    /// fabricates a delta of its own.
    fn subscribe(&self) -> Option<(Vec<u8>, Self::Watcher)>;

    /// Answer one `OP_READ`. A source unsure it holds the bytes answers
    /// `BadIndex`, never a short read — a caller cannot tell a short answer
    /// from EOF.
    fn answer_read(
        &self,
        index: u32,
        offset: u64,
        len: u32,
    ) -> impl Future<Output = (ReadStatus, Vec<u8>)>;

    /// Answer one `OP_CHUNK_MAP`: a file's ordered chunk addresses, computed on
    /// first ask and kept afterwards.
    fn answer_chunk_map(&self, index: u32) -> impl Future<Output = Option<ChunkMap>>;

    /// Answer one `OP_CHUNK`: the bytes at an address, or `None`.
    ///
    /// **Only for addresses reachable from a row this source holds for this
    /// share.** A chunk store is global — a chunk is the same chunk whichever
    /// share it arrived through — but `OP_CHUNK` names no share, so answering
    /// any address would let one link discover what else the host is storing.
    fn answer_chunk(&self, address: ChunkHash) -> impl Future<Output = Option<Vec<u8>>>;

    /// Answer one `OP_HAVE`: exactly which chunks of `root` can be served.
    /// Partial is a first-class answer.
    fn answer_have(&self, root: Root) -> impl Future<Output = Option<Coverage>>;

    /// Let the last bytes land, after `finish` and before the stream is
    /// dropped.
    ///
    /// The one place the two platforms are allowed to differ, and it is here
    /// because waiting needs a timer and each has its own. `finish` only
    /// *marks* a stream done, so on a fast link — loopback especially — the
    /// teardown can outrun the bytes; the CLI answers that by waiting a bounded
    /// two seconds on `stopped()`.
    ///
    /// Defaults to dropping the stream, which is what the browser has always
    /// done. Left as a default rather than forced on both because giving the
    /// browser the same wait means giving it a wasm timer, which is a change
    /// worth making deliberately rather than as a side effect of sharing this
    /// loop.
    fn settle(&self, send: SendStream) -> impl Future<Output = ()> {
        async move {
            drop(send);
        }
    }
}

/// Authenticate one stream by its request header and answer it.
///
/// A bad token closes the whole connection — the bearer is poisoned, so there
/// is nothing to salvage — while an unknown op or a malformed request drops
/// only this stream. The close code says which refusal it was, so a consumer
/// that got a password wrong can be told to try again instead of concluding the
/// share is broken.
///
/// # Errors
/// A write to the stream failed. A *read* failing is not an error: the peer
/// went away mid-request, and there is nothing left to answer.
///
/// # Panics
/// Never: the fixed-width slices taken out of each request body are sized by
/// the same `read_body::<N>` that filled them.
pub async fn serve_stream<S: ServeSource>(
    conn: &Connection,
    mut send: SendStream,
    mut recv: RecvStream,
    auth: &ShareAuth,
    source: S,
) -> Result<()> {
    let mut header = [0u8; REQUEST_HEADER_LEN];
    if recv.read_exact(&mut header).await.is_err() {
        return Ok(());
    }
    if !auth.accepts(&header) {
        conn.close(auth.refusal_code().into(), auth.refusal_reason());
        return Ok(());
    }
    match header[SECRET_LEN] {
        OP_MANIFEST => {
            let Some(envelope) = source.manifest_envelope() else {
                return Ok(());
            };
            write_ok_body(&mut send, &envelope).await?;
        }
        OP_WATCH => {
            // Long-lived, unlike every other op: it returns when the consumer
            // goes away, so it must not fall through to the `finish` below.
            let Some((opening, mut watcher)) = source.subscribe() else {
                return Ok(());
            };
            if write_ok_body(&mut send, &opening).await.is_err() {
                return Ok(());
            }
            while let Some(frame) = watcher.recv().await {
                if write_ok_body(&mut send, &frame).await.is_err() {
                    break;
                }
            }
            return Ok(());
        }
        OP_READ => {
            let Some(request) = read_body::<16>(&mut recv).await else {
                return Ok(());
            };
            let index = u32::from_le_bytes(request[..4].try_into().expect("4 bytes"));
            let offset = u64::from_le_bytes(request[4..12].try_into().expect("8 bytes"));
            let len = u32::from_le_bytes(request[12..].try_into().expect("4 bytes"));
            let (status, data) = if len > MAX_READ_LEN {
                // Checked here rather than in each source, so a new source
                // cannot forget it and quietly serve an unbounded read.
                (ReadStatus::LenOverCap, Vec::new())
            } else {
                source.answer_read(index, offset, len).await
            };
            write_body(&mut send, status, &data).await?;
        }
        OP_CHUNK_MAP => {
            let Some(request) = read_body::<4>(&mut recv).await else {
                return Ok(());
            };
            let Some(row) = source.answer_chunk_map(u32::from_le_bytes(request)).await else {
                return refuse(&mut send).await;
            };
            let addresses: Vec<[u8; 32]> =
                row.leaves().iter().map(|leaf| *leaf.as_bytes()).collect();
            let body = encode_chunk_map(row.root().as_bytes(), row.size(), &addresses);
            write_ok_body(&mut send, &body).await?;
        }
        OP_CHUNK => {
            let Some(request) = read_body::<32>(&mut recv).await else {
                return Ok(());
            };
            let Some(bytes) = source.answer_chunk(ChunkHash::from_bytes(request)).await else {
                return refuse(&mut send).await;
            };
            write_ok_body(&mut send, &bytes).await?;
        }
        OP_HAVE => {
            let Some(request) = read_body::<32>(&mut recv).await else {
                return Ok(());
            };
            let Some(coverage) = source.answer_have(Root::from_bytes(request)).await else {
                return refuse(&mut send).await;
            };
            let chunks = u32::try_from(coverage.len()).unwrap_or(u32::MAX);
            write_ok_body(&mut send, &encode_have(chunks, coverage.as_bits())).await?;
        }
        other => {
            // Unknown op: drop just this stream, keep the connection. A peer
            // speaking a newer protocol is not a peer to hang up on.
            let _ = other;
            return Ok(());
        }
    }
    let _ = send.finish();
    source.settle(send).await;
    Ok(())
}

/// A fixed-size request body, or `None` if the peer went away mid-request.
async fn read_body<const N: usize>(recv: &mut RecvStream) -> Option<[u8; N]> {
    let mut request = [0u8; N];
    recv.read_exact(&mut request).await.ok()?;
    Some(request)
}

/// `BadIndex` and nothing else.
///
/// The one answer for every "I cannot vouch for that": an index out of range, a
/// tombstone, a file that moved, an address from another share, a root never
/// heard of. They are the same thing from the far side — *this peer cannot
/// serve it* — and telling them apart is exactly the probe oracle that scoping
/// exists to close.
async fn refuse(send: &mut SendStream) -> Result<()> {
    send.write_all(&[ReadStatus::BadIndex.to_byte()])
        .await
        .context("writing a refusal")?;
    let _ = send.finish();
    Ok(())
}

async fn write_ok_body(send: &mut SendStream, body: &[u8]) -> Result<()> {
    write_body(send, ReadStatus::Ok, body).await
}

/// Every answer on this protocol: `status(1) ‖ len(u32 LE) ‖ body`.
async fn write_body(send: &mut SendStream, status: ReadStatus, body: &[u8]) -> Result<()> {
    send.write_all(&[status.to_byte()])
        .await
        .context("writing the status")?;
    let len = u32::try_from(body.len()).context("answer body over u32")?;
    send.write_all(&len.to_le_bytes())
        .await
        .context("writing the body length")?;
    send.write_all(body).await.context("writing the body")?;
    Ok(())
}
