//! The mount protocol's identity and byte layouts.
//!
//! One QUIC bi-stream per request. Every stream opens with
//! `token(32) ‖ op(1)`; the op decides what follows. The producer treats a
//! bad token as fatal to the whole connection and an unknown op as fatal to
//! just that stream.
//!
//! The 32 bytes are the share *token*, not the ticket secret — they are the
//! same thing on an unprotected share and differ on a passworded one. See
//! [`crate::auth::share_token`].

use anyhow::{Context, Result, bail};

use crate::manifest::ReadStatus;

/// ALPN for the mount protocol — request/response bi-streams.
///
/// Forked from agent-habilis/swarm's `agent-habilis-swarm/mount/1` when this
/// tool took the `agent-share` name. QUIC refuses a handshake on ALPN
/// mismatch, so `ahsw mount` and `agent-share` no longer connect to each
/// other — deliberate, not drift.
pub const MOUNT_ALPN: &[u8] = b"agent-share/mount/1";

/// ALPN for the `WebRTC` signaling exchange: one bi-stream, one JSEP envelope
/// each way, `finish` as the frame boundary.
///
/// Distinct from [`MOUNT_ALPN`] because it is a different conversation with a
/// different lifetime — signaling is short-lived and carries no bearer
/// secret, since the iroh connection underneath already authenticates both
/// ends. The mount data then rides a *fresh* connection over the negotiated
/// data channel.
pub const WEBRTC_SIGNAL_ALPN: &[u8] = b"agent-share/webrtc-signal/1";

/// Length of the bearer-capability secret carried in a mount ticket, and so
/// also of the share token derived from it.
pub const SECRET_LEN: usize = 32;

/// Per-request header: the 32-byte share token followed by the 1-byte op.
pub const REQUEST_HEADER_LEN: usize = SECRET_LEN + 1;

/// Connection close code: the token presented did not match, on a share that
/// carries no password.
///
/// The bearer is simply wrong, and there is nothing the peer can do about it —
/// which is what separates it from [`CLOSE_UNAUTHORIZED`].
pub const CLOSE_BAD_SECRET: u32 = 1;

/// Connection close code: the token presented did not match, on a share that
/// **is** password-protected.
///
/// Split from [`CLOSE_BAD_SECRET`] so a consumer can say "wrong password" and
/// offer another try, rather than reporting a share that refuses to talk. The
/// producer picks by what its *own* share is, never by guessing at why the peer
/// failed — so this code leaks only what the ticket's flag already says.
pub const CLOSE_UNAUTHORIZED: u32 = 2;

/// Request the manifest: the full dir + file listing with sizes and attrs.
pub const OP_MANIFEST: u8 = 1;

/// Request a byte range of one file, addressed by its manifest index.
pub const OP_READ: u8 = 2;

/// Subscribe to tree changes: one long-lived stream carrying the current
/// manifest, then a [`crate::manifest::ManifestDelta`] per change.
///
/// Additive on purpose, rather than a new ALPN. A producer that predates this
/// op drops the stream and keeps the connection (its `other =>` arm), so a
/// new consumer sees a clean end-of-stream and can fall back to the snapshot
/// it already has instead of failing the mount.
pub const OP_WATCH: u8 = 3;

/// Synthetic throughput / latency probe. Additive like [`OP_WATCH`]: an older
/// producer drops the stream and keeps the connection.
pub const OP_BENCH: u8 = 4;

/// Request the BLAKE3 root and bao outboard for one file, by manifest index.
///
/// The op that makes a third-party read safe. A consumer learns a file's hash
/// from the origin, over a channel already authenticated to the ticket's
/// endpoint id, and can then accept the *bytes* from any peer and check them
/// against it. A hostile peer can refuse or fail verification; it cannot
/// substitute content.
///
/// **The origin hashes on demand, never at scan time.** Hashing a tree up-front
/// is exactly what `manifest::MountManifest` refuses to do — it would turn
/// serving a 500 GB share from a `stat` walk into a full read of it, and that
/// laziness is the property distinguishing this design from `iroh-blobs`. So a
/// file gets a root the first time somebody asks for one, and not before.
///
/// Request body: `index(u32)`. Response: the status byte, then
/// `root(32) ‖ outboard_len(u32) ‖ outboard`.
pub const OP_HASH: u8 = 5;

/// Ceiling on one [`OP_HASH`] outboard.
///
/// An outboard is ~0.097 % of the file at the 64 `KiB` chunk groups this uses
/// (measured — RFC 03, S0.3), so this admits files into the terabytes while
/// still refusing an answer that could only come from a peer trying to exhaust
/// memory.
pub const MAX_OUTBOARD_BYTES: u32 = 64 * 1024 * 1024;

