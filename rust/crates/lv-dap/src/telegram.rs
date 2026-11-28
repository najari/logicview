//! DAP telegrams: what the tool sends on DAP1 and what the device answers.
//!
//! A telegram is, first bit first: a start bit `1`, the command (5 bits),
//! the argument length (6 bits), the argument (0 to 63 bits) and a CRC6 over
//! all of that. Numbers go least significant bit first. A command without an
//! argument carries `0x3F` in the length field. The device answers with zero
//! bits, a start bit, the value and a CRC6 over the value.
//!
//! The format follows the public analysis of the miniWiggler traffic
//! (epozzobon/tricore-things); the tests of [`crate::mwtrace`] compare it
//! with captures of the Infineon tools.

use crate::bits::Bits;

/// Telegram commands.
pub mod cmd {
    /// Shift 32 bits through the JTAG data register.
    pub const JTAG_SWAP_DR: u8 = 2;
    /// Write a register of the IO client.
    pub const WRITE_REG: u8 = 8;
    /// Start a block write; the words follow as payloads.
    pub const WRITE_MEM: u8 = 9;
    /// Read a block; the words come back as payloads, then a CRC32.
    pub const READ_MEM: u8 = 10;
    pub const SYNC: u8 = 16;
    /// Configures the DAP port itself.
    pub const DAPISC: u8 = 17;
    pub const JTAG_SET_IR: u8 = 19;
    pub const JTAG_RESET: u8 = 21;
    /// Read a register of the IO client.
    pub const READ_REG: u8 = 26;
    pub const SET_IO_CLIENT: u8 = 28;
}

/// Registers of the IO client (Cerberus).
pub mod reg {
    /// IO configuration, 12 bits.
    pub const IOCONF: u8 = 0x0;
    /// The address of the next access.
    pub const IOADDR: u8 = 0x1;
    /// Writing stores 32 bits at the address (also COMDATA, for keys).
    pub const WRITE32: u8 = 0x4;
    /// Reading returns the 32 bits at the address.
    pub const READ32: u8 = 0x5;
    pub const WRITE16: u8 = 0x6;
    /// The 16 bits at the address, twice.
    pub const READ16: u8 = 0x7;
    pub const WRITE8: u8 = 0x8;
    /// The byte at the address, four times.
    pub const READ8: u8 = 0x9;
    /// Status of the IO client, 16 bits.
    pub const IOINFO: u8 = 0xB;
    /// OCDS/JTAG configuration, 16 bits.
    pub const OJCONF: u8 = 0xE;
}

/// Size flags of a `READ_REG` argument.
pub const READ_16: u8 = 0x40;
pub const READ_32: u8 = 0x50;

/// The argument length field of a telegram without an argument.
const NO_ARGUMENT: u64 = 0x3F;

/// CRC6 of the DAP telegrams.
pub fn crc6(bits: impl IntoIterator<Item = bool>) -> u8 {
    let mut crc: u8 = 0x3F;
    for bit in bits {
        let b = (bit as u8 ^ crc ^ (crc >> 1)) & 1;
        crc = (b << 5) | (crc >> 1);
    }
    crc ^ (crc >> 1)
}

/// The CRC32 a block read ends with: the CRC-32 of the data without the
/// final inversion, sent most significant bit first.
pub fn block_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc.reverse_bits()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TelegramError {
    #[error("no start bit")]
    NoStart,
    #[error("the telegram is cut short")]
    Short,
    #[error("CRC6 is {found:#04x}, expected {expected:#04x}")]
    Crc { found: u8, expected: u8 },
}

/// One telegram from the tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Telegram {
    pub cmd: u8,
    /// Length in bits and value, `None` for a command without an argument.
    pub arg: Option<(u8, u64)>,
}

impl Telegram {
    pub fn new(cmd: u8, len: u8, arg: u64) -> Telegram {
        debug_assert!(cmd < 32 && len < 63 && (len == 0 || arg >> (len - 1) <= 1));
        Telegram {
            cmd,
            arg: Some((len, arg)),
        }
    }

    pub fn bare(cmd: u8) -> Telegram {
        Telegram { cmd, arg: None }
    }

    pub fn write_reg(reg: u8, width: u8, value: u64) -> Telegram {
        Telegram::new(cmd::WRITE_REG, width + 4, value << 4 | reg as u64)
    }

    /// `size` is [`READ_16`] or [`READ_32`].
    pub fn read_reg(reg: u8, size: u8) -> Telegram {
        Telegram::new(cmd::READ_REG, 7, (reg | size) as u64)
    }

    pub fn set_io_client(client: u8) -> Telegram {
        Telegram::new(cmd::SET_IO_CLIENT, 3, client as u64)
    }

