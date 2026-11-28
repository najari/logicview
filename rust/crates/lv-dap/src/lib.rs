//! Infineon AURIX debugging over the 2-pin DAP port: telegrams, the link to
//! the device's IO client (memory access, reset) and the wires that carry it.
//!
//! - [`telegram`]: what goes over DAP1, bit by bit.
//! - [`wire`]: the probe that drives DAP0, DAP1 and the reset line.
//! - [`link`]: connecting, memory access and reset on top of a wire.
//! - [`cmsis`]: a CMSIS-DAP v2 probe as the wires.
//! - [`sim`]: a pretend device, for tests and for trying the tools.
//! - [`mwtrace`]: reads USB captures of the Infineon tools, the reference
//!   the rest is checked against.

pub mod bits;
pub mod cmsis;
pub mod link;
pub mod mwtrace;
pub mod sim;
pub mod telegram;
pub mod wire;

pub use bits::Bits;
pub use link::{Link, LinkError, Reset, Target};
pub use wire::{Op, Wire, WireError};
