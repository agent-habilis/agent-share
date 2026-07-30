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

/// Ceiling on the encoded manifest, so a hostile producer can't force an
/// unbounded allocation before the first decode error.
pub const MAX_MANIFEST_BYTES: u32 = 64 * 1024 * 1024;

/// Ceiling on a single READ. Sized to fit the NFS client's `rsize=131072`
/// with headroom; the producer rejects anything larger without killing the
/// connection.
pub const MAX_READ_LEN: u32 = 256 * 1024;

/// Body length of an [`OP_READ`] request: `index(u32) ‖ offset(u64) ‖ len(u32)`.
pub const READ_REQUEST_LEN: usize = 16;

/// Build the header for an [`OP_MANIFEST`] request. The manifest op has no
/// body, so this is the whole request.
#[must_use]
pub fn encode_manifest_request(secret: &[u8; SECRET_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REQUEST_HEADER_LEN);
    out.extend_from_slice(secret);
    out.push(OP_MANIFEST);
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
        MAX_MANIFEST_BYTES, MAX_READ_LEN, MOUNT_ALPN, OP_MANIFEST, OP_READ, SECRET_LEN,
        WEBRTC_SIGNAL_ALPN, decode_read_request, decode_response_header, encode_manifest_request,
        encode_read_request,
    };
    use crate::manifest::ReadStatus;

    #[test]
    fn wire_constants_are_pinned() {
        // Wire-format pins for `agent-share`'s own mount protocol: a change
        // here breaks every already-issued ticket and every peer running an
        // older build, so it must be a deliberate edit, never a refactor's
        // side effect. The op codes and secret length are still bit-identical
        // to agent-habilis/swarm's `ahsw mount`; only the ALPN was forked.
        assert_eq!(MOUNT_ALPN, b"agent-share/mount/1");
        assert_eq!(WEBRTC_SIGNAL_ALPN, b"agent-share/webrtc-signal/1");
        assert_eq!(OP_MANIFEST, 1);
        assert_eq!(OP_READ, 2);
        assert_eq!(SECRET_LEN, 32);
        assert_eq!(MAX_MANIFEST_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_READ_LEN, 256 * 1024);
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
