//! Slate: a fast, simple text editor that handles huge files. The engine (`core`) and the syntax coloring
//! (`highlight`) are shared; each platform has its own window: `ui` on Windows (Win32 and Direct2D), `gtk` on Linux.

pub mod core;
pub mod edit;
pub mod highlight;
pub mod settings;
pub mod theme;
#[cfg(target_os = "linux")]
pub mod gtk;
#[cfg(windows)]
pub mod ui;
