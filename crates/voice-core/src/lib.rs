//! Pure, I/O-free logic shared by the CLI, the GPUI app and the future MCP server.
//! No threads, no clocks, no I/O: time is always passed in as milliseconds.

pub mod call_machine;
pub mod cosine;
pub mod echo_filter;
pub mod fbank;
pub mod media_gate;
pub mod segmenter;
pub mod speaker_profile;
pub mod thresholds;
pub mod turn_heuristics;