    /// A block access of `words` 32-bit words (1 to 256) at `address`.
    pub fn block(read: bool, address: u32, words: usize) -> Telegram {
        debug_assert!(address.is_multiple_of(4) && (1..=256).contains(&words));
        let arg = (address as u64 >> 2) << 10 | ((words & 0xFF) as u64) << 2 | read as u64;
        Telegram::new(if read { cmd::READ_MEM } else { cmd::WRITE_MEM }, 40, arg)
    }

    /// For a block telegram: the address and the number of words.
    pub fn block_range(&self) -> Option<(u32, usize)> {
        match (self.cmd, self.arg) {
            (cmd::READ_MEM | cmd::WRITE_MEM, Some((40, arg))) => {
                let words = (arg >> 2 & 0xFF) as usize;
                Some((
                    ((arg >> 10) << 2) as u32,
                    if words == 0 { 256 } else { words },
                ))
            }
            _ => None,
        }
    }

    /// The bits of the telegram, start bit first.
    pub fn bits(&self) -> Bits {
        let mut out = Bits::new();
        out.push(true);
        out.push_value(self.cmd as u64, 5);
        match self.arg {
            Some((len, arg)) => {
                out.push_value(len as u64, 6);
                out.push_value(arg, len as usize);
            }
            None => out.push_value(NO_ARGUMENT, 6),
        }
        let crc = crc6(out.iter());
        out.push_value(crc as u64, 6);
        out
    }

    /// The telegram as the miniWiggler clocks it out: a zero after a final
    /// one bit, and zero bits in front up to a whole number of bytes.
    pub fn padded(&self) -> Bits {
        pad(&self.bits())
    }

    /// Reads a telegram from `bits`, skipping the zero bits before it.
    pub fn parse(bits: &Bits) -> Result<Telegram, TelegramError> {
        let start = bits.find_one(0).ok_or(TelegramError::NoStart)?;
        if bits.len() < start + 18 {
            return Err(TelegramError::Short);
        }
        let cmd = bits.value(start + 1, 5) as u8;
        let len = bits.value(start + 6, 6);
        let (arg, arg_bits) = if len == NO_ARGUMENT {
            (None, 0)
        } else {
            (
                Some((len as u8, bits.value(start + 12, len as usize))),
                len as usize,
            )
        };
        let end = start + 12 + arg_bits;
        if bits.len() < end + 6 {
            return Err(TelegramError::Short);
        }
        let expected = crc6(bits.slice(start, end).iter());
        let found = bits.value(end, 6) as u8;
        if found != expected {
            return Err(TelegramError::Crc { found, expected });
        }
        Ok(Telegram { cmd, arg })
    }
}

/// Pads `bits` as the miniWiggler sends them (see [`Telegram::padded`]).
pub fn pad(bits: &Bits) -> Bits {
    let trailing = !bits.is_empty() && bits.get(bits.len() - 1);
    let total = (bits.len() + trailing as usize).next_multiple_of(8);
    let mut out = Bits::zeros(total - bits.len() - trailing as usize);
    out.extend(bits);
    if trailing {
        out.push(false);
    }
    out
}

/// One word of a block write: a start bit, 32 data bits and six zero bits.
pub fn write_payload(word: u32) -> Bits {
    let mut out = Bits::new();
    out.push(true);
    out.push_value(word as u64, 32);
    out.push_value(0, 6);
    pad(&out)
}

/// What the device sent in answer to a telegram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// Only zero bits: the device did not answer.
    Silent,
    /// A start bit and nothing after it.
    Ack,
    Value(u64),
}

/// Reads a reply with a value of `value_bits` bits from what was clocked in.
pub fn parse_reply(bits: &Bits, value_bits: usize) -> Result<Reply, TelegramError> {
    let Some(start) = bits.find_one(0) else {
        return Ok(Reply::Silent);
    };
    if value_bits == 0 || bits.find_one(start + 1).is_none() {
        return Ok(Reply::Ack);
    }
    let end = start + 1 + value_bits;
    if bits.len() < end + 6 {
        return Err(TelegramError::Short);
    }
    let expected = crc6(bits.slice(start + 1, end).iter());
    let found = bits.value(end, 6) as u8;
    if found != expected {
        return Err(TelegramError::Crc { found, expected });
    }
    Ok(Reply::Value(bits.value(start + 1, value_bits)))
}

