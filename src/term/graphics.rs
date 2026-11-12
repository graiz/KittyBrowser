//! Kitty graphics protocol: send frames to the terminal and place them.
//!
//! Planned: detect support with a query, pick the fastest transmission the
//! terminal accepts (shared memory, temp file, then base64 chunks), and
//! replace an image in place by id so redraws don't flicker.
