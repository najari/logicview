//! The link to the device's IO client: connecting, memory access, reset.
//!
//! The sequences are the ones the Infineon tools send through a miniWiggler
//! (see [`crate::mwtrace`]): every access starts with "IO client 1" and
//! `IOCONF = 0xC1`, words go as block transfers, and a transfer the device
//! does not answer is followed by a read of `IOINFO`.

use crate::bits::Bits;
use crate::telegram::{
    READ_16, READ_32, Reply, Telegram, TelegramError, block_crc32, cmd, crc6, parse_payloads,
    parse_reply, reg, write_payload,
};
use crate::wire::{Op, Wire, WireError};

/// Clock while connecting without a reset, as the Infineon tools use.
pub const ATTACH_CLOCK_HZ: u32 = 400_000;
/// Clock around a reset.
pub const RESET_CLOCK_HZ: u32 = 1_000_000;
/// Clock after connecting, unless another is asked for.
pub const DEFAULT_CLOCK_HZ: u32 = 5_000_000;

/// The largest block transfer, in words.
pub const MAX_BLOCK_WORDS: usize = 256;

/// The answer to a `SYNC` telegram.
const SYNC_ANSWER: u64 = 0xAAAA_AAAA;
/// `DAPISC` arguments: the low 16 bits set how long the device may stay
/// busy before it answers, and come back as the answer.
const DAPISC_ATTACH: u64 = 0x4ABB_AF53_0F00;
const DAPISC_RESET: u64 = 0x4ABB_AF53_0400;
const DAPISC_SHORT: u64 = 0x0F00;
/// JTAG instruction that selects the ID code.
const JTAG_IDCODE: u64 = 0x04;

/// `OJCONF` values around a reset. Bit 14 keeps the cores halted.
const OJCONF_AFTER_RESET: u64 = 0x0503;
const OJCONF_HALT: u64 = 0x4501;
const OJCONF_RUN: u64 = 0x0501;
/// `IOCONF` value in front of every access.
const IOCONF_ACCESS: u64 = 0xC1;

/// `IOINFO` after a reset once the device takes accesses, and while it
/// waits for the debug password.
const IOINFO_READY: u16 = 0x0400;
const IOINFO_LOCKED: u16 = 0x0080;

/// Bytes clocked in after each kind of telegram (the miniWiggler's counts).
const IN_SET_CLIENT: usize = 3;
const IN_WRITE: usize = 7;
const IN_READ_16: usize = 9;
const IN_READ_32: usize = 11;
const IN_DAPISC: usize = 5;
const IN_SYNC: usize = 7;
const IN_JTAG_IR: usize = 3;
const IN_JTAG_DR: usize = 7;

