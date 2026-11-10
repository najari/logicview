//! Driver for FX2-based logic analyzers running the open fx2lafw firmware.
//!
//! Talks WinUSB directly through `nusb`; on Windows nusb enables RAW_IO on
//! bulk IN endpoints, so many transfers stay queued at once.

mod device;
pub mod protocol;
mod stream;

use std::path::PathBuf;

pub use device::{
    FoundDevice, Fx2lafw, PROFILES, Profile, default_firmware_dirs, find_firmware, list_devices,
    open, reuse,
};
pub use protocol::SAMPLERATES;
pub use stream::{StreamConfig, StreamEnd, StreamStats};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("USB error: {0}")]
    Usb(#[from] nusb::Error),
    #[error("USB transfer failed: {0}")]
    Transfer(#[from] nusb::transfer::TransferError),
    #[error("{0} Hz cannot be sampled; choose one of the listed sample rates")]
    UnsupportedSamplerate(u64),
    #[error("firmware {name} not found; looked in: {}", .searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))]
    FirmwareNotFound {
        name: String,
        searched: Vec<PathBuf>,
    },
    #[error("cannot read firmware {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("firmware file {0} is empty or larger than 64 KiB")]
    BadFirmware(PathBuf),
    #[error("device runs fx2lafw {major}.{minor}, but version 1.x is required")]
    FirmwareVersion { major: u8, minor: u8 },
    #[error("device at {location} did not come back after the firmware upload{}", .last.as_ref().map(|e| format!(" (last error: {e})")).unwrap_or_default())]
    Renumerate {
        location: String,
        last: Option<String>,
    },
    #[error("unexpected reply from device: {0}")]
    Protocol(String),
}