/// Request the ordered chunk addresses of one file, by manifest index.
///
/// The op that makes a third-party read safe, and the successor to
/// [`OP_HASH`]. A consumer learns a file's leaf row — the `blake3` address of
/// each 64 `KiB` chunk, in order — and can then accept **each chunk** from any
/// peer and check it on its own. A hostile peer can refuse or fail
/// verification; it cannot substitute content.
///
/// The difference from [`OP_HASH`] is what a peer can be asked for afterwards.
/// A bao outboard proves a range *of a particular file at a particular offset*,
/// so a reader must first agree with its source about which file it is reading.
/// A leaf row is a list of self-proving addresses, so a chunk can come from a
/// peer that has never heard of this share.
///
/// **The origin hashes on demand, never at scan time**, exactly as [`OP_HASH`]
/// did — a file gets a row the first time somebody asks for one, and not
/// before, so serving a 500 GB tree stays a `stat` walk.
///
/// Request body: `index(u32)`. Response: the status byte, then
/// `root(32) ‖ size(u64) ‖ count(u32) ‖ addresses(32 × count)`.
///
/// The size is carried because the root commits to it: a row of *n* addresses
/// describes a range of sizes (the last chunk may be short), and two files
/// differing only in that last chunk's length must not share a name.
pub const OP_CHUNK_MAP: u8 = 6;

/// Request one chunk, by its content address alone.
///
/// **No file, no offset, no manifest index.** That absence is the point: a peer
/// answers because it holds those bytes, not because it holds that file, so a
/// chunk can be served by someone who fetched it through an entirely different
/// share. It is also what lets a peer holding *part* of a file serve that part.
///
/// Still carries the request header's token, and a source **must answer only
/// for chunks reachable from a chunk map in that token's share**. A store
/// shared across shares would otherwise let anyone with one link probe what
/// else this peer holds, one address at a time.
///
/// Request body: `address(32)`. Response: the status byte, then
/// `len(u32) ‖ bytes`.
pub const OP_CHUNK: u8 = 7;

/// Ask a peer which chunks of a file it can actually serve.
///
/// The exact answer that a peer card cannot carry. Availability on the card is
/// a bounded hint — it rides a CRDT broadcast to everyone, which keeps its
/// history — so it says whether a peer is worth dialling. This says precisely
/// what that peer holds, over a connection that already exists.
///
/// Request body: `root(32)`. Response: the status byte, then
/// `chunks(u32) ‖ bitmap`, where bit *i* of the bitmap is chunk *i*, least
/// significant bit first, and the bitmap is `chunks.div_ceil(8)` bytes.
pub const OP_HAVE: u8 = 8;

/// Ask what changed since a version this consumer already holds.
///
/// A consumer coming back to a share it has seen before does not need the tree
/// again — on a large share that is several MB of manifest to learn that almost
/// nothing moved. It names the version it holds and gets the difference.
///
/// Request body: `since_version(u64)`. Response: the status byte, then
/// `target_version(u64) ‖ signature(64) ‖ count(u32) ‖ [len(u32) ‖ delta]…`,
/// each delta a [`crate::manifest::ManifestDelta`] to apply in order.
///
/// **Refused rather than answered when the producer cannot reach that far
/// back**, which a consumer answers by asking for the whole tree over
/// [`OP_MANIFEST`]. That is also what a seeder always does: it re-serves a
/// frozen snapshot and holds no history of its own.
///
/// # What makes an unsigned delta safe here
///
/// The deltas carry no signature — nothing in this protocol signs one. They do
/// not need to. The consumer applies the chain to the manifest it already holds,
/// re-encodes, and checks the **creator's** signature over
/// `(target_version, its own reconstruction)`. Encoding is canonical
/// (`encoding_is_canonical`), so a chain that is wrong in any way — truncated,
/// reordered, or forged by a peer in the middle — reconstructs bytes the
/// signature does not cover, and the consumer falls back to a full fetch instead
/// of adopting it. The proof is the same one [`OP_MANIFEST`] carries; only the
/// transport is cheaper.
pub const OP_MANIFEST_SINCE: u8 = 9;

/// Bytes in one chunk address.
pub const CHUNK_ADDRESS_LEN: usize = 32;

/// Ceiling on one [`OP_CHUNK`] body.
///
/// Every chunk but a file's last is exactly this, and the last is shorter, so
/// anything larger is a peer answering a question nobody asked.
pub const MAX_CHUNK_LEN: u32 = 64 * 1024;

/// Ceiling on one [`OP_CHUNK_MAP`] answer.
///
/// A leaf row is 32 bytes per 64 `KiB`, or ~0.05 % of the file — half the size
/// of the outboard it replaces. At this cap a single file may be up to 128 `GiB`
/// before its row stops fitting, which is well past anything the manifest's own
/// limits admit.
pub const MAX_CHUNK_MAP_BYTES: u32 = 64 * 1024 * 1024;

/// [`OP_BENCH`] kind: consumer sends `n` bytes; producer echoes them back.
pub const BENCH_KIND_ECHO: u8 = 0;

/// [`OP_BENCH`] kind: producer replies with `n` synthetic bytes (no echo body).
pub const BENCH_KIND_FILL: u8 = 1;

/// Cap on one echo payload (and thus one echo response body).
pub const MAX_BENCH_ECHO_BYTES: u32 = 64 * 1024;

/// Cap on one fill response body. Throughput runs issue many fill requests.
pub const MAX_BENCH_FILL_BYTES: u32 = 1024 * 1024;

/// Default consumer measurement window after connect (seconds).
pub const DEFAULT_BENCH_DURATION_SECS: u64 = 30;

/// How often the timed bench samples latency with an echo (seconds).
pub const BENCH_ECHO_INTERVAL_SECS: u64 = 1;

/// Ceiling on the encoded manifest, so a hostile producer can't force an
/// unbounded allocation before the first decode error.
pub const MAX_MANIFEST_BYTES: u32 = 64 * 1024 * 1024;

