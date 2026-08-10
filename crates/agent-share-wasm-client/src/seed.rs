//! The browser's seeder: [`agent_share_mount::Seeder`] over `IdbStore`.
//!
//! This file used to hold the implementation. It now holds only the choice of
//! store, because the CLI needed exactly the same logic over `FsStore` and two
//! copies of "what may this peer serve, and to whom" is not a thing to keep.
//! The rules that used to be documented here — verbatim envelopes, partial
//! holdings served rather than withheld, and answers scoped to this share —
//! live with the implementation.

use fofoca_chunks::IdbStore;

/// What a seeding tab serves from.
pub(crate) type SeederShared = agent_share_mount::Seeder<IdbStore>;
