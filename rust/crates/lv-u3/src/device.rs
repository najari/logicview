//! Finding, opening and streaming from the board.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use lv_fx2lafw::{StreamConfig, StreamEnd, StreamStats};
use nusb::MaybeFuture;

use crate::Error;
use crate::link::{Link, usb::Usb};
use crate::protocol::*;

const REPLY_TIMEOUT: Duration = Duration::from_millis(500);

/// How the board is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The WCH CH375 driver (`CH375DLL64.DLL`), the Windows default.
    Ch375,
    /// WinUSB on Windows, libusb elsewhere.
    Usb,
}

/// A board seen on the bus.
#[derive(Debug, Clone)]
pub struct FoundU3 {
    pub info: nusb::DeviceInfo,
    pub serial: String,
    /// `usb/<bus>-<port>`, as for the other devices.
    pub location: String,
    pub via: Via,
}

impl FoundU3 {
    pub fn name(&self) -> String {
        "WeAct Logic Analyzer V2 (U3)".into()
    }
}

/// The serial number: from the descriptor, or on Windows the device
/// instance id, which is the serial number for this board.
fn serial_of(info: &nusb::DeviceInfo) -> String {
    if let Some(s) = info.serial_number().filter(|s| !s.is_empty()) {
        return s.to_string();
    }
    #[cfg(windows)]
    {
        let id = info.instance_id().to_string_lossy();
        if let Some(s) = id.rsplit('\\').next() {
            return s.to_string();
        }
    }
    String::new()
}

fn via_of(info: &nusb::DeviceInfo) -> Via {
    #[cfg(windows)]
    {
        if !info
            .driver()
            .is_some_and(|d| d.eq_ignore_ascii_case("winusb"))
        {
            return Via::Ch375;
        }
    }
    let _ = info;
    Via::Usb
}

/// Boards on the bus. The IAP (update) mode has another product id and is
/// not listed.
pub fn list_devices() -> Result<Vec<FoundU3>, Error> {
    let buses = lv_fx2lafw::bus_ids();
    let mut out = Vec::new();
    for info in nusb::list_devices().wait()? {
        if info.vendor_id() != VID || info.product_id() != PID {
            continue;
        }
        #[cfg(windows)]
        log::debug!(
            "U3 {} driver {:?}",
            info.instance_id().to_string_lossy(),
            info.driver()
        );
        out.push(FoundU3 {
            serial: serial_of(&info),
            location: lv_fx2lafw::location_of(&info, &buses),
            via: via_of(&info),
            info,
        });
    }
    Ok(out)
}

/// An opened board.
pub struct U3 {
    link: Box<dyn Link>,
    pub location: String,
    pub name: String,
    pub serial: String,
    pub via: Via,
    /// Firmware (`sw`) and IAP version bytes of the `0xAC` reply.
    pub fw_version: (u8, u8),
    /// bcdUSB of the link: 0x0300 for USB 3.0.
    pub bcd_usb: u16,
    /// Channels sampled: 8 or 16 (see [`U3::set_channels`]).
    pub channels: u8,
    /// A press of the board key ends the capture (firmware 0.2).
    pub key_stop: bool,
}

impl std::fmt::Debug for U3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("U3")
            .field("location", &self.location)
            .field("via", &self.via)
            .field("fw_version", &self.fw_version)
            .field("bcd_usb", &format_args!("{:04x}", self.bcd_usb))
            .field("channels", &self.channels)
            .finish()
    }
}

/// Open the board and ask for its firmware version.
pub fn open(found: &FoundU3) -> Result<U3, Error> {
    let link: Box<dyn Link> = match found.via {
        #[cfg(windows)]
        Via::Ch375 => Box::new(crate::link::ch375::Ch375::open(&found.serial)?),
        #[cfg(not(windows))]
        Via::Ch375 => unreachable!("CH375 is a Windows driver"),
        Via::Usb => Box::new(Usb::open(&found.info)?),
    };
    let bcd_usb = link.bcd_usb();
    let mut dev = U3 {
        link,
        location: found.location.clone(),
        name: found.name(),
        serial: found.serial.clone(),
        via: found.via,
        fw_version: (0, 0),
        bcd_usb,
        channels: 16,
        key_stop: false,
    };
    dev.link.flush_replies();
    let v = dev.command(CMD_GET_VERSION, &[])?;
    dev.fw_version = (
        v.first().copied().unwrap_or(0),
        v.get(1).copied().unwrap_or(0),
    );
    Ok(dev)
}

