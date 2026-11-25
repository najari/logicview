//! Driver for the WeAct Studio Logic Analyzer V2 ("U3", CH32H417, USB
//! `1a86:5537`): 16 channels, up to 200 MHz over USB 3.0. logicview uses
//! its 8-channel mode (D0..D7, one byte per sample).
//!
//! On Windows the board is bound to the WCH CH375 driver by default and is
//! reached through `CH375DLL64.DLL`; with WinUSB (firmware 0.2 binds it once
//! the CH375 driver is removed), and on Linux and macOS, through nusb.

mod device;
mod link;
pub mod protocol;

pub use device::{FoundU3, U3, Via, list_devices, open, reuse};
pub use protocol::{ALL_RATES, PID, THRESHOLD_MAX, THRESHOLD_MIN, VID};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("USB error: {0}")]
    Usb(#[from] nusb::Error),
    #[error("USB transfer failed: {0}")]
    Transfer(nusb::transfer::TransferError),
    #[error("{0}")]
    Ch375(String),
    #[error("the board did not answer")]
    Timeout,
    #[error("the board went away: {0}")]
    Disconnected(String),
    #[error("{0} Hz cannot be sampled; choose one of the listed sample rates")]
    UnsupportedSamplerate(u64),
    #[error("threshold {0} V is out of range ({THRESHOLD_MIN}..{THRESHOLD_MAX} V)")]
    Threshold(f64),
    #[error("unexpected reply from the board: {0}")]
    Protocol(String),
}
