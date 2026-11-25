//! Every analyzer logicview can capture from, behind one type: FX2 boards
//! running fx2lafw (`lv-fx2lafw`) and the WeAct Logic Analyzer V2 (`lv-u3`).

use std::sync::atomic::AtomicBool;

use lv_fx2lafw::{FoundDevice, Fx2lafw};
pub use lv_fx2lafw::{
    IDLE_RELEASE, Kept, StreamConfig, StreamEnd, StreamStats, default_firmware_dirs,
};
use lv_u3::{FoundU3, U3};

/// Firmware update of the WeAct U3 (its IAP bootloader).
pub use lv_u3::iap;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Fx2(#[from] lv_fx2lafw::Error),
    #[error(transparent)]
    U3(#[from] lv_u3::Error),
    #[error("{0} has no adjustable input threshold")]
    NoThreshold(String),
    #[error("{0} does not have this")]
    NotSupported(String),
    #[error("{name} samples at most {max} channels ({asked} asked for)")]
    Channels {
        name: String,
        max: usize,
        asked: usize,
    },
}

/// An analyzer seen on the bus.
#[derive(Debug, Clone)]
pub enum Found {
    Fx2(FoundDevice),
    U3(FoundU3),
}

impl Found {
    pub fn name(&self) -> String {
        match self {
            Found::Fx2(f) => f.name(),
            Found::U3(f) => f.name(),
        }
    }

    /// `usb/<bus>-<port>`.
    pub fn location(&self) -> &str {
        match self {
            Found::Fx2(f) => &f.location,
            Found::U3(f) => &f.location,
        }
    }

    /// Driver name, as in a sigrok device id (`fx2lafw:usb/1-8`).
    pub fn driver(&self) -> &'static str {
        match self {
            Found::Fx2(_) => "fx2lafw",
            Found::U3(_) => "weact-u3",
        }
    }

    pub fn info(&self) -> &nusb::DeviceInfo {
        match self {
            Found::Fx2(f) => &f.info,
            Found::U3(f) => &f.info,
        }
    }

    /// Channels the device can sample.
    pub fn max_channels(&self) -> usize {
        match self {
            Found::Fx2(_) => 8,
            Found::U3(_) => 16,
        }
    }

    /// Channel counts a capture can choose (the device samples D0..D(n-1)).
    pub fn channel_choices(&self) -> &'static [usize] {
        match self {
            Found::Fx2(_) => &[8],
            Found::U3(_) => &[2, 4, 8, 16],
        }
    }

    /// The serial number, where the device reports one.
    pub fn serial(&self) -> Option<&str> {
        match self {
            Found::Fx2(_) => None,
            Found::U3(f) => Some(&f.serial),
        }
    }

    /// How the device is reached, for people.
    pub fn transport(&self) -> &'static str {
        match self {
            Found::Fx2(_) => "WinUSB",
            Found::U3(f) => match f.via {
                lv_u3::Via::Ch375 => "CH375",
                lv_u3::Via::Usb => "WinUSB",
            },
        }
    }

    /// bcdUSB of the link (0x0300 for USB 3.0).
    pub fn usb_version(&self) -> u16 {
        self.info().usb_version()
    }

    /// The WeAct U3: input level, board key, indicator, channel modes.
    pub fn is_u3(&self) -> bool {
        matches!(self, Found::U3(_))
    }

    /// Sample rates the device offers with `channels` channels, slowest
    /// first.
    pub fn samplerates(&self, channels: usize) -> Vec<u64> {
        let mut r = match self {
            Found::Fx2(_) => lv_fx2lafw::SAMPLERATES.to_vec(),
            Found::U3(f) => lv_u3::protocol::rates_for(
                f.info.usb_version(),
                lv_u3::protocol::channel_mode(channels).unwrap_or(16),
            )
            .to_vec(),
        };
        r.sort_unstable();
        r
    }

    /// The input threshold range in volts, if it can be set.
    pub fn threshold_range(&self) -> Option<(f64, f64)> {
        match self {
            Found::Fx2(_) => None,
            Found::U3(_) => Some((lv_u3::THRESHOLD_MIN, lv_u3::THRESHOLD_MAX)),
        }
    }

    /// Opening takes a firmware upload first (about 2 s).
    pub fn needs_firmware(&self) -> bool {
        matches!(self, Found::Fx2(f) if !f.has_firmware)
    }

    /// One line about the firmware or driver, for device lists.
    pub fn status(&self) -> String {
        match self {
            Found::Fx2(f) if f.has_firmware => "fx2lafw 올라감".into(),
            Found::Fx2(_) => "없음 (열 때 올림)".into(),
            Found::U3(f) => match f.via {
                lv_u3::Via::Ch375 => "CH375 드라이버".into(),
                lv_u3::Via::Usb => "WinUSB/libusb".into(),
            },
        }
    }
}

