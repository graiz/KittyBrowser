//! Pane size in cells and pixels, and resize notifications.
//!
//! Note from the prototype: a new Rex pane reports 80x24 for a moment before
//! it is laid out, so never trust the first reading. Re-read on SIGWINCH.