/// Ceiling on an [`OP_MANIFEST`] body, which wraps the manifest in the version
/// and signature of [`crate::authorship::SignedManifest`].
///
/// A separate constant rather than a bigger [`MAX_MANIFEST_BYTES`], because the
/// two guard different things: this bounds the allocation, that bounds the tree
/// a producer may serve. A reader checks the envelope against this before
/// allocating and the manifest inside it against that after decoding, so
/// neither cap moves because the other did.
pub const MAX_SIGNED_MANIFEST_BYTES: u32 = MAX_MANIFEST_BYTES + SIGNED_MANIFEST_PREFIX_LEN;

/// Bytes an envelope adds in front of the manifest: `version(u64) ‖
/// signature(64)`.
pub const SIGNED_MANIFEST_PREFIX_LEN: u32 = 8 + 64;

/// Ceiling on a single READ. Sized to fit the NFS client's `rsize=131072`
/// with headroom; the producer rejects anything larger without killing the
/// connection.
pub const MAX_READ_LEN: u32 = 256 * 1024;

/// First byte of a watch frame's body: the whole manifest follows.
///
/// Sent as the opening frame, and again whenever the producer cannot express
/// what happened as a delta the consumer could apply — a consumer that fell
/// too far behind, or a change too large for [`MAX_DELTA_BYTES`]. Receiving
/// one means "discard what you have and take this instead".
pub const WATCH_FRAME_MANIFEST: u8 = 0;

/// First byte of a watch frame's body: a [`crate::manifest::ManifestDelta`]
/// follows, to be applied to the state built from every frame before it.
pub const WATCH_FRAME_DELTA: u8 = 1;

/// Ceiling on one watch frame's delta. Far below [`MAX_MANIFEST_BYTES`]
/// because a delta describes a change, not a tree — a producer that wants to
/// say more than this has effectively rescanned, and the consumer is better
/// off being cut off than allocating for it. The producer splits oversized
/// batches rather than emitting a frame this large.
pub const MAX_DELTA_BYTES: u32 = 8 * 1024 * 1024;

/// Body length of an [`OP_READ`] request: `index(u32) ‖ offset(u64) ‖ len(u32)`.
pub const READ_REQUEST_LEN: usize = 16;

/// Fixed prefix of an [`OP_BENCH`] request after the op byte: `kind(u8) ‖ n(u32)`.
pub const BENCH_REQUEST_PREFIX_LEN: usize = 5;

/// Build the header for an [`OP_MANIFEST`] request. The manifest op has no
/// body, so this is the whole request.
#[must_use]
pub fn encode_manifest_request(token: &[u8; SECRET_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN);
    out.extend_from_slice(token);
    out.push(OP_MANIFEST);
    out
}

/// Ceiling on an [`OP_MANIFEST_SINCE`] answer.
///
/// A chain that grows past this is a producer describing more change than the
/// tree is worth: at that point the whole manifest is both smaller and simpler,
/// so it refuses and the consumer asks for one. Sized as a few of the largest
/// single deltas rather than as a share of the manifest cap, because what bounds
/// it is how much churn is worth catching up on.
pub const MAX_MANIFEST_SINCE_BYTES: u32 = 4 * MAX_DELTA_BYTES;

/// Build a complete [`OP_MANIFEST_SINCE`] request: header followed by
/// `since_version(u64)`.
#[must_use]
pub fn encode_manifest_since_request(token: &[u8; SECRET_LEN], since: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + 8);
    out.extend_from_slice(token);
    out.push(OP_MANIFEST_SINCE);
    out.extend_from_slice(&since.to_le_bytes());
    out
}

/// One producer's answer to [`OP_MANIFEST_SINCE`], before it is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestSince {
    /// The version the chain arrives at.
    pub target_version: u64,
    /// The creator's signature over that version's manifest — the thing the
    /// consumer checks its own reconstruction against.
    pub signature: [u8; 64],
    /// Deltas to apply in order, each an encoded
    /// [`crate::manifest::ManifestDelta`].
    pub deltas: Vec<Vec<u8>>,
}

impl ManifestSince {
    /// Wire layout: `target_version(u64) ‖ signature(64) ‖ count(u32) ‖
    /// [len(u32) ‖ delta]…`.
    ///
    /// # Panics
    /// More than `u32::MAX` deltas, or one longer than `u32::MAX` bytes — the
    /// same bounds every other length on this wire assumes, and both far past
    /// what [`MAX_MANIFEST_SINCE_BYTES`] admits.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.target_version.to_le_bytes());
        out.extend_from_slice(&self.signature);
        out.extend_from_slice(
            &u32::try_from(self.deltas.len())
                .expect("delta count fits u32")
                .to_le_bytes(),
        );
        for delta in &self.deltas {
            out.extend_from_slice(
                &u32::try_from(delta.len())
                    .expect("delta length fits u32")
                    .to_le_bytes(),
            );
            out.extend_from_slice(delta);
        }
        out
    }

    /// # Errors
    /// The body is truncated, carries trailing bytes, or claims a delta longer
    /// than [`MAX_DELTA_BYTES`] — each of which is a producer this consumer
    /// should stop believing rather than allocate for.
    ///
    /// # Panics
    /// Never: every fixed-width slice below is taken after a length check that
    /// covers it.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        use anyhow::{bail, ensure};

        ensure!(
            bytes.len() >= 8 + 64 + 4,
            "a manifest-since answer is short"
        );
        let target_version = u64::from_le_bytes(bytes[..8].try_into().expect("8 bytes"));
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes[8..72]);
        let count = u32::from_le_bytes(bytes[72..76].try_into().expect("4 bytes")) as usize;
        let mut cursor = 76;
        // Capacity from the bytes actually present, never from `count`: the
        // claim is the hostile input here.
        let mut deltas = Vec::new();
        for _ in 0..count {
            ensure!(bytes.len() >= cursor + 4, "a delta length is truncated");
            let len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().expect("4 bytes"));
            if len > MAX_DELTA_BYTES {
                bail!("a delta claims {len} bytes, past the cap");
            }
            let len = len as usize;
            cursor += 4;
            ensure!(bytes.len() >= cursor + len, "a delta is truncated");
            deltas.push(bytes[cursor..cursor + len].to_vec());
            cursor += len;
        }
        ensure!(cursor == bytes.len(), "trailing bytes after the deltas");
        Ok(Self {
            target_version,
            signature,
            deltas,
        })
    }
}

