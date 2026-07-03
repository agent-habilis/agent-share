//! Small cross-cutting helpers.

#[expect(
    dead_code,
    reason = "output.rs is shared verbatim; each consumer uses a subset"
)]
pub(crate) mod output;
