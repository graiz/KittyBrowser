//! The page engine: turns a URL into frames and takes input back.
//!
//! There is one backend, headless Chrome driven over the DevTools protocol.
//! `cdp` is the wire transport; `chrome` starts the browser and exposes the
//! handful of commands and events the app needs.

pub mod cdp;
pub mod chrome;
