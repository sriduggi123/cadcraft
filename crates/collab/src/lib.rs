//! Real-time collaboration core for CADCraft: what one command changed in a drawing, as a batch
//! of [`Change`]s, and how another copy of the drawing takes them. Pure library: no networking, no
//! UI. The design is the fork's `plan/multiplayer-design.md` (decisions D2 to D13), after
//! FigCraft's `docs/collab-protocol.md`.
//!
//! - [`diff`] turns a command's edit (the drawing before and after) into a batch: an entity's
//!   `Common` fields are one property each and its geometry is one property, tables and other
//!   drawing-level data are written whole, and drawing variables one by one.
//! - [`apply`] applies a batch to another copy of the drawing.
//! - [`vars`] says which drawing variables belong to each person (current layer, snaps…) and are
//!   never synced.
//! - [`space`] gives each client its own range of handles, so two people never make the same one.
//! - [`snapshot`] encodes a whole drawing for the server: a postcard envelope with the drawing as
//!   JSON inside it, so a snapshot keeps loading after entity types gain fields.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

mod apply;
mod change;
mod container;
mod diff;
mod docfields;
mod json;
pub mod snapshot;
pub mod space;
pub mod vars;

pub use apply::apply;
pub use cadcraft_doc::{Drawing, Handle};
pub use change::Change;
pub use container::Container;
pub use diff::diff;
pub use json::Json;

/// What can go wrong syncing.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum Error {
    /// A change names an entity that doesn't exist (or no longer does).
    #[error("no such object {0:?}")]
    Unknown(Handle),
    /// A change or message is not valid.
    #[error("{0}")]
    Invalid(String),
    #[error("can't encode: {0}")]
    Encode(String),
    #[error("can't decode: {0}")]
    Decode(String),
    /// Input larger than this many bytes.
    #[error("the input is over the {0}-byte limit")]
    TooLarge(usize),
}

pub type Result<T> = std::result::Result<T, Error>;