/// A handle kept from an earlier capture, if it still reaches `found`.
pub fn reuse(cached: Option<U3>, found: &FoundU3) -> Option<U3> {
    let mut d = cached?;
    if d.location != found.location || d.serial != found.serial {
        return None;
    }
    if d.is_alive() {
        Some(d)
    } else {
        log::info!("{}: kept handle is stale; opening again", d.location);
        None
    }
}

impl U3 {
    /// Sample rates of the link the board is on with the current channel
    /// mode, fastest first.
    pub fn samplerates(&self) -> &'static [u64] {
        rates_for(self.bcd_usb, self.channels)
    }

    /// Sample `n` channels: up to 8 takes the 8-channel mode (one byte per
    /// sample), up to 16 the 16-channel mode (two bytes).
    pub fn set_channels(&mut self, n: usize) -> Result<(), Error> {
        self.channels = channel_mode(n).ok_or(Error::Channels(n))?;
        Ok(())
    }

    /// Bytes per sample in the current mode.
    pub fn unitsize(&self) -> usize {
        if self.channels > 8 { 2 } else { 1 }
    }

    pub fn is_usb3(&self) -> bool {
        self.bcd_usb >= 0x0300
    }

    /// Firmware 0.2 and later have the board key and the indicator level.
    pub fn has_key_and_led(&self) -> bool {
        self.fw_version.0 >= 2
    }

    /// How often the board key was pressed (a wrapping counter), if the
    /// firmware reports it.
    pub fn key_count(&mut self) -> Result<Option<u8>, Error> {
        let d = self.command(CMD_GET_LOGIC_PARAMS, &[])?;
        Ok(d.get(2).copied())
    }

    /// Set the indicator brightness (one of [`LED_LEVELS`], percent).
    pub fn set_led_brightness(&mut self, level: u8) -> Result<(), Error> {
        if !self.has_key_and_led() {
            return Err(Error::Protocol(format!(
                "firmware 0.{} has no indicator brightness (0.2 or later)",
                self.fw_version.0
            )));
        }
        self.link
            .write(&command(CMD_SET_LED_BRIGHT, &[level.min(100)]))?;
        let r = self.link.read_reply(REPLY_TIMEOUT)?;
        // Firmware 0.2 answers 0xFA instead of the 0xBF the vendor driver
        // expects (seen 2026-11-25); the status byte is what counts.
        let mut r = r;
        if r.first() == Some(&0xfa) {
            r[0] = CMD_SET_LED_BRIGHT | 0x10;
        }
        reply_data(CMD_SET_LED_BRIGHT, &r)?;
        Ok(())
    }

    /// Restart the board in its update (IAP) mode; it comes back as the HID
    /// device `1a86:fe17`. The board does not answer.
    pub fn enter_iap(&mut self) -> Result<(), Error> {
        self.send(CMD_ENTER_IAP)
    }

    fn command(&mut self, cmd: u8, data: &[u8]) -> Result<Vec<u8>, Error> {
        self.link.write(&command(cmd, data))?;
        let r = self.link.read_reply(REPLY_TIMEOUT)?;
        reply_data(cmd, &r)
    }

    /// Commands the firmware does not answer.
    fn send(&mut self, cmd: u8) -> Result<(), Error> {
        self.link.write(&command(cmd, &[]))
    }

    pub fn is_alive(&mut self) -> bool {
        self.link.flush_replies();
        self.command(CMD_GET_VERSION, &[]).is_ok()
    }

    /// Set the input threshold in volts on the pins (0.6..1.5 V; 0.6 V is a
    /// 1.0 V logic level, 1.5 V a 3.3 V one).
    pub fn set_threshold(&mut self, volts: f64) -> Result<(), Error> {
        let code = threshold_code(volts)?;
        self.command(CMD_SET_LOGIC_LEVEL, &code.to_be_bytes())?;
        Ok(())
    }

    /// The threshold the board is set to.
    pub fn threshold(&mut self) -> Result<Option<f64>, Error> {
        let d = self.command(CMD_GET_LOGIC_LEVEL, &[])?;
        Ok(match d[..] {
            [hi, lo, ..] => threshold_of_code(u16::from_be_bytes([hi, lo])),
            _ => None,
        })
    }

    /// Stream samples into `sink`, like [`lv_fx2lafw::Fx2lafw::stream`]:
    /// one byte per sample in 8-channel mode, two (little-endian) in
    /// 16-channel mode, bit n = D<n>. The sink only gets whole samples, and
    /// `limit_samples` counts samples.
    pub fn stream(
        &mut self,
        cfg: &StreamConfig,
        stop: &AtomicBool,
        mut sink: impl FnMut(&[u8]) -> bool,
    ) -> Result<StreamStats, Error> {
        let div = divider(self.samplerates(), cfg.samplerate)?;
        let unit = self.unitsize();
        let width = if unit == 2 {
            SAMPLE_WIDTH_16BIT
        } else {
            SAMPLE_WIDTH_8BIT
        };
        let block = cfg
            .transfer_size
            .unwrap_or_else(|| transfer_size(cfg.samplerate, unit));
        self.link.flush_replies();
        // Make sure the board is idle; it does not answer a stop.
        self.send(CMD_STOP_LOGIC)?;
        let key_stop = self.key_stop && self.has_key_and_led();
        // The count before the start tells a key press from a stalled link.
        let key_before = if key_stop {
            self.key_count().ok().flatten()
        } else {
            None
        };
        self.command(CMD_SET_LOGIC_PARAMS, &[width, div, u8::from(key_stop)])?;
        self.link.start_upload(block)?;
        if let Err(e) = self.send(CMD_START_LOGIC) {
            self.link.stop_upload();
            return Err(e);
        }

        let started = Instant::now();
        // The board stops sending when the link cannot keep up.
        let block_time =
            Duration::from_secs_f64(block as f64 / unit as f64 / cfg.samplerate.max(1) as f64);
        let idle_limit = (block_time * 4).max(Duration::from_millis(1000));
        let mut last_data = started;
        let mut stats = StreamStats {
            end: StreamEnd::Stopped,
            samples: 0,
            transfers: 0,
            empty_transfers: 0,
            transfer_size: block,
            transfer_count: 1,
            elapsed: Duration::ZERO,
            first_data: None,
        };
        let mut failure = None;
        // A sample split between two reads, and room to join it.
        let mut carry: Option<u8> = None;
        let mut joined: Vec<u8> = Vec::new();
        let end = loop {
            if stop.load(Ordering::Relaxed) {
                break StreamEnd::Stopped;
            }
            let data = match self.link.next_data(Duration::from_millis(20)) {
                Ok(Some(d)) => d,
                Ok(None) => {
                    if last_data.elapsed() > idle_limit {
                        break StreamEnd::DeviceStopped;
                    }
                    continue;
                }
                Err(Error::Disconnected(_)) => break StreamEnd::Disconnected,
                Err(e) => {
                    failure = Some(e);
                    break StreamEnd::DeviceStopped;
                }
            };
            stats.transfers += 1;
            if data.is_empty() {
                stats.empty_transfers += 1;
                continue;
            }
            last_data = Instant::now();
            stats.first_data.get_or_insert_with(|| started.elapsed());
            let data = if unit == 2 && (carry.is_some() || data.len() % 2 == 1) {
                joined.clear();
                joined.extend(carry.take());
                joined.extend_from_slice(data);
                if joined.len() % 2 == 1 {
                    carry = joined.pop();
                }
                &joined[..]
            } else {
                data
            };
            let n = (data.len() / unit) as u64;
            let take = match cfg.limit_samples {
                Some(limit) => (limit - stats.samples).min(n),
                None => n,
            };
            if !sink(&data[..take as usize * unit]) {
                break StreamEnd::SinkFull;
            }
            stats.samples += take;
            if cfg.limit_samples.is_some_and(|l| stats.samples >= l) {
                break StreamEnd::LimitReached;
            }
        };
        stats.elapsed = started.elapsed();
        let _ = self.send(CMD_STOP_LOGIC);
        self.link.stop_upload();
        let mut end = end;
        if end == StreamEnd::DeviceStopped
            && key_before.is_some()
            && self.key_count().ok().flatten() != key_before
        {
            log::info!("{}: stopped by the board key", self.location);
            end = StreamEnd::Stopped;
            failure = None;
        }
        if end == StreamEnd::DeviceStopped && !self.is_alive() {
            log::info!(
                "{}: no data, and the board no longer answers",
                self.location
            );
            end = StreamEnd::Disconnected;
            failure = None;
        }
        if let Some(e) = failure {
            return Err(e);
        }
        stats.end = end;
        Ok(stats)
    }
}
