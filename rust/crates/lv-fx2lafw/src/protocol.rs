//! fx2lafw wire protocol: vendor requests, start command and transfer sizing.
//!
//! Values follow libsigrok `src/hardware/fx2lafw/protocol.{c,h}` and
//! `src/ezusb.c` (see `sources/libsigrok` in this repository).

use crate::Error;

/// Cypress EZ-USB "firmware load" vendor request (RAM write, CPUCS register).
pub const REQ_FIRMWARE_LOAD: u8 = 0xa0;
/// Address of the CPUCS register; writing 1 holds the 8051 in reset.
pub const CPUCS_ADDR: u16 = 0xe600;
/// Firmware is written in chunks of this size.
pub const FW_CHUNK_SIZE: usize = 4 * 1024;
/// Largest firmware image the 16-bit address field can reach.
pub const FW_MAX_SIZE: usize = 1 << 16;

pub const CMD_GET_FW_VERSION: u8 = 0xb0;
pub const CMD_START: u8 = 0xb1;
pub const CMD_GET_REVID_VERSION: u8 = 0xb2;

pub const FLAG_CLK_30MHZ: u8 = 0;
pub const FLAG_CLK_48MHZ: u8 = 1 << 6;
pub const FLAG_SAMPLE_8BIT: u8 = 0;

/// Largest GPIF delay the firmware accepts.
pub const MAX_SAMPLE_DELAY: u64 = 6 * 256;
/// Firmware major version this driver speaks.
pub const REQUIRED_FW_MAJOR: u8 = 1;

/// Bulk IN endpoint carrying samples.
pub const EP_DATA: u8 = 0x82;
/// Upper bound of transfers queued at once.
pub const MAX_TRANSFERS: usize = 32;
/// The device is considered stopped after this many empty transfers in a row.
pub const MAX_EMPTY_TRANSFERS: usize = MAX_TRANSFERS * 2;

/// Sample rates offered for 8-channel capture (CAP-1). 48 MHz is left out:
/// 8 channels at 48 MHz exceed what USB 2.0 bulk transfers can carry.
pub const SAMPLERATES: &[u64] = &[
    20_000, 25_000, 50_000, 100_000, 200_000, 250_000, 500_000, 1_000_000, 2_000_000, 3_000_000,
    4_000_000, 6_000_000, 8_000_000, 12_000_000, 16_000_000, 24_000_000,
];

/// Build the 3-byte `CMD_START` payload `[flags, delay_hi, delay_lo]` for
/// 8-bit sampling, choosing the 48 MHz or 30 MHz GPIF clock the same way
/// libsigrok does.
pub fn start_command(samplerate: u64) -> Result<[u8; 3], Error> {
    if samplerate == 0 {
        return Err(Error::UnsupportedSamplerate(samplerate));
    }
    let mut flags = 0u8;
    let mut delay: u64 = 0;
    if 48_000_000 % samplerate == 0 {
        flags = FLAG_CLK_48MHZ;
        delay = 48_000_000 / samplerate - 1;
        if delay > MAX_SAMPLE_DELAY {
            delay = 0;
        }
    }
    if delay == 0 && 30_000_000 % samplerate == 0 {
        flags = FLAG_CLK_30MHZ;
        delay = 30_000_000 / samplerate - 1;
    }
    // A rate neither clock divides, or one too slow for the delay counter.
    let clock = if flags == FLAG_CLK_48MHZ {
        48_000_000
    } else {
        30_000_000
    };
    if delay > MAX_SAMPLE_DELAY || clock / (delay + 1) != samplerate {
        return Err(Error::UnsupportedSamplerate(samplerate));
    }
    flags |= FLAG_SAMPLE_8BIT;
    Ok([flags, (delay >> 8) as u8, (delay & 0xff) as u8])
}

fn bytes_per_ms(samplerate: u64) -> usize {
    (samplerate / 1000).max(1) as usize
}

/// Size of one bulk transfer: 10 ms of data, rounded up to 512 bytes.
pub fn transfer_size(samplerate: u64) -> usize {
    let s = 10 * bytes_per_ms(samplerate);
    (s + 511) & !511
}

/// Number of transfers to keep queued: about 500 ms of data, at most 32.
pub fn transfer_count(samplerate: u64) -> usize {
    (500 * bytes_per_ms(samplerate) / transfer_size(samplerate)).clamp(1, MAX_TRANSFERS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap1_start_command_matches_libsigrok() {
        // 24 MHz: 48 MHz clock, delay 1.
        assert_eq!(start_command(24_000_000).unwrap(), [FLAG_CLK_48MHZ, 0, 1]);
        // 48 MHz itself: delay 0 on the 48 MHz clock.
        assert_eq!(start_command(48_000_000).unwrap(), [FLAG_CLK_48MHZ, 0, 0]);
        // 1 MHz: 48 MHz clock, delay 47.
        assert_eq!(start_command(1_000_000).unwrap(), [FLAG_CLK_48MHZ, 0, 47]);
        // 20 kHz: 48 MHz would need delay 2399 > 1536, so 30 MHz with 1499.
        assert_eq!(
            start_command(20_000).unwrap(),
            [FLAG_CLK_30MHZ, (1499 >> 8) as u8, (1499 & 0xff) as u8]
        );
        // 3 MHz divides 48 MHz: delay 15.
        assert_eq!(start_command(3_000_000).unwrap(), [FLAG_CLK_48MHZ, 0, 15]);
    }

    #[test]
    fn cap1_every_offered_rate_has_a_start_command() {
        for &r in SAMPLERATES {
            let [flags, hi, lo] = start_command(r).unwrap();
            let clock = if flags & FLAG_CLK_48MHZ != 0 {
                48_000_000
            } else {
                30_000_000
            };
            let delay = ((hi as u64) << 8) | lo as u64;
            assert_eq!(clock / (delay + 1), r, "rate {r}");
        }
    }

    #[test]
    fn cap1_rejects_rates_no_clock_divides() {
        assert!(start_command(0).is_err());
        assert!(start_command(7_000_000).is_err());
        assert!(start_command(10_000).is_err()); // needs delay 2999 on 30 MHz
    }

    #[test]
    fn cap3_transfer_sizing() {
        // 24 MHz: 240 KB per transfer, 32 transfers.
        assert_eq!(transfer_size(24_000_000), 240_128);
        assert_eq!(transfer_count(24_000_000), 32);
        // 20 kHz: 200 bytes -> one 512-byte packet; 10000 / 512 = 19 transfers.
        assert_eq!(transfer_size(20_000), 512);
        assert_eq!(transfer_count(20_000), 19);
        for &r in SAMPLERATES {
            assert_eq!(transfer_size(r) % 512, 0);
            assert!((1..=MAX_TRANSFERS).contains(&transfer_count(r)));
        }
    }
}
