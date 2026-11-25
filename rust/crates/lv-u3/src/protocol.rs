//! WeAct U3 (CH32H417) wire protocol: commands, sample rate tables and
//! threshold codes.
//!
//! Values follow the `wch-ch32h417-libusb` driver of
//! <https://github.com/WeActStudio/WeActStudio.U3LogicAnalyzer>
//! (`libsigrok/src/hardware/wch-ch32h417-libusb/protocol.{c,h}`, `api.c`).
//! A command is a 64-byte packet `[cmd, len, data...]` on bulk endpoint
//! 0x01; the reply is a 32-byte packet `[cmd | 0x10, len, status, data...]`
//! on 0x81, where `len` counts the status byte and the data.

use crate::Error;

pub const VID: u16 = 0x1a86;
pub const PID: u16 = 0x5537;

/// Command pipe (endpoints 0x01 / 0x81) and logic sample pipe (0x82).
pub const PIPE_CMD: u8 = 1;
pub const PIPE_LOGIC: u8 = 2;

pub const PACKET_OUT: usize = 64;
pub const PACKET_IN: usize = 32;

pub const CMD_SET_LOGIC_PARAMS: u8 = 0xa0;
pub const CMD_SET_LOGIC_LEVEL: u8 = 0xa1;
pub const CMD_START_LOGIC: u8 = 0xa2;
pub const CMD_STOP_LOGIC: u8 = 0xa3;
pub const CMD_GET_LOGIC_PARAMS: u8 = 0xa4;
pub const CMD_GET_LOGIC_LEVEL: u8 = 0xa5;
pub const CMD_GET_VERSION: u8 = 0xac;

/// Bytes per sample on the wire: 8 channels (D0..D7, bit n = channel n).
pub const SAMPLE_WIDTH_8BIT: u8 = 0x08;

/// Logic sample rates over USB 3.0. The index of a rate is the divider
/// index sent to the device, so the order must not change.
pub const RATES_USB3: &[u64] = &[
    200_000_000,
    187_500_000,
    175_000_000,
    162_500_000,
    156_250_000,
    150_000_000,
    137_500_000,
    125_000_000,
    112_500_000,
    100_000_000,
    93_750_000,
    87_500_000,
    81_250_000,
    78_125_000,
    75_000_000,
    68_750_000,
    50_000_000,
    40_000_000,
    30_000_000,
    20_000_000,
    16_000_000,
    12_000_000,
    8_000_000,
    6_000_000,
    4_000_000,
];

/// Logic sample rates over USB 2.0 with 8 channels; the firmware adds its
/// own offset to the index.
pub const RATES_USB2_8CH: &[u64] = &[
    40_000_000, 30_000_000, 20_000_000, 16_000_000, 12_000_000, 8_000_000, 6_000_000, 4_000_000,
];

/// Every rate any link offers.
pub const ALL_RATES: &[u64] = RATES_USB3;

/// Rates the device offers on a link of this USB version (bcdUSB).
pub fn rates_for(bcd_usb: u16) -> &'static [u64] {
    if bcd_usb >= 0x0300 {
        RATES_USB3
    } else {
        RATES_USB2_8CH
    }
}

/// The divider index of `rate` in `rates`.
pub fn divider(rates: &[u64], rate: u64) -> Result<u8, Error> {
    rates
        .iter()
        .position(|&r| r == rate)
        .map(|i| i as u8)
        .ok_or(Error::UnsupportedSamplerate(rate))
}

/// Threshold range on the input pins, in volts (V2 board, serial `b5…`).
pub const THRESHOLD_MIN: f64 = 0.6;
pub const THRESHOLD_MAX: f64 = 1.5;

/// Logic levels the threshold range stands for (V2 board).
pub const LEVEL_MIN: f64 = 1.0;
pub const LEVEL_MAX: f64 = 3.3;

/// The threshold on the pins for signals of `level` volts (1.0..3.3 V),
/// the way the vendor software maps them.
pub fn threshold_for_level(level: f64) -> f64 {
    THRESHOLD_MIN + (level - LEVEL_MIN) * (THRESHOLD_MAX - THRESHOLD_MIN) / (LEVEL_MAX - LEVEL_MIN)
}

/// The level code for a threshold on the input pins. The DAC takes the
/// logic level at the input in 0.1 V (1.0..3.3 V) with bit 15 set; 0.6 V on
/// the pins is 1.0 V at the input and 1.5 V is 3.3 V (measured by WeAct).
pub fn threshold_code(volts: f64) -> Result<u16, Error> {
    let input = 1.0 + (volts - THRESHOLD_MIN) * (3.3 - 1.0) / (THRESHOLD_MAX - THRESHOLD_MIN);
    let code = (input * 10.0).round();
    if !(10.0..=33.0).contains(&code) || !volts.is_finite() {
        return Err(Error::Threshold(volts));
    }
    Ok(0x8000 | code as u16)
}

