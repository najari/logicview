//! A TAS server for AURIX debug tools.
//!
//! Infineon's tools and the tools of aurix_debugger (the probe-rs TriCore
//! fork, `mcd_tas`, `m25_tcf`) reach the device through TasServer over TCP.
//! This server speaks the same protocol and reaches the device through an
//! [`lv_dap::Target`]: a probe that logicview drives itself.

pub mod proto;
mod server;

pub use proto::DEFAULT_PORT;
pub use server::{Server, Stats};