/// Build a complete [`OP_HASH`] request: header followed by `index(u32)`.
#[must_use]
pub fn encode_hash_request(token: &[u8; SECRET_LEN], index: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + 4);
    out.extend_from_slice(token);
    out.push(OP_HASH);
    out.extend_from_slice(&index.to_le_bytes());
    out
}

/// Decode an [`OP_HASH`] request body — the producer's side of
/// [`encode_hash_request`].
///
/// # Errors
/// The body is not exactly four bytes.
pub fn decode_hash_request(body: &[u8]) -> Result<u32> {
    let bytes: [u8; 4] = body
        .try_into()
        .map_err(|_| anyhow::anyhow!("hash request body must be 4 bytes, got {}", body.len()))?;
    Ok(u32::from_le_bytes(bytes))
}

/// Build a complete [`OP_CHUNK_MAP`] request: header followed by `index(u32)`.
#[must_use]
pub fn encode_chunk_map_request(token: &[u8; SECRET_LEN], index: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + 4);
    out.extend_from_slice(token);
    out.push(OP_CHUNK_MAP);
    out.extend_from_slice(&index.to_le_bytes());
    out
}

/// Decode an [`OP_CHUNK_MAP`] request body.
///
/// # Errors
/// The body is not exactly four bytes.
pub fn decode_chunk_map_request(body: &[u8]) -> Result<u32> {
    let bytes: [u8; 4] = body.try_into().map_err(|_| {
        anyhow::anyhow!("chunk map request body must be 4 bytes, got {}", body.len())
    })?;
    Ok(u32::from_le_bytes(bytes))
}

/// Fixed prefix of an [`OP_CHUNK_MAP`] answer: `root(32) ‖ size(u64) ‖ count(u32)`.
pub const CHUNK_MAP_PREFIX_LEN: usize = 44;

/// Encode an [`OP_CHUNK_MAP`] answer body.
#[must_use]
pub fn encode_chunk_map(root: &[u8; 32], size: u64, addresses: &[[u8; 32]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(CHUNK_MAP_PREFIX_LEN + addresses.len() * CHUNK_ADDRESS_LEN);
    out.extend_from_slice(root);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(addresses.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    for address in addresses {
        out.extend_from_slice(address);
    }
    out
}

/// Decode an [`OP_CHUNK_MAP`] answer body into `(root, size, addresses)`.
///
/// # Errors
/// The body is truncated, or claims more addresses than it carries. The count
/// is checked against the *actual* length before anything is allocated, so a
/// peer cannot make this reserve memory it never intends to fill.
///
/// # Panics
/// Never: the slices taken below are inside the length checked immediately
/// above them, so the `try_into` conversions cannot fail.
pub fn decode_chunk_map(body: &[u8]) -> Result<([u8; 32], u64, Vec<[u8; 32]>)> {
    if body.len() < CHUNK_MAP_PREFIX_LEN {
        anyhow::bail!(
            "a chunk map answer is at least {CHUNK_MAP_PREFIX_LEN} bytes, got {}",
            body.len()
        );
    }
    let mut root = [0u8; 32];
    root.copy_from_slice(&body[..32]);
    let size = u64::from_le_bytes(body[32..40].try_into().expect("8 bytes"));
    let count = u32::from_le_bytes(body[40..44].try_into().expect("4 bytes")) as usize;
    let needed = CHUNK_MAP_PREFIX_LEN
        .checked_add(
            count
                .checked_mul(CHUNK_ADDRESS_LEN)
                .ok_or_else(|| anyhow::anyhow!("chunk map length overflows"))?,
        )
        .ok_or_else(|| anyhow::anyhow!("chunk map length overflows"))?;
    if body.len() < needed {
        anyhow::bail!(
            "a {count}-address chunk map needs {needed} bytes, got {}",
            body.len()
        );
    }
    let mut addresses = Vec::with_capacity(count);
    for index in 0..count {
        let start = CHUNK_MAP_PREFIX_LEN + index * CHUNK_ADDRESS_LEN;
        let mut address = [0u8; 32];
        address.copy_from_slice(&body[start..start + CHUNK_ADDRESS_LEN]);
        addresses.push(address);
    }
    Ok((root, size, addresses))
}

/// Build a complete [`OP_CHUNK`] request: header followed by `address(32)`.
#[must_use]
pub fn encode_chunk_request(token: &[u8; SECRET_LEN], address: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + CHUNK_ADDRESS_LEN);
    out.extend_from_slice(token);
    out.push(OP_CHUNK);
    out.extend_from_slice(address);
    out
}

/// Decode an [`OP_CHUNK`] request body.
///
/// # Errors
/// The body is not exactly 32 bytes.
pub fn decode_chunk_request(body: &[u8]) -> Result<[u8; 32]> {
    body.try_into().map_err(|_| {
        anyhow::anyhow!(
            "chunk request body must be {CHUNK_ADDRESS_LEN} bytes, got {}",
            body.len()
        )
    })
}

/// Build a complete [`OP_HAVE`] request: header followed by `root(32)`.
#[must_use]
pub fn encode_have_request(token: &[u8; SECRET_LEN], root: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + 32);
    out.extend_from_slice(token);
    out.push(OP_HAVE);
    out.extend_from_slice(root);
    out
}