/// The words of a block read reply: each word follows a start bit, with
/// zero bits between the words. Returns the words found, at most `count`.
pub fn parse_payloads(bits: &Bits, count: usize) -> Vec<u32> {
    let mut words = Vec::with_capacity(count);
    let mut at = 0;
    while words.len() < count {
        let Some(start) = bits.find_one(at) else {
            break;
        };
        if bits.len() < start + 33 {
            break;
        }
        words.push(bits.value(start + 1, 32) as u32);
        at = start + 33;
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegrams_round_trip() {
        for t in [
            Telegram::bare(cmd::SYNC),
            Telegram::new(cmd::DAPISC, 16, 0xF00),
            Telegram::new(cmd::DAPISC, 48, 0x4ABB_AF53_0400),
            Telegram::set_io_client(1),
            Telegram::read_reg(reg::IOINFO, READ_16),
            Telegram::write_reg(reg::IOADDR, 32, 0xF000_0464),
            Telegram::write_reg(reg::IOCONF, 12, 0xC1),
            Telegram::block(true, 0x7000_002C, 256),
            Telegram::block(false, 0x7000_0000, 3),
        ] {
            assert_eq!(Telegram::parse(&t.bits()), Ok(t));
            let padded = t.padded();
            assert!(padded.len().is_multiple_of(8));
            assert!(!padded.get(padded.len() - 1));
            assert_eq!(Telegram::parse(&padded), Ok(t));
        }
    }

    #[test]
    fn block_arguments_hold_the_address_and_the_words() {
        let t = Telegram::block(true, 0x7000_002C, 256);
        // tricore-things: (addr << 8) | (size & 0x3FC) | 1
        assert_eq!(t.arg, Some((40, 0x7000_002C << 8 | 1)));
        assert_eq!(t.block_range(), Some((0x7000_002C, 256)));
        let t = Telegram::block(false, 0xD000_0010, 16);
        assert_eq!(t.arg, Some((40, 0xD000_0010 << 8 | 64)));
        assert_eq!(t.block_range(), Some((0xD000_0010, 16)));
    }

    #[test]
    fn a_damaged_telegram_fails_its_crc() {
        let mut bits = Telegram::set_io_client(1).bits();
        let mut bad = bits.slice(0, 12);
        bad.push(!bits.get(12));
        bad.extend(&bits.slice(13, bits.len()));
        assert!(matches!(
            Telegram::parse(&bad),
            Err(TelegramError::Crc { .. })
        ));
        bits.truncate(10);
        assert_eq!(Telegram::parse(&bits), Err(TelegramError::Short));
        assert_eq!(
            Telegram::parse(&Bits::zeros(40)),
            Err(TelegramError::NoStart)
        );
    }

    fn reply(lead: usize, value: u64, n: usize) -> Bits {
        let mut b = Bits::zeros(lead);
        b.push(true);
        b.push_value(value, n);
        let crc = crc6(b.slice(lead + 1, lead + 1 + n).iter());
        b.push_value(crc as u64, 6);
        b.push_value(0, 9);
        b
    }

    #[test]
    fn replies_are_found_behind_the_padding() {
        assert_eq!(parse_reply(&Bits::zeros(72), 32), Ok(Reply::Silent));
        assert_eq!(
            parse_reply(&reply(5, 0x1234_5678, 32), 32),
            Ok(Reply::Value(0x1234_5678))
        );
        assert_eq!(
            parse_reply(&reply(0, 0x400, 16), 16),
            Ok(Reply::Value(0x400))
        );
        let mut ack = Bits::zeros(3);
        ack.push(true);
        ack.push_value(0, 20);
        assert_eq!(parse_reply(&ack, 0), Ok(Reply::Ack));
        assert_eq!(parse_reply(&ack, 32), Ok(Reply::Ack));

        let mut bad = reply(2, 0xFFFF, 16);
        let flipped = !bad.get(4);
        let mut b = bad.slice(0, 4);
        b.push(flipped);
        b.extend(&bad.slice(5, bad.len()));
        bad = b;
        assert!(matches!(
            parse_reply(&bad, 16),
            Err(TelegramError::Crc { .. })
        ));
    }

    #[test]
    fn payloads_are_collected() {
        let mut b = Bits::zeros(3);
        for word in [0xDEAD_BEEFu32, 0, 0x8000_0001] {
            b.push(true);
            b.push_value(word as u64, 32);
            b.push_value(0, 4);
        }
        assert_eq!(parse_payloads(&b, 3), vec![0xDEAD_BEEF, 0, 0x8000_0001]);
        assert_eq!(parse_payloads(&b, 2), vec![0xDEAD_BEEF, 0]);
        assert_eq!(parse_payloads(&b.slice(0, 60), 3), vec![0xDEAD_BEEF]);
    }

    #[test]
    fn write_payloads_are_five_bytes() {
        // tricore-things: '01' + 32 data bits + '000000'.
        assert_eq!(
            write_payload(0x1234_5678).as_bytes(),
            &[0xE2, 0x59, 0xD1, 0x48, 0x00]
        );
    }

    #[test]
    fn the_block_crc_is_the_bit_reversed_crc32_without_the_final_inversion() {
        // CRC-32 of "123456789" is 0xCBF43926.
        assert_eq!(
            block_crc32(b"123456789"),
            (0xCBF4_3926u32 ^ 0xFFFF_FFFF).reverse_bits()
        );
    }
}
