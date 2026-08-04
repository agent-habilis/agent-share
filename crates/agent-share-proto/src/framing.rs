//! The mount protocol's identity and byte layouts.
//!
//! One QUIC bi-stream per request. Every stream opens with
//! `secret(32) ‖ op(1)`; the op decides what follows. The producer treats a
//! bad secret as fatal to the whole connection and an unknown op as fatal to
//! just that stream.

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

/// Length of the bearer-capability secret carried in a mount ticket.
pub const SECRET_LEN: usize = 32;

/// Per-request header: the 32-byte bearer secret followed by the 1-byte op.
pub const REQUEST_HEADER_LEN: usize = SECRET_LEN + 1;

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
pub fn encode_manifest_request(secret: &[u8; SECRET_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN);
    out.extend_from_slice(secret);
    out.push(OP_MANIFEST);
    out
}

/// Build a complete [`OP_HASH`] request: header followed by `index(u32)`.
#[must_use]
pub fn encode_hash_request(secret: &[u8; SECRET_LEN], index: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + 4);
    out.extend_from_slice(secret);
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

/// Build the header for an [`OP_WATCH`] request. Like the manifest op it has
/// no body; unlike it, the response never ends until the share does.
#[must_use]
pub fn encode_watch_request(secret: &[u8; SECRET_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN);
    out.extend_from_slice(secret);
    out.push(OP_WATCH);
    out
}

/// Build a complete [`OP_READ`] request: header followed by the 16-byte body.
#[must_use]
pub fn encode_read_request(
    secret: &[u8; SECRET_LEN],
    index: u32,
    offset: u64,
    len: u32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + READ_REQUEST_LEN);
    out.extend_from_slice(secret);
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
pub fn encode_bench_echo_request(secret: &[u8; SECRET_LEN], payload: &[u8]) -> Result<Vec<u8>> {
    let len = u32::try_from(payload.len()).context("echo payload too large for u32")?;
    if len > MAX_BENCH_ECHO_BYTES {
        bail!("echo payload {len} exceeds cap {MAX_BENCH_ECHO_BYTES}");
    }
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + BENCH_REQUEST_PREFIX_LEN + payload.len());
    out.extend_from_slice(secret);
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
pub fn encode_bench_fill_request(secret: &[u8; SECRET_LEN], len: u32) -> Result<Vec<u8>> {
    if len == 0 || len > MAX_BENCH_FILL_BYTES {
        bail!("fill length {len} must be in 1..={MAX_BENCH_FILL_BYTES}");
    }
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN + BENCH_REQUEST_PREFIX_LEN);
    out.extend_from_slice(secret);
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
        BENCH_ECHO_INTERVAL_SECS, BENCH_KIND_ECHO, BENCH_KIND_FILL, DEFAULT_BENCH_DURATION_SECS,
        MAX_BENCH_ECHO_BYTES, MAX_BENCH_FILL_BYTES, MAX_DELTA_BYTES, MAX_MANIFEST_BYTES,
        MAX_OUTBOARD_BYTES, MAX_READ_LEN, MOUNT_ALPN, OP_BENCH, OP_HASH, OP_MANIFEST, OP_READ,
        OP_WATCH, REQUEST_HEADER_LEN, SECRET_LEN, WEBRTC_SIGNAL_ALPN, decode_bench_request_prefix,
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