/// Decode an [`OP_HAVE`] request body.
///
/// # Errors
/// The body is not exactly 32 bytes.
pub fn decode_have_request(body: &[u8]) -> Result<[u8; 32]> {
    body.try_into()
        .map_err(|_| anyhow::anyhow!("have request body must be 32 bytes, got {}", body.len()))
}

/// Encode an [`OP_HAVE`] answer body: `chunks(u32) ‖ bitmap`.
#[must_use]
pub fn encode_have(chunks: u32, bitmap: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + bitmap.len());
    out.extend_from_slice(&chunks.to_le_bytes());
    out.extend_from_slice(bitmap);
    out
}

/// Decode an [`OP_HAVE`] answer body into `(chunks, bitmap)`.
///
/// # Errors
/// The body is truncated, or the bitmap is too short for the count it claims.
///
/// # Panics
/// Never: the four bytes read below are inside the length checked immediately
/// above them.
pub fn decode_have(body: &[u8]) -> Result<(u32, Vec<u8>)> {
    if body.len() < 4 {
        anyhow::bail!("a have answer is at least 4 bytes, got {}", body.len());
    }
    let chunks = u32::from_le_bytes(body[..4].try_into().expect("4 bytes"));
    let needed = (chunks as usize).div_ceil(8);
    if body.len() - 4 < needed {
        anyhow::bail!(
            "a {chunks}-chunk bitmap needs {needed} bytes, got {}",
            body.len() - 4
        );
    }
    Ok((chunks, body[4..4 + needed].to_vec()))
}

/// Build the header for an [`OP_WATCH`] request. Like the manifest op it has
/// no body; unlike it, the response never ends until the share does.
#[must_use]
pub fn encode_watch_request(token: &[u8; SECRET_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN);
    out.extend_from_slice(token);
    out.push(OP_WATCH);
    out
}

/// Build a complete [`OP_READ`] request: header followed by the 16-byte body.
#[must_use]
pub fn encode_read_request(token: &[u8; SECRET_LEN], index: u32, offset: u64, len: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + READ_REQUEST_LEN);
    out.extend_from_slice(token);
    out.push(OP_READ);
    out.extend_from_slice(&index.to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out
}

/// Build an [`OP_BENCH`] echo request: header + kind/len + payload.
///
/// # Errors
/// `payload.len()` exceeds [`MAX_BENCH_ECHO_BYTES`].
pub fn encode_bench_echo_request(token: &[u8; SECRET_LEN], payload: &[u8]) -> Result<Vec<u8>> {
    let len = u32::try_from(payload.len()).context("echo payload too large for u32")?;
    if len > MAX_BENCH_ECHO_BYTES {
        bail!("echo payload {len} exceeds cap {MAX_BENCH_ECHO_BYTES}");
    }
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + BENCH_REQUEST_PREFIX_LEN + payload.len());
    out.extend_from_slice(token);
    out.push(OP_BENCH);
    out.push(BENCH_KIND_ECHO);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Build an [`OP_BENCH`] fill request: header + kind/len (no body).
///
/// # Errors
/// `len` exceeds [`MAX_BENCH_FILL_BYTES`] or is zero.
pub fn encode_bench_fill_request(token: &[u8; SECRET_LEN], len: u32) -> Result<Vec<u8>> {
    if len == 0 || len > MAX_BENCH_FILL_BYTES {
        bail!("fill length {len} must be in 1..={MAX_BENCH_FILL_BYTES}");
    }
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + BENCH_REQUEST_PREFIX_LEN);
    out.extend_from_slice(token);
    out.push(OP_BENCH);
    out.push(BENCH_KIND_FILL);
    out.extend_from_slice(&len.to_le_bytes());
    Ok(out)
}

/// Decode the `kind ‖ len` prefix of an [`OP_BENCH`] request.
///
/// # Errors
/// Prefix is short or kind is unknown.
#[expect(
    clippy::missing_panics_doc,
    reason = "the length check above makes the 4-byte slice exactly sized"
)]
pub fn decode_bench_request_prefix(prefix: &[u8]) -> Result<(u8, u32)> {
    if prefix.len() < BENCH_REQUEST_PREFIX_LEN {
        bail!(
            "bench request prefix must be {BENCH_REQUEST_PREFIX_LEN} bytes, got {}",
            prefix.len()
        );
    }
    let kind = prefix[0];
    let len = u32::from_le_bytes(prefix[1..5].try_into().expect("4 bytes"));
    match kind {
        BENCH_KIND_ECHO | BENCH_KIND_FILL => Ok((kind, len)),
        other => bail!("unknown bench kind {other}"),
    }
}

