//! Embedded static assets: the binary serves itself, no files on disk.

pub const APP_CSS: &str = include_str!("assets/app.css");
pub const APP_JS: &str = include_str!("assets/app.js");
