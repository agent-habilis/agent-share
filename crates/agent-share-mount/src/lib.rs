//! The mount protocol's behaviour, in one copy.
//!
//! [`agent_share_proto`] owns this protocol's *bytes* — the ops, the framing,
//! the manifest and ticket codecs. This crate owns what a peer *does* with
//! them: answer a request, fetch a chunk, decide which peer to ask.
//!
//! # Why it exists
//!
//! Because it was written twice. The CLI and the browser each had their own
//! `serve_stream` matching the same ops over the same
//! `iroh::endpoint::{SendStream, RecvStream}`, their own chunk fetch helpers
//! over the same wire, and the browser alone had the seeder and the scheduler.
//! Adding a native seeder by porting the browser's would have made three
//! copies of the first and two of the rest.
//!
//! Two copies of a protocol server do not stay equal. They drift in the order
//! they check things, in which errors close a stream and which close a
//! connection, and in what an unknown op does — and the drift shows up as one
//! platform failing to talk to the other, which is the hardest kind of bug to
//! see from either side.
//!
//! # The seam
//!
//! A peer is whatever can answer [`ServeSource`]. There are three of them and
//! they have nothing in common but the trait: the CLI's producer reads through
//! to files on disk, the browser's producer reads through to whatever the
//! browser gave it for a picked file, and a seeder of either kind reads from a
//! chunk store. One dispatch loop drives all three.
//!
//! Futures here are `?Send`, for the reason [`fofoca_chunks`] gives: the same
//! code runs under tokio and in a browser, where the state is `Rc`-flavoured.
//! Callers that need `Send` get it from their own source, so the CLI can
//! `tokio::spawn` the very loop the browser drives with `spawn_local`.
//!
//! # What does not live here
//!
//! Discovery, the mesh, tickets and passwords; the NFS bridge; the filesystem
//! watcher; anything that touches `web_sys` or `tokio`. This crate is handed a
//! stream that is already open and a source that already knows what it holds.

pub use self::seed::{Feed, Seeder, WeakSeeder};
pub use self::serve::{ServeSource, Watcher, serve_stream};

mod seed;
mod serve;
