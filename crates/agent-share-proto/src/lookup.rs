//! The lookup allowlist carried in a ticket (`mdns`/`dht`/`relay`) plus its
//! byte codec.
//!
//! This is the **wire half** of the allowlist: the types a ticket embeds and
//! the bytes they encode to. The CLI half that turns `--mdns/--dht/--relay`
//! flags into a [`LookupOpts`] stays in the binary, because a browser has no
//! flags to resolve and no business linking a `clap` value parser.
//!
//! A share's network reach is fully described by its lookups: no lookups
//! means loopback-only; any lookup means reachable across machines.

use anyhow::{Context, Result, bail};
use fofoca_protocol::iroh_base::RelayUrl;

/// The connectivity relay. `Disabled` ⇒ no relay at all
/// (`RelayMode::Disabled`); `Pinned` ⇒ the lookup-layer pinned default
/// *ladder*; `Custom` ⇒ an operator-supplied **ordered ladder**
/// (`--relay a,b,c`). Relay is an allowlist member like mdns/dht, not an
/// always-on URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayChoice {
    Disabled,
    Pinned,
    Custom(Vec<RelayUrl>),
}

/// The lookup allowlist baked into a ticket. `mdns`/`dht` are the enabled
/// iroh address-lookups; `relay` is the connectivity relay (see
/// [`RelayChoice`]). An all-off set is a loopback-only share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupOpts {
    pub mdns: bool,
    pub dht: bool,
    pub relay: RelayChoice,
}

/// Wire ceiling on a custom relay ladder, so a forged id can't blow up
/// allocation. Far above any real ladder.
const MAX_RELAY_LADDER: usize = 16;
/// Wire ceiling on a single relay URL's byte length.
const MAX_RELAY_URL_BYTES: usize = 512;

impl LookupOpts {
    /// Loopback-only: no address-lookups, no relay.
    #[must_use]
    pub fn loopback() -> Self {
        LookupOpts {
            mdns: false,
            dht: false,
            relay: RelayChoice::Disabled,
        }
    }

    /// The all-on default for a share reachable across machines: both
    /// address-lookups plus the pinned default relay ladder.
    #[must_use]
    pub fn public_preset() -> Self {
        LookupOpts {
            mdns: true,
            dht: true,
            relay: RelayChoice::Pinned,
        }
    }

    /// True when nothing reaches off-machine — the share is loopback-only.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        !self.mdns && !self.dht && self.relay == RelayChoice::Disabled
    }

    /// Human/JSON label for the share's reach. Derived from the lookups —
    /// there is no stored network mode.
    #[must_use]
    pub fn network_label(&self) -> &'static str {
        if self.is_loopback() {
            "private"
        } else {
            "public"
        }
    }

    /// Append the canonical wire encoding to `buf`:
    /// `[flags u8][if custom: [count u8] ([len u16 LE] url)*]`.
    ///
    /// # Panics
    /// If a custom ladder exceeds `MAX_RELAY_LADDER` rungs or carries a URL
    /// over `MAX_RELAY_URL_BYTES`. Decoding rejects both, and the CLI parser
    /// never builds one, so only a hand-built value can trip it.
    pub fn encode_into(&self, buf: &mut Vec<u8>) {
        let mut flags: u8 = 0;
        if self.mdns {
            flags |= 0b0001;
        }
        if self.dht {
            flags |= 0b0010;
        }
        match &self.relay {
            RelayChoice::Disabled => {}
            RelayChoice::Pinned => flags |= 0b0100,
            RelayChoice::Custom(_) => flags |= 0b0100 | 0b1000,
        }
        buf.push(flags);
        if let RelayChoice::Custom(ladder) = &self.relay {
            // The ladder is created locally and bounded by the CLI/embed,
            // so this cast and the lengths below always fit.
            buf.push(u8::try_from(ladder.len()).expect("relay ladder bounded by MAX_RELAY_LADDER"));
            for url in ladder {
                let text = url.to_string();
                let len =
                    u16::try_from(text.len()).expect("relay URL bounded by MAX_RELAY_URL_BYTES");
                buf.extend_from_slice(&len.to_le_bytes());
                buf.extend_from_slice(text.as_bytes());
            }
        }
    }

    /// Decode from a cursor over the config region, advancing `pos`.
    ///
    /// # Errors
    /// Truncated input, an empty or over-long ladder, or an invalid relay URL.
    pub fn decode_from(bytes: &[u8], pos: &mut usize) -> Result<Self> {
        let flags = *bytes.get(*pos).context("truncated lookup flags")?;
        *pos += 1;
        let mdns = flags & 0b0001 != 0;
        let dht = flags & 0b0010 != 0;
        let relay_enabled = flags & 0b0100 != 0;
        let relay_custom = flags & 0b1000 != 0;
        if relay_custom && !relay_enabled {
            bail!("custom-relay bit set without relay-enabled bit");
        }
        let relay = if !relay_enabled {
            RelayChoice::Disabled
        } else if !relay_custom {
            RelayChoice::Pinned
        } else {
            let count = *bytes.get(*pos).context("truncated relay ladder count")? as usize;
            *pos += 1;
            if count == 0 {
                bail!("custom relay ladder is empty");
            }
            if count > MAX_RELAY_LADDER {
                bail!("relay ladder too long: {count}");
            }
            let mut ladder = Vec::with_capacity(count);
            for _ in 0..count {
                let len = read_u16(bytes, pos).context("truncated relay URL length")? as usize;
                if len > MAX_RELAY_URL_BYTES {
                    bail!("relay URL too long: {len}");
                }
                let end = pos.checked_add(len).context("relay URL length overflow")?;
                let raw = bytes.get(*pos..end).context("truncated relay URL")?;
                *pos = end;
                let text = std::str::from_utf8(raw).context("relay URL is not UTF-8")?;
                ladder.push(text.parse::<RelayUrl>().context("invalid relay URL")?);
            }
            RelayChoice::Custom(ladder)
        };
        Ok(LookupOpts { mdns, dht, relay })
    }
}

