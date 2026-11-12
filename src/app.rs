//! The event loop: owns the terminal, the engine, and the UI state.
//!
//! Loop: wait for input, a resize, or a new frame from the engine; update
//! state; draw only what changed.

pub fn run(_url: &str) -> Result<(), String> {
    Err("not implemented yet".into())
}
