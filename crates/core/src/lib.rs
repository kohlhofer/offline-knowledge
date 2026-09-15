//! The offline knowledge core: import a ZIM file once, then suggest titles,
//! search full text and read articles as structured documents.
//!
//! Every frontend (terminal, HTTP, MCP) is a thin adapter over [`Library`].

pub mod document;
mod error;
mod fulltext;
pub mod html;
pub mod import;
mod library;
pub mod normalize;
mod stubs;
pub mod text;
mod titles;

pub use document::{Document, Target};
pub use error::{Error, Result};
pub use library::{Library, Resolution, SearchResult, Suggestion};
