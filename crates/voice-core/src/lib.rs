//! Pure, I/O-free logic shared by the CLI, the GPUI app and the future MCP server.
//! No threads, no clocks, no I/O: time is always passed in as milliseconds.

pub mod cosine;
pub mod fbank;
pub mod media_gate;
pub mod thresholds;