/// Read a little-endian `u16` at `pos`, advancing it.
///
/// # Errors
/// Fewer than two bytes remain.
pub fn read_u16(bytes: &[u8], pos: &mut usize) -> Result<u16> {
    let end = pos.checked_add(2).context("u16 length overflow")?;
    let slice = bytes.get(*pos..end).context("truncated u16")?;
    *pos = end;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

#[cfg(test)]
mod tests {
    use super::{LookupOpts, RelayChoice};

    fn round_trip(opts: &LookupOpts) -> LookupOpts {
        let mut buf = Vec::new();
        opts.encode_into(&mut buf);
        let mut pos = 0;
        let decoded = LookupOpts::decode_from(&buf, &mut pos).expect("decode");
        assert_eq!(pos, buf.len(), "the region is self-delimiting");
        decoded
    }

    #[test]
    fn presets_round_trip() {
        for opts in [LookupOpts::loopback(), LookupOpts::public_preset()] {
            assert_eq!(round_trip(&opts), opts);
        }
    }

    #[test]
    fn custom_ladder_preserves_order() {
        let opts = LookupOpts {
            mdns: false,
            dht: false,
            relay: RelayChoice::Custom(vec![
                "https://a.example".parse().expect("url"),
                "https://b.example".parse().expect("url"),
            ]),
        };
        assert_eq!(round_trip(&opts), opts);
    }

    #[test]
    fn flag_bytes_are_pinned_wire_format() {
        // Interop tripwire: these bits are shared with agent-habilis/swarm.
        let mut loopback = Vec::new();
        LookupOpts::loopback().encode_into(&mut loopback);
        assert_eq!(loopback, [0b0000]);

        let mut public = Vec::new();
        LookupOpts::public_preset().encode_into(&mut public);
        assert_eq!(public, [0b0111]);
    }

    #[test]
    fn custom_bit_without_enabled_bit_is_rejected() {
        let mut pos = 0;
        assert!(LookupOpts::decode_from(&[0b1000], &mut pos).is_err());
    }

    #[test]
    fn empty_and_overlong_ladders_are_rejected() {
        let mut empty_pos = 0;
        assert!(
            LookupOpts::decode_from(&[0b1100, 0], &mut empty_pos).is_err(),
            "a zero-length custom ladder is meaningless"
        );
        let mut overlong_pos = 0;
        assert!(
            LookupOpts::decode_from(&[0b1100, 17], &mut overlong_pos).is_err(),
            "over MAX_RELAY_LADDER"
        );
    }
}