/// How to connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset {
    /// Attach to the running device.
    None,
    /// Reset and keep the cores halted.
    Halt,
    /// Reset and let the device start.
    Run,
}

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error(
        "장치가 {0}에 응답하지 않습니다. 배선(DAP0·DAP1·GND), IO 전압, 장치가 DAP 모드인지 확인하세요"
    )]
    NoAnswer(&'static str),
    #[error("{what}의 응답이 깨졌습니다: {error}")]
    Reply {
        what: &'static str,
        error: TelegramError,
    },
    #[error("{0}의 응답이 예상과 다릅니다: {1}")]
    Unexpected(&'static str, String),
    #[error("장치가 디버그 암호로 잠겨 있습니다 (IOINFO {0:#06x})")]
    Locked(u16),
    #[error("{}", access_text(.address, .write, .ioinfo))]
    Access {
        address: u32,
        write: bool,
        ioinfo: Option<u16>,
    },
    #[error("{0:#010x} 블록 읽기의 CRC32가 맞지 않습니다")]
    BlockCrc(u32),
    #[error("연결되지 않았습니다")]
    NotConnected,
}

impl LinkError {
    /// Whether the device refused this one access (a bus error) while the
    /// link itself still works.
    pub fn is_access(&self) -> bool {
        matches!(self, LinkError::Access { .. })
    }
}

/// Memory and reset access to a device, however it is reached.
pub trait Target: Send {
    fn name(&self) -> String;
    /// Connects, with or without a reset, and returns the JTAG ID.
    fn connect(&mut self, reset: Reset) -> Result<u32, LinkError>;
    /// The JTAG ID of the device connected last.
    fn jtag_id(&self) -> Option<u32>;
    /// Reads `buf.len()` bytes. A length of 1, 2 or 4 at an aligned address
    /// is one access of that width.
    fn read(&mut self, address: u32, buf: &mut [u8]) -> Result<(), LinkError>;
    fn write(&mut self, address: u32, data: &[u8]) -> Result<(), LinkError>;
    fn clock_hz(&self) -> u32;
    fn set_clock_hz(&mut self, hz: u32) -> Result<u32, LinkError>;
}

/// A telegram with the bytes to clock in after it.
fn exchange(ops: &mut Vec<Op>, telegram: Telegram, in_bytes: usize) {
    ops.push(Op::Out(telegram.padded()));
    ops.push(Op::In(in_bytes * 8));
}

/// "IO client 1" and `IOCONF = 0xC1`: two exchanges.
fn preamble(ops: &mut Vec<Op>) {
    exchange(ops, Telegram::set_io_client(1), IN_SET_CLIENT);
    exchange(
        ops,
        Telegram::write_reg(reg::IOCONF, 12, IOCONF_ACCESS),
        IN_WRITE,
    );
}

/// The bytes the miniWiggler clocks in for a block read of `words`.
pub fn block_read_bytes(words: usize) -> usize {
    words * 38 / 8 + 12
}

fn value(what: &'static str, bits: &Bits, n: usize) -> Result<Option<u64>, LinkError> {
    match parse_reply(bits, n) {
        Ok(Reply::Value(v)) => Ok(Some(v)),
        Ok(Reply::Silent) => Ok(None),
        // A start bit and nothing else is a value of zero only if its CRC
        // is zero too, which it never is.
        Ok(Reply::Ack) => Err(LinkError::Unexpected(what, "값 없이 시작 비트만 옴".into())),
        Err(error) => Err(LinkError::Reply { what, error }),
    }
}

fn acked(bits: &Bits) -> bool {
    bits.find_one(0).is_some()
}

pub struct Link<W: Wire> {
    wire: W,
    /// The clock used once connected.
    fast_hz: u32,
    jtag_id: Option<u32>,
    connected: bool,
    /// The port was set up after a reset ([`DAPISC_RESET`]): the device
    /// answers eight bits sooner than after an attach.
    early_answers: bool,
}

impl<W: Wire> Link<W> {
    pub fn new(wire: W) -> Link<W> {
        Link {
            wire,
            fast_hz: DEFAULT_CLOCK_HZ,
            jtag_id: None,
            connected: false,
            early_answers: false,
        }
    }

    pub fn wire(&self) -> &W {
        &self.wire
    }

    pub fn wire_mut(&mut self) -> &mut W {
        &mut self.wire
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Takes the device as connected without talking to it: for a wire
    /// that another tool has set up already, and for tests.
    pub fn assume_connected(&mut self) {
        self.connected = true;
    }

    /// The clock to use once connected.
    pub fn set_speed(&mut self, hz: u32) -> Result<u32, LinkError> {
        self.fast_hz = hz;
        if self.connected {
            self.fast_hz = self.wire.set_clock(hz)?;
        }
        Ok(self.fast_hz)
    }

    pub fn speed(&self) -> u32 {
        if self.connected {
            self.wire.clock()
        } else {
            self.fast_hz
        }
    }

    /// `IOINFO` of IO client 1. Reading it also ends a failed access.
    pub fn ioinfo(&mut self) -> Result<u16, LinkError> {
        let mut ops = Vec::new();
        exchange(&mut ops, Telegram::set_io_client(1), IN_SET_CLIENT);
        exchange(
            &mut ops,
            Telegram::read_reg(reg::IOINFO, READ_16),
            IN_READ_16,
        );
        let got = self.wire.run(&ops)?;
        value("IOINFO", &got[1], 16)?
            .map(|v| v as u16)
            .ok_or(LinkError::NoAnswer("IOINFO 읽기"))
    }

    /// Reads the JTAG ID through the JTAG telegrams.
    pub fn read_jtag_id(&mut self) -> Result<u32, LinkError> {
        let mut ops = Vec::new();
        exchange(
            &mut ops,
            Telegram::new(cmd::JTAG_SET_IR, 8, JTAG_IDCODE),
            IN_JTAG_IR,
        );
        exchange(
            &mut ops,
            Telegram::new(cmd::JTAG_SWAP_DR, 32, 0),
            IN_JTAG_DR,
        );
        let got = self.wire.run(&ops)?;
        let id = if self.early_answers {
            // The start bit is over before the tool listens: the ID code
            // starts at the first bit, its CRC follows. (A CRC6 does not
            // change with ones in front, so the position has to be known.)
            let answer = &got[1];
            let expected = crc6(answer.slice(0, 32).iter());
            let found = answer.value(32, 6) as u8;
            if answer.find_one(0).is_none() {
                return Err(LinkError::NoAnswer("JTAG ID 읽기"));
            }
            if found != expected {
                return Err(LinkError::Reply {
                    what: "JTAG ID",
                    error: TelegramError::Crc { found, expected },
                });
            }
            answer.value(0, 32) as u32
        } else {
            value("JTAG ID", &got[1], 32)?.ok_or(LinkError::NoAnswer("JTAG ID 읽기"))? as u32
        };
        self.jtag_id = Some(id);
        Ok(id)
    }

    /// Connects to the device and returns its JTAG ID.
    pub fn connect(&mut self, reset: Reset) -> Result<u32, LinkError> {
        self.connected = false;
        self.jtag_id = None;
        match reset {
            Reset::None => self.attach()?,
            Reset::Halt | Reset::Run => self.reset(reset == Reset::Halt)?,
        }
        self.wire.set_clock(self.fast_hz)?;
        self.connected = true;
        let id = self.read_jtag_id()?;
        log::debug!(
            "DAP: connected, JTAG ID {id:#010x}, {} Hz",
            self.wire.clock()
        );
        Ok(id)
    }

    /// Attaches without a reset: sync, port setup, then the first access
    /// preamble (which the device may leave unanswered once).
    fn attach(&mut self) -> Result<(), LinkError> {
        self.early_answers = false;
        self.wire.set_clock(ATTACH_CLOCK_HZ)?;
        let mut ops = vec![Op::Idle(8)];
        exchange(&mut ops, Telegram::bare(cmd::SYNC), IN_SYNC);
        exchange(
            &mut ops,
            Telegram::new(cmd::DAPISC, 48, DAPISC_ATTACH),
            IN_DAPISC,
        );
        exchange(&mut ops, Telegram::bare(cmd::JTAG_RESET), IN_JTAG_IR);
        let got = self.wire.run(&ops)?;
        match value("SYNC", &got[0], 32)? {
            Some(SYNC_ANSWER) => {}
            Some(other) => {
                return Err(LinkError::Unexpected("SYNC", format!("{other:#x}")));
            }
            None => return Err(LinkError::NoAnswer("SYNC")),
        }
        match value("DAPISC", &got[1], 16)? {
            Some(v) if v == DAPISC_ATTACH & 0xFFFF => {}
            Some(other) => {
                return Err(LinkError::Unexpected("DAPISC", format!("{other:#x}")));
            }
            None => return Err(LinkError::NoAnswer("DAPISC")),
        }

        // The first preamble after attaching is often not acknowledged;
        // reading IOINFO puts the IO client back in order.
        for attempt in 0..3 {
            let mut ops = Vec::new();
            preamble(&mut ops);
            let got = self.wire.run(&ops)?;
            if acked(&got[1]) {
                return Ok(());
            }
            let ioinfo = self.ioinfo()?;
            log::debug!(
                "DAP: attach attempt {attempt}: IOCONF not acknowledged, IOINFO {ioinfo:#06x}"
            );
            if ioinfo == IOINFO_LOCKED {
                return Err(LinkError::Locked(ioinfo));
            }
        }
        Err(LinkError::NoAnswer("IOCONF 쓰기"))
    }

    /// Resets the device through the reset line and sets it up again.
    fn reset(&mut self, halt: bool) -> Result<(), LinkError> {
        self.early_answers = true;
        self.wire.set_clock(RESET_CLOCK_HZ)?;
        // The setup has to follow the reset closely: the device only takes
        // the halt request while it starts up.
        let mut ops = vec![
            Op::Reset(true),
            Op::Sleep(10),
            Op::Reset(false),
            Op::Idle(200),
        ];
        exchange(
            &mut ops,
            Telegram::new(cmd::DAPISC, 16, DAPISC_SHORT),
            IN_DAPISC,
        );
        exchange(
            &mut ops,
            Telegram::new(cmd::DAPISC, 48, DAPISC_RESET),
            IN_DAPISC,
        );
        exchange(&mut ops, Telegram::set_io_client(1), IN_SET_CLIENT);
        exchange(
            &mut ops,
            Telegram::read_reg(reg::IOINFO, READ_16),
            IN_READ_16,
        );
        exchange(
            &mut ops,
            Telegram::write_reg(reg::OJCONF, 16, OJCONF_AFTER_RESET),
            IN_WRITE,
        );
        let got = self.wire.run(&ops)?;
        match value("DAPISC", &got[1], 16)? {
            Some(v) if v == DAPISC_RESET & 0xFFFF => {}
            Some(other) => {
                return Err(LinkError::Unexpected("DAPISC", format!("{other:#x}")));
            }
            None => return Err(LinkError::NoAnswer("리셋 뒤 DAPISC")),
        }
        if !acked(&got[4]) {
            return Err(LinkError::NoAnswer("OJCONF 쓰기"));
        }

        // IOINFO reads 0x80 for a moment, then 0x400. A device that asks
        // for the debug password stays at 0x80.
        let mut ioinfo = 0;
        for _ in 0..50 {
            ioinfo = self.ioinfo()?;
            if ioinfo == IOINFO_READY {
                break;
            }
        }
        if ioinfo == IOINFO_LOCKED {
            return Err(LinkError::Locked(ioinfo));
        }
        if ioinfo != IOINFO_READY {
            return Err(LinkError::Unexpected(
                "리셋 뒤 IOINFO",
                format!("{ioinfo:#06x}"),
            ));
        }

        let mut ops = Vec::new();
        exchange(&mut ops, Telegram::set_io_client(1), IN_SET_CLIENT);
        exchange(
            &mut ops,
            Telegram::write_reg(reg::OJCONF, 16, if halt { OJCONF_HALT } else { OJCONF_RUN }),
            IN_WRITE,
        );
        if halt {
            exchange(
                &mut ops,
                Telegram::write_reg(reg::IOCONF, 12, IOCONF_ACCESS),
                IN_WRITE,
            );
        }
        self.wire.run(&ops)?;
        Ok(())
    }

    /// After an access the device did not answer: reads `IOINFO`, which
    /// ends the failed access, and names the failure.
    fn access_failed(&mut self, address: u32, write: bool) -> LinkError {
        match self.ioinfo() {
            Ok(ioinfo) => LinkError::Access {
                address,
                write,
                ioinfo: Some(ioinfo),
            },
            Err(LinkError::Wire(e)) => LinkError::Wire(e),
            Err(_) => LinkError::Access {
                address,
                write,
                ioinfo: None,
            },
        }
    }

    fn check_connected(&self) -> Result<(), LinkError> {
        if self.connected {
            Ok(())
        } else {
            Err(LinkError::NotConnected)
        }
    }

    /// Reads `words` 32-bit words (1 to 256) at a word address.
    pub fn read_block(&mut self, address: u32, words: usize) -> Result<Vec<u8>, LinkError> {
        self.check_connected()?;
        assert!(address.is_multiple_of(4) && (1..=MAX_BLOCK_WORDS).contains(&words));
        for attempt in 0..2 {
            let mut ops = Vec::new();
            preamble(&mut ops);
            exchange(
                &mut ops,
                Telegram::block(true, address, words),
                block_read_bytes(words),
            );
            let got = self.wire.run(&ops)?;
            let payloads = parse_payloads(&got[2], words + 1);
            if payloads.is_empty() {
                return Err(self.access_failed(address, false));
            }
            let data: Vec<u8> = payloads
                .iter()
                .take(words)
                .flat_map(|w| w.to_le_bytes())
                .collect();
            if payloads.len() == words + 1 && payloads[words] == block_crc32(&data) {
                return Ok(data);
            }
            log::warn!(
                "DAP: block read {address:#010x}: {} of {} words, attempt {attempt}",
                payloads.len(),
                words + 1
            );
            // Leave the device in a known state before trying again.
            let _ = self.ioinfo()?;
        }
        Err(LinkError::BlockCrc(address))
    }

    /// Writes whole 32-bit words (1 to 256) at a word address.
    pub fn write_block(&mut self, address: u32, data: &[u8]) -> Result<(), LinkError> {
        self.check_connected()?;
        let words = data.len() / 4;
        assert!(
            address.is_multiple_of(4)
                && data.len().is_multiple_of(4)
                && (1..=MAX_BLOCK_WORDS).contains(&words)
        );
        let mut ops = Vec::new();
        preamble(&mut ops);
        exchange(&mut ops, Telegram::block(false, address, words), IN_WRITE);
        for (i, word) in data.as_chunks::<4>().0.iter().enumerate() {
            let word = u32::from_le_bytes(*word);
            ops.push(Op::Out(write_payload(word)));
            ops.push(Op::In(if i + 1 == words { 16 } else { 8 }));
        }
        exchange(
            &mut ops,
            Telegram::write_reg(reg::IOADDR, 32, address as u64),
            IN_WRITE,
        );
        let got = self.wire.run(&ops)?;
        // The block telegram and every word are acknowledged.
        if got[2..3 + words].iter().all(acked) {
            Ok(())
        } else {
            Err(self.access_failed(address, true))
        }
    }

    /// One 8 or 16 bit read.
    fn read_narrow(&mut self, address: u32, bytes: usize) -> Result<u32, LinkError> {
        self.check_connected()?;
        let mut ops = Vec::new();
        preamble(&mut ops);
        exchange(
            &mut ops,
            Telegram::write_reg(reg::IOADDR, 32, address as u64),
            IN_WRITE,
        );
        let register = if bytes == 1 { reg::READ8 } else { reg::READ16 };
        exchange(&mut ops, Telegram::read_reg(register, READ_32), IN_READ_32);
        let got = self.wire.run(&ops)?;
        match value("읽기", &got[3], 32) {
            // The byte comes four times, the half word twice.
            Ok(Some(v)) => Ok(v as u32 & if bytes == 1 { 0xFF } else { 0xFFFF }),
            Ok(None) => Err(self.access_failed(address, false)),
            Err(e) => Err(e),
        }
    }

    /// One 8 or 16 bit write.
    fn write_narrow(&mut self, address: u32, value: u32, bytes: usize) -> Result<(), LinkError> {
        self.check_connected()?;
        let mut ops = Vec::new();
        preamble(&mut ops);
        exchange(
            &mut ops,
            Telegram::write_reg(reg::IOADDR, 32, address as u64),
            IN_WRITE,
        );
        let (register, width) = if bytes == 1 {
            (reg::WRITE8, 8)
        } else {
            (reg::WRITE16, 16)
        };
        exchange(
            &mut ops,
            Telegram::write_reg(register, width, value as u64),
            IN_WRITE,
        );
        let got = self.wire.run(&ops)?;
        if acked(&got[3]) {
            Ok(())
        } else {
            Err(self.access_failed(address, true))
        }
    }

    pub fn read8(&mut self, address: u32) -> Result<u8, LinkError> {
        self.read_narrow(address, 1).map(|v| v as u8)
    }

    pub fn read16(&mut self, address: u32) -> Result<u16, LinkError> {
        self.read_narrow(address, 2).map(|v| v as u16)
    }

    pub fn read32(&mut self, address: u32) -> Result<u32, LinkError> {
        let b = self.read_block(address, 1)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn write8(&mut self, address: u32, value: u8) -> Result<(), LinkError> {
        self.write_narrow(address, value as u32, 1)
    }

    pub fn write16(&mut self, address: u32, value: u16) -> Result<(), LinkError> {
        self.write_narrow(address, value as u32, 2)
    }

    pub fn write32(&mut self, address: u32, value: u32) -> Result<(), LinkError> {
        self.write_block(address, &value.to_le_bytes())
    }
}

/// How an access of any address and length is cut into accesses the port
/// can do: bytes and half words up to a word boundary, blocks of words,
/// then the rest. Returns `(offset, length)` pieces of 1, 2 or a multiple
/// of 4 bytes.
pub fn split_access(address: u32, len: usize) -> Vec<(usize, usize)> {
    let mut pieces = Vec::new();
    let mut at = 0;
    while at < len {
        let a = address.wrapping_add(at as u32);
        let left = len - at;
        let n = if a.is_multiple_of(4) && left >= 4 {
            (left / 4).min(MAX_BLOCK_WORDS) * 4
        } else if a.is_multiple_of(2) && left >= 2 {
            2
        } else {
            1
        };
        pieces.push((at, n));
        at += n;
    }
    pieces
}

impl<W: Wire> Target for Link<W> {
    fn name(&self) -> String {
        self.wire.name()
    }

    fn connect(&mut self, reset: Reset) -> Result<u32, LinkError> {
        Link::connect(self, reset)
    }

    fn jtag_id(&self) -> Option<u32> {
        self.jtag_id
    }

    fn read(&mut self, address: u32, buf: &mut [u8]) -> Result<(), LinkError> {
        for (at, n) in split_access(address, buf.len()) {
            let a = address.wrapping_add(at as u32);
            match n {
                1 => buf[at] = self.read8(a)?,
                2 => buf[at..at + 2].copy_from_slice(&self.read16(a)?.to_le_bytes()),
                _ => buf[at..at + n].copy_from_slice(&self.read_block(a, n / 4)?),
            }
        }
        Ok(())
    }

    fn write(&mut self, address: u32, data: &[u8]) -> Result<(), LinkError> {
        for (at, n) in split_access(address, data.len()) {
            let a = address.wrapping_add(at as u32);
            match n {
                1 => self.write8(a, data[at])?,
                2 => self.write16(a, u16::from_le_bytes([data[at], data[at + 1]]))?,
                _ => self.write_block(a, &data[at..at + n])?,
            }
        }
        Ok(())
    }

    fn clock_hz(&self) -> u32 {
        self.speed()
    }

    fn set_clock_hz(&mut self, hz: u32) -> Result<u32, LinkError> {
        self.set_speed(hz)
    }
}

impl Target for Box<dyn Target> {
    fn name(&self) -> String {
        (**self).name()
    }

    fn connect(&mut self, reset: Reset) -> Result<u32, LinkError> {
        (**self).connect(reset)
    }

    fn jtag_id(&self) -> Option<u32> {
        (**self).jtag_id()
    }

    fn read(&mut self, address: u32, buf: &mut [u8]) -> Result<(), LinkError> {
        (**self).read(address, buf)
    }

    fn write(&mut self, address: u32, data: &[u8]) -> Result<(), LinkError> {
        (**self).write(address, data)
    }

    fn clock_hz(&self) -> u32 {
        (**self).clock_hz()
    }

    fn set_clock_hz(&mut self, hz: u32) -> Result<u32, LinkError> {
        (**self).set_clock_hz(hz)
    }
}

fn access_text(address: &u32, write: &bool, ioinfo: &Option<u16>) -> String {
    format!(
        "{address:#010x} {} 실패: 장치가 접근을 받지 않았습니다{}",
        if *write { "쓰기" } else { "읽기" },
        ioinfo
            .map(|v| format!(" (IOINFO {v:#06x})"))
            .unwrap_or_default()
    )
}