/// Supported analyzers on the bus, FX2 boards first.
pub fn list_devices() -> Result<Vec<Found>, Error> {
    let mut out: Vec<Found> = lv_fx2lafw::list_devices()?
        .into_iter()
        .map(Found::Fx2)
        .collect();
    out.extend(lv_u3::list_devices()?.into_iter().map(Found::U3));
    Ok(out)
}

/// Signal levels for [`threshold_for_level`], in volts.
pub const LEVELS: &[f64] = &[3.3, 3.0, 2.5, 1.8, 1.5, 1.2, 1.0];

/// The input threshold for signals of `level` volts (WeAct U3).
pub fn threshold_for_level(level: f64) -> f64 {
    lv_u3::protocol::threshold_for_level(level)
}

/// Every sample rate some device offers, slowest first.
pub fn all_samplerates() -> Vec<u64> {
    let mut r: Vec<u64> = lv_fx2lafw::SAMPLERATES
        .iter()
        .chain(lv_u3::ALL_RATES)
        .copied()
        .collect();
    r.sort_unstable();
    r.dedup();
    r
}

/// An opened analyzer.
#[derive(Debug)]
pub enum Device {
    Fx2(Fx2lafw),
    U3(U3),
}

/// The device handle kept between captures (see [`Kept`]).
pub type DeviceSlot = Kept<Device>;

/// Open a device (an FX2 board gets its firmware first if needed).
pub fn open(found: &Found) -> Result<Device, Error> {
    Ok(match found {
        Found::Fx2(f) => Device::Fx2(lv_fx2lafw::open(f, &lv_fx2lafw::default_firmware_dirs())?),
        Found::U3(f) => Device::U3(lv_u3::open(f)?),
    })
}

/// A handle kept from an earlier capture, if it still fits `found`.
pub fn reuse(cached: Option<Device>, found: &Found) -> Option<Device> {
    match (cached?, found) {
        (Device::Fx2(d), Found::Fx2(f)) => lv_fx2lafw::reuse(Some(d), f).map(Device::Fx2),
        (Device::U3(d), Found::U3(f)) => lv_u3::reuse(Some(d), f).map(Device::U3),
        _ => None,
    }
}

impl Device {
    pub fn name(&self) -> &str {
        match self {
            Device::Fx2(d) => &d.name,
            Device::U3(d) => &d.name,
        }
    }

    pub fn location(&self) -> &str {
        match self {
            Device::Fx2(d) => &d.location,
            Device::U3(d) => &d.location,
        }
    }

    /// Firmware and link, for people.
    pub fn describe(&self) -> String {
        match self {
            Device::Fx2(d) => format!(
                "fx2lafw {}.{}, REVID {}",
                d.fw_version.0, d.fw_version.1, d.revid
            ),
            Device::U3(d) => format!(
                "펌웨어 0.{}, USB {}, {}",
                d.fw_version.0,
                if d.is_usb3() { "3.0" } else { "2.0" },
                match d.via {
                    lv_u3::Via::Ch375 => "CH375 드라이버",
                    lv_u3::Via::Usb => "WinUSB/libusb",
                }
            ),
        }
    }

    /// Channels the device can sample.
    pub fn max_channels(&self) -> usize {
        match self {
            Device::Fx2(_) => 8,
            Device::U3(_) => 16,
        }
    }

