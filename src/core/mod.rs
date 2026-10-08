//! The editor engine, independent of the UI: sources, the piece table, documents, files, search, line tools, JSON
//! and XML tools.

pub mod buffer;
pub mod document;
pub mod io;
pub mod job;
pub mod json;
pub mod jsonnav;
pub mod lines;
pub mod search;
pub mod source;
pub mod text;
pub mod xml;
pub mod xmlnav;