/// The threshold a level code stands for (inverse of [`threshold_code`]).
pub fn threshold_of_code(code: u16) -> Option<f64> {
    if code & 0x8000 == 0 {
        return None;
    }
    let input = f64::from(code & 0x7fff) / 10.0;
    Some(THRESHOLD_MIN + (input - 1.0) * (THRESHOLD_MAX - THRESHOLD_MIN) / (3.3 - 1.0))
}

/// A command packet.
pub fn command(cmd: u8, data: &[u8]) -> [u8; PACKET_OUT] {
    let mut p = [0u8; PACKET_OUT];
    p[0] = cmd;
    p[1] = data.len() as u8;
    p[2..2 + data.len()].copy_from_slice(data);
    p
}

/// The data of a reply to `cmd`, after checking the echo and status byte.
pub fn reply_data(cmd: u8, reply: &[u8]) -> Result<Vec<u8>, Error> {
    let bad = |what: &str| Error::Protocol(format!("reply to 0x{cmd:02x}: {what}: {reply:02x?}"));
    if reply.len() < 2 {
        return Err(bad("too short"));
    }
    if reply[0] != cmd | 0x10 {
        return Err(bad("wrong command echo"));
    }
    let n = reply[1] as usize;
    if n == 0 {
        return Ok(Vec::new());
    }
    if reply.len() < 3 {
        return Err(bad("no status byte"));
    }
    if reply[2] != 0 {
        return Err(bad(&format!("status 0x{:02x}", reply[2])));
    }
    let end = (2 + n).min(reply.len());
    Ok(reply[3..end].to_vec())
}

/// Bytes per bulk read: about 10 ms of samples, whole 16 KiB SuperSpeed
/// bursts, between 64 KiB and 1 MiB.
pub fn transfer_size(samplerate: u64) -> usize {
    const BURST: usize = 16 * 1024;
    let s = (samplerate / 100) as usize;
    s.div_ceil(BURST).clamp(4, 64) * BURST
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u3_divider_is_the_table_index() {
        assert_eq!(divider(RATES_USB3, 200_000_000).unwrap(), 0);
        assert_eq!(divider(RATES_USB3, 20_000_000).unwrap(), 19);
        assert_eq!(divider(RATES_USB3, 4_000_000).unwrap(), 24);
        assert_eq!(divider(RATES_USB2_8CH, 40_000_000).unwrap(), 0);
        assert_eq!(divider(RATES_USB2_8CH, 4_000_000).unwrap(), 7);
        assert!(divider(RATES_USB2_8CH, 200_000_000).is_err());
        assert_eq!(RATES_USB3.len(), 25);
        assert_eq!(rates_for(0x0300).len(), 25);
        assert_eq!(rates_for(0x0210), RATES_USB2_8CH);
    }

    #[test]
    fn u3_threshold_codes() {
        // The ends of the range are 1.0 V and 3.3 V at the input.
        assert_eq!(threshold_code(0.6).unwrap(), 0x8000 | 10);
        assert_eq!(threshold_code(1.5).unwrap(), 0x8000 | 33);
        // 2.3 V at the input (what a fresh board reported).
        let t = threshold_of_code(0x8017).unwrap();
        assert!((t - 1.1087).abs() < 1e-3, "{t}");
        assert_eq!(threshold_code(t).unwrap(), 0x8017);
        assert!(threshold_code(0.5).is_err());
        assert!(threshold_code(1.6).is_err());
        assert!(threshold_code(f64::NAN).is_err());
        assert_eq!(threshold_of_code(0x0017), None);
        for (level, code) in [(1.0, 10), (1.8, 18), (3.3, 33)] {
            assert_eq!(
                threshold_code(threshold_for_level(level)).unwrap(),
                0x8000 | code
            );
        }
    }

    #[test]
    fn u3_reply_parsing() {
        // GET_VERSION on firmware 0.2: sw 2, IAP 1.
        let r = [0xbc, 0x03, 0x00, 0x02, 0x01, 0, 0];
        assert_eq!(reply_data(CMD_GET_VERSION, &r).unwrap(), [2, 1]);
        // SET_LOGIC_PARAMS answers with status only.
        let r = [0xb0, 0x01, 0x00, 0x80, 0x17];
        assert_eq!(reply_data(CMD_SET_LOGIC_PARAMS, &r).unwrap(), [0u8; 0]);
        assert!(reply_data(CMD_GET_VERSION, &[0xb4, 1, 0]).is_err());
        assert!(reply_data(CMD_GET_VERSION, &[0xbc, 1, 5]).is_err());
        assert!(reply_data(CMD_GET_VERSION, &[0xbc]).is_err());
        let p = command(CMD_SET_LOGIC_PARAMS, &[8, 19, 0]);
        assert_eq!(&p[..5], &[0xa0, 3, 8, 19, 0]);
        assert_eq!(p.len(), 64);
    }

    #[test]
    fn u3_transfer_sizes() {
        assert_eq!(transfer_size(200_000_000), 1 << 20);
        assert_eq!(transfer_size(4_000_000), 64 * 1024);
        assert_eq!(transfer_size(20_000_000), 208 * 1024);
        for &r in RATES_USB3 {
            assert_eq!(transfer_size(r) % (16 * 1024), 0);
        }
    }
}
