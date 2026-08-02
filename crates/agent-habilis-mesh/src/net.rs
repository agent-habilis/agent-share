//! The iroh-exposing corner: endpoint construction and reachability probes.
//!
//! Deliberately quarantined. Every other public module is free of `iroh` types
//! so a consumer's own surface can be; a diagnostics command that genuinely
//! needs an `Endpoint` reaches in here and accepts the coupling.

pub use crate::gossip::conn_path;
#[cfg(feature = "host")]
pub use crate::lookup::{NetworkCapability, capability_probe};
pub use crate::lookup::{
    TransportHandles, TransportOpts, add_peer_addr, build_endpoint, build_peer_endpoint,
    check_injected_identity, probe_connect, probe_ladder, relay_ladder,
};
pub use crate::protocol::peer_addr::{endpoint_addr_from_json, endpoint_addr_to_json};
