//! Minimal subprocess-test harness, trimmed from agent-habilis/swarm's
//! `tests/common` (the daemon/gossip machinery is not needed here).

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Generous ceiling for a subprocess to print its expected line; tests poll
/// and pass as soon as the line arrives.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_mins(1);

/// Poll interval while waiting on subprocess output.
pub(crate) const POLL: Duration = Duration::from_millis(250);

/// A `🐝…` swarm id with loopback lookups (no mDNS/DHT/relay), minted once
/// via agent-habilis/swarm as
/// `Swarm::new([7u8; 32], SwarmName::new("test")?, SwarmConfig::loopback())
/// .to_string()` — the encoding is deterministic, so this const doubles as a
/// cross-repo wire-compat pin (`swarm_id_wire_format_is_pinned` in
/// `src/protocol/swarm/mod.rs` re-derives it).
pub(crate) const LOOPBACK_SWARM_ID: &str =
    "🐝2UXAThUkdBAbiJNXvCt4YeMGQ9myFg7gJJZSr3pG3MAGzUwWmmV7D2Msw3sco";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ahmo"))
}

pub(crate) fn test_cmd() -> Command {
    Command::new(bin())
}
