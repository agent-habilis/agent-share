//! The path-traversal guard, vendored from agent-habilis/swarm's
//! `src/file/walk.rs` — trimmed to [`safe_component`], the single control that
//! keeps a hostile producer's manifest from naming paths outside the mount.

use anyhow::{Result, bail};

/// Validate a single path component received from the peer.
///
/// # Errors
/// Empty, `.`/`..`, NUL, or containing a path separator. `std::path::is_separator`
/// is platform-aware, so `\` is rejected on Windows (where it separates paths)
/// without over-rejecting it on unix (where it is a legal filename byte).
pub(crate) fn safe_component(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('\0')
        || name.chars().any(std::path::is_separator)
    {
        bail!("unsafe path component from peer: {name:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::safe_component;

    #[test]
    fn safe_component_rejects_traversal_and_separators() {
        assert!(safe_component("..").is_err());
        assert!(safe_component(".").is_err());
        assert!(safe_component("a/b").is_err());
        assert!(safe_component("").is_err());
        assert!(safe_component("normal-name.txt").is_ok());
    }
}
