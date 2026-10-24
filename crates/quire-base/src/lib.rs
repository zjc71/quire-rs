//! Shared primitives for the Quire document engine.
//!
//! This crate deliberately has no knowledge of documents, formats, layout,
//! rendering or the operating system. Every other crate may depend on it.

#![forbid(unsafe_code)]

pub mod error;
pub mod geom;
pub mod ids;
pub mod units;
pub mod xml;

pub use error::{QuireError, Result};
pub use geom::Rect;
pub use ids::{DocId, ImageId, ParaId, Revision, RunId, TableId};
pub use units::{Emu, Pt, Twip};