/// Decode an [`OP_READ`] request body — the producer's side of
/// [`encode_read_request`], returning `(index, offset, len)`.
///
/// # Errors
/// The body is not exactly [`READ_REQUEST_LEN`] bytes.
#[expect(
    clippy::missing_panics_doc,
    reason = "the length check above makes every slice below exactly sized"
)]
pub fn decode_read_request(body: &[u8]) -> Result<(u32, u64, u32)> {
    if body.len() != READ_REQUEST_LEN {
        bail!(
            "read request must be {READ_REQUEST_LEN} bytes, got {}",
            body.len()
        );
    }
    let index = u32::from_le_bytes(body[..4].try_into().expect("4 bytes"));
    let offset = u64::from_le_bytes(body[4..12].try_into().expect("8 bytes"));
    let len = u32::from_le_bytes(body[12..].try_into().expect("4 bytes"));
    Ok((index, offset, len))
}

/// Interpret the `status(1) ‖ len(u32 LE)` prefix every response carries,
/// returning the declared payload length.
///
/// Split from the payload read because the caller owns the stream: a native
/// consumer reads the rest with `read_exact`, the browser from an already
/// buffered chunk. `requested` is the cap the response must not exceed —
/// [`MAX_MANIFEST_BYTES`] for a manifest, the requested `len` for a read.
///
/// # Errors
/// A short prefix, an unknown status byte, a non-`Ok` status, or a declared
/// length above `requested`.
#[expect(
    clippy::missing_panics_doc,
    reason = "`get(1..5)` already proved the slice is four bytes"
)]
pub fn decode_response_header(prefix: &[u8], requested: u32) -> Result<u32> {
    let status = *prefix
        .first()
        .context("response is missing its status byte")?;
    match ReadStatus::from_byte(status)? {
        ReadStatus::Ok => {}
        ReadStatus::BadIndex => bail!("the producer does not know that file index"),
        ReadStatus::Io => bail!("the producer failed to read that file"),
        ReadStatus::LenOverCap => bail!("the read exceeds the producer's cap"),
    }
    let raw = prefix.get(1..5).context("response is missing its length")?;
    let len = u32::from_le_bytes(raw.try_into().expect("4 bytes"));
    if len > requested {
        bail!("the producer sent more than requested: {len} > {requested}");
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::{
        BENCH_ECHO_INTERVAL_SECS, BENCH_KIND_ECHO, BENCH_KIND_FILL, CLOSE_BAD_SECRET,
        CLOSE_UNAUTHORIZED, DEFAULT_BENCH_DURATION_SECS, MAX_BENCH_ECHO_BYTES,
        MAX_BENCH_FILL_BYTES, MAX_DELTA_BYTES, MAX_MANIFEST_BYTES, MAX_OUTBOARD_BYTES,
        MAX_READ_LEN, MOUNT_ALPN, OP_BENCH, OP_HASH, OP_MANIFEST, OP_READ, OP_WATCH,
        REQUEST_HEADER_LEN, SECRET_LEN, WEBRTC_SIGNAL_ALPN, decode_bench_request_prefix,
        decode_hash_request, decode_read_request, decode_response_header,
        encode_bench_echo_request, encode_bench_fill_request, encode_hash_request,
        encode_manifest_request, encode_read_request,
    };
    use crate::manifest::ReadStatus;

    #[test]
    fn a_hash_request_round_trips() {
        let secret = [3u8; SECRET_LEN];
        let request = encode_hash_request(&secret, 42);
        assert_eq!(&request[..SECRET_LEN], &secret);
        assert_eq!(request[SECRET_LEN], OP_HASH);
        assert_eq!(
            decode_hash_request(&request[REQUEST_HEADER_LEN..]).expect("decode"),
            42
        );
    }

    #[test]
    fn a_malformed_hash_request_is_rejected() {
        assert!(decode_hash_request(&[]).is_err());
        assert!(decode_hash_request(&[0, 0, 0]).is_err(), "too short");
        assert!(decode_hash_request(&[0, 0, 0, 0, 0]).is_err(), "too long");
    }

    #[test]
    fn wire_constants_are_pinned() {
        // Wire-format pins for `agent-share`'s own mount protocol. Changing
        // one is allowed — nothing here promises compatibility — but it must
        // be a deliberate edit, never a refactor's side effect: two builds of
        // the same version still have to agree, so a constant that shifts by
        // accident silently breaks a producer on `main` against a consumer on
        // a branch. The op codes and secret length are still bit-identical to
        // agent-habilis/swarm's `ahsw mount`; only the ALPN was forked.
        assert_eq!(MOUNT_ALPN, b"agent-share/mount/1");
        assert_eq!(WEBRTC_SIGNAL_ALPN, b"agent-share/webrtc-signal/1");
        assert_eq!(OP_MANIFEST, 1);
        assert_eq!(OP_READ, 2);
        // Added after the fork. The graceful-degradation path this once had
        // — an unknown op costing one stream rather than the connection — is
        // no longer required of new ops, since every peer runs the same build.
        // It stays here because it is already written and costs nothing.
        assert_eq!(OP_WATCH, 3);
        assert_eq!(OP_BENCH, 4);
        // Added for RFC 03. The op number is the *only* thing about hashing
        // that belongs in this crate — the outboard format, the store and the
        // verification all live in `fofoca-blobs`, which knows nothing about
        // shares. Reserving a number is not learning about blobs.
        assert_eq!(OP_HASH, 5);
        assert_eq!(MAX_OUTBOARD_BYTES, 64 * 1024 * 1024);
        assert_eq!(BENCH_KIND_ECHO, 0);
        assert_eq!(BENCH_KIND_FILL, 1);
        assert_eq!(MAX_BENCH_ECHO_BYTES, 64 * 1024);
        assert_eq!(MAX_BENCH_FILL_BYTES, 1024 * 1024);
        assert_eq!(DEFAULT_BENCH_DURATION_SECS, 30);
        assert_eq!(BENCH_ECHO_INTERVAL_SECS, 1);
        assert_eq!(SECRET_LEN, 32);
        // Close codes. `1` predates the split and keeps its meaning; `2` is the
        // one a consumer reads as "wrong password", so swapping them would turn
        // a retryable prompt into a dead end.
        assert_eq!(CLOSE_BAD_SECRET, 1);
        assert_eq!(CLOSE_UNAUTHORIZED, 2);
        assert_eq!(MAX_MANIFEST_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_READ_LEN, 256 * 1024);
        assert_eq!(MAX_DELTA_BYTES, 8 * 1024 * 1024);
    }

    #[test]
    fn manifest_request_is_header_only() {
        let request = encode_manifest_request(&[7u8; SECRET_LEN]);
        assert_eq!(request.len(), SECRET_LEN + 1);
        assert_eq!(&request[..SECRET_LEN], &[7u8; SECRET_LEN]);
        assert_eq!(request[SECRET_LEN], OP_MANIFEST);
    }

    #[test]
    fn read_request_round_trips() {
        let request = encode_read_request(&[3u8; SECRET_LEN], 42, 1_000_000, 4096);
        assert_eq!(request[SECRET_LEN], OP_READ);
        let (index, offset, len) =
            decode_read_request(&request[SECRET_LEN + 1..]).expect("decode body");
        assert_eq!((index, offset, len), (42, 1_000_000, 4096));
    }

    #[test]
    fn read_request_rejects_a_wrong_sized_body() {
        assert!(decode_read_request(&[0u8; 15]).is_err());
        assert!(decode_read_request(&[0u8; 17]).is_err());
    }

    #[test]
    fn bench_echo_request_round_trips() {
        let payload = b"ping";
        let request = encode_bench_echo_request(&[9u8; SECRET_LEN], payload).expect("encode");
        assert_eq!(request[SECRET_LEN], OP_BENCH);
        let (kind, len) = decode_bench_request_prefix(&request[SECRET_LEN + 1..]).expect("prefix");
        assert_eq!((kind, len), (BENCH_KIND_ECHO, 4));
        assert_eq!(&request[SECRET_LEN + 1 + 5..], payload);
    }

    #[test]
    fn bench_fill_request_encodes() {
        let request = encode_bench_fill_request(&[1u8; SECRET_LEN], 1024).expect("encode");
        assert_eq!(request[SECRET_LEN], OP_BENCH);
        let (kind, len) = decode_bench_request_prefix(&request[SECRET_LEN + 1..]).expect("prefix");
        assert_eq!((kind, len), (BENCH_KIND_FILL, 1024));
    }

    #[test]
    fn bench_rejects_oversize_echo_and_fill() {
        let big = vec![0u8; (MAX_BENCH_ECHO_BYTES as usize) + 1];
        assert!(encode_bench_echo_request(&[0u8; SECRET_LEN], &big).is_err());
        assert!(encode_bench_fill_request(&[0u8; SECRET_LEN], 0).is_err());
        assert!(encode_bench_fill_request(&[0u8; SECRET_LEN], MAX_BENCH_FILL_BYTES + 1).is_err());
    }

    #[test]
    fn response_header_reads_the_length() {
        let mut prefix = vec![ReadStatus::Ok.to_byte()];
        prefix.extend_from_slice(&99u32.to_le_bytes());
        assert_eq!(decode_response_header(&prefix, 100).expect("ok"), 99);
    }

    #[test]
    fn response_header_rejects_overlong_and_failed_reads() {
        let mut over = vec![ReadStatus::Ok.to_byte()];
        over.extend_from_slice(&101u32.to_le_bytes());
        assert!(
            decode_response_header(&over, 100).is_err(),
            "a producer must not exceed the requested length"
        );

        for status in [ReadStatus::BadIndex, ReadStatus::Io, ReadStatus::LenOverCap] {
            let mut failed = vec![status.to_byte()];
            failed.extend_from_slice(&0u32.to_le_bytes());
            assert!(decode_response_header(&failed, 100).is_err(), "{status:?}");
        }

        assert!(decode_response_header(&[], 100).is_err(), "empty prefix");
        assert!(
            decode_response_header(&[ReadStatus::Ok.to_byte(), 0, 0], 100).is_err(),
            "truncated length"
        );
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::{
        CHUNK_ADDRESS_LEN, MAX_CHUNK_LEN, MAX_CHUNK_MAP_BYTES, OP_CHUNK, OP_CHUNK_MAP, OP_HAVE,
        REQUEST_HEADER_LEN, SECRET_LEN, decode_chunk_map, decode_chunk_map_request,
        decode_chunk_request, decode_have, decode_have_request, encode_chunk_map,
        encode_chunk_map_request, encode_chunk_request, encode_have, encode_have_request,
    };

    fn token() -> [u8; SECRET_LEN] {
        [7u8; SECRET_LEN]
    }

    fn address(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    /// The op bytes are wire format. Changing one silently reroutes every
    /// request built by an older peer into a different handler, so they are
    /// pinned here rather than left to whoever renumbers next.
    #[test]
    fn the_op_bytes_are_pinned() {
        assert_eq!(OP_CHUNK_MAP, 6);
        assert_eq!(OP_CHUNK, 7);
        assert_eq!(OP_HAVE, 8);
        assert_eq!(CHUNK_ADDRESS_LEN, 32);
        assert_eq!(MAX_CHUNK_LEN, 64 * 1024);
        assert_eq!(MAX_CHUNK_MAP_BYTES, 64 * 1024 * 1024);
    }

    #[test]
    fn a_chunk_map_request_round_trips() {
        let request = encode_chunk_map_request(&token(), 42);
        assert_eq!(request[SECRET_LEN], OP_CHUNK_MAP);
        assert_eq!(
            decode_chunk_map_request(&request[REQUEST_HEADER_LEN..]).expect("decode"),
            42
        );
        assert!(decode_chunk_map_request(&[1, 2]).is_err());
    }

    #[test]
    fn a_chunk_map_round_trips() {
        let addresses = vec![address(1), address(2), address(3)];
        let body = encode_chunk_map(&address(9), 131_073, &addresses);
        let (root, size, decoded) = decode_chunk_map(&body).expect("decode");
        assert_eq!(root, address(9));
        assert_eq!(size, 131_073);
        assert_eq!(decoded, addresses);
    }

    #[test]
    fn an_empty_chunk_map_round_trips() {
        // A zero-byte file has a root and no addresses, and that has to survive
        // the wire rather than being mistaken for a truncated answer.
        let body = encode_chunk_map(&address(4), 0, &[]);
        let (root, size, decoded) = decode_chunk_map(&body).expect("decode");
        assert_eq!(root, address(4));
        assert_eq!(size, 0);
        assert!(decoded.is_empty());
    }

    /// A count larger than the body is a peer trying to make us allocate for
    /// data it never sends. The length is checked before the `Vec` is reserved.
    #[test]
    fn a_chunk_map_claiming_more_than_it_carries_is_refused() {
        let mut body = encode_chunk_map(&address(5), 64 * 1024, &[address(6)]);
        body[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_chunk_map(&body).is_err());
        // Truncated bodies too.
        assert!(decode_chunk_map(&body[..10]).is_err());
        assert!(decode_chunk_map(&[]).is_err());
    }

    #[test]
    fn a_chunk_request_round_trips() {
        let request = encode_chunk_request(&token(), &address(11));
        assert_eq!(request[SECRET_LEN], OP_CHUNK);
        assert_eq!(
            decode_chunk_request(&request[REQUEST_HEADER_LEN..]).expect("decode"),
            address(11)
        );
        assert!(decode_chunk_request(&[0u8; 31]).is_err());
        assert!(decode_chunk_request(&[0u8; 33]).is_err());
    }

    /// The op that buys the resilience carries no file and no offset. If a body
    /// ever grows one, a peer stops being able to answer for bytes it got
    /// through a different share — which is the whole point of the op.
    #[test]
    fn a_chunk_request_names_only_an_address() {
        let request = encode_chunk_request(&token(), &address(12));
        assert_eq!(request.len(), REQUEST_HEADER_LEN + CHUNK_ADDRESS_LEN);
    }

    #[test]
    fn a_have_request_round_trips() {
        let request = encode_have_request(&token(), &address(13));
        assert_eq!(request[SECRET_LEN], OP_HAVE);
        assert_eq!(
            decode_have_request(&request[REQUEST_HEADER_LEN..]).expect("decode"),
            address(13)
        );
        assert!(decode_have_request(&[0u8; 8]).is_err());
    }

    #[test]
    fn a_have_answer_round_trips() {
        // 10 chunks, holding 0, 3 and 9.
        let bitmap = [0b0000_1001u8, 0b0000_0010u8];
        let body = encode_have(10, &bitmap);
        let (chunks, decoded) = decode_have(&body).expect("decode");
        assert_eq!(chunks, 10);
        assert_eq!(decoded, bitmap);
    }

    #[test]
    fn a_have_answer_with_a_short_bitmap_is_refused() {
        let body = encode_have(64, &[0u8; 2]);
        assert!(decode_have(&body).is_err());
        assert!(decode_have(&[1, 2]).is_err());
    }

    #[test]
    fn a_have_answer_for_an_empty_file_round_trips() {
        let body = encode_have(0, &[]);
        let (chunks, bitmap) = decode_have(&body).expect("decode");
        assert_eq!(chunks, 0);
        assert!(bitmap.is_empty());
    }
}
