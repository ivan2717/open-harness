//! Provider wire codecs and replay validation outside the execution core.
//!
//! These helpers are building blocks for transport adapters. They preserve raw
//! payloads, while exposing portable, ordered content to the harness. Provider
//! compatibility rules and transport clients belong above these codecs.

mod replay;
mod usage;
mod wire;

pub use replay::{validate_legacy_response, validate_replay_state};
pub use usage::UsageDecodeError;
pub use wire::WireProtocol;
