//! The `mercury` command line: sign in with a personal token, see what you may ask, and ask.
//!
//! The transport (`client`, `sse`) and the rendering (`render`) meet in one event loop (`ask`),
//! so the live timeline and `--json` are made from the same events, and nothing of an answer is
//! shown before its `answer` event. The token comes from `MERCURY_TOKEN` or the OS keychain, and
//! no output, error or `Debug` shows it.

pub mod ask;
pub mod cli;
pub mod client;
pub mod credentials;
pub mod error;
pub mod render;
pub mod run;
pub mod sse;
