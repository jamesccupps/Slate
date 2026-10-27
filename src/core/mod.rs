//! The editor engine, independent of the UI: sources, the piece table, documents, files, search, line tools, JSON,
//! XML and CSV tools.

pub mod buffer;
pub mod csv;
pub mod document;
pub mod io;
pub mod job;
pub mod json;
pub mod jsonnav;
pub mod lines;
pub mod os;
pub mod search;
pub mod source;
pub mod text;
pub mod xml;
pub mod xmlnav;