    /// Sample `n` channels (D0..D(n-1)) from now on.
    pub fn set_channels(&mut self, n: usize) -> Result<(), Error> {
        if n == 0 || n > self.max_channels() {
            return Err(Error::Channels {
                name: self.name().to_string(),
                max: self.max_channels(),
                asked: n,
            });
        }
        match self {
            Device::Fx2(_) => Ok(()),
            Device::U3(d) => Ok(d.set_channels(n)?),
        }
    }

    /// Sample rates of this device with its current channel count,
    /// slowest first.
    pub fn samplerates(&self) -> Vec<u64> {
        let mut r = match self {
            Device::Fx2(_) => lv_fx2lafw::SAMPLERATES.to_vec(),
            Device::U3(d) => d.samplerates().to_vec(),
        };
        r.sort_unstable();
        r
    }

    /// An error unless the device can sample at `rate`.
    pub fn check_rate(&self, rate: u64) -> Result<(), Error> {
        match self {
            Device::Fx2(_) => lv_fx2lafw::protocol::start_command(rate)
                .map(|_| ())
                .map_err(Into::into),
            Device::U3(d) => lv_u3::protocol::divider(d.samplerates(), rate)
                .map(|_| ())
                .map_err(Into::into),
        }
    }

    /// Bytes in one block of samples handed to the sink.
    pub fn transfer_size(&self, rate: u64) -> usize {
        match self {
            Device::Fx2(_) => lv_fx2lafw::protocol::transfer_size(rate),
            Device::U3(d) => lv_u3::protocol::transfer_size(rate, d.unitsize()),
        }
    }

    fn not_supported(&self) -> Error {
        Error::NotSupported(self.name().to_string())
    }

    /// A press of the board key ends the capture (WeAct U3, firmware 0.2).
    pub fn set_key_stop(&mut self, on: bool) {
        if let Device::U3(d) = self {
            d.key_stop = on;
        }
    }

    /// The board key press counter, if the device has a key.
    pub fn key_count(&mut self) -> Result<Option<u8>, Error> {
        match self {
            Device::U3(d) if d.has_key_and_led() => Ok(d.key_count()?),
            _ => Ok(None),
        }
    }

    /// Set the indicator brightness (percent, see `lv_u3::protocol::LED_LEVELS`).
    pub fn set_led_brightness(&mut self, level: u8) -> Result<(), Error> {
        match self {
            Device::U3(d) => Ok(d.set_led_brightness(level)?),
            _ => Err(self.not_supported()),
        }
    }

    /// Restart the device in its firmware update mode.
    pub fn enter_update_mode(&mut self) -> Result<(), Error> {
        match self {
            Device::U3(d) => Ok(d.enter_iap()?),
            _ => Err(self.not_supported()),
        }
    }

    pub fn is_alive(&mut self) -> bool {
        match self {
            Device::Fx2(d) => d.is_alive(),
            Device::U3(d) => d.is_alive(),
        }
    }

    /// Set the input threshold (volts on the pins).
    pub fn set_threshold(&mut self, volts: f64) -> Result<(), Error> {
        match self {
            Device::Fx2(d) => Err(Error::NoThreshold(d.name.clone())),
            Device::U3(d) => Ok(d.set_threshold(volts)?),
        }
    }

    /// Stream samples (one byte each up to 8 channels, two little-endian
    /// bytes for more; bit n = D<n>) into `sink`; see
    /// [`lv_fx2lafw::Fx2lafw::stream`].
    pub fn stream(
        &mut self,
        cfg: &StreamConfig,
        stop: &AtomicBool,
        sink: impl FnMut(&[u8]) -> bool,
    ) -> Result<StreamStats, Error> {
        match self {
            Device::Fx2(d) => Ok(d.stream(cfg, stop, sink)?),
            Device::U3(d) => Ok(d.stream(cfg, stop, sink)?),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_all_rates_cover_both_devices() {
        let all = all_samplerates();
        assert!(all.windows(2).all(|w| w[0] < w[1]));
        for r in lv_fx2lafw::SAMPLERATES.iter().chain(lv_u3::ALL_RATES) {
            assert!(all.contains(r), "{r}");
        }
        assert_eq!(all.first(), Some(&20_000));
        assert_eq!(all.last(), Some(&200_000_000));
    }
}
