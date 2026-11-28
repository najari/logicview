//! A pretend device on the DAP wires, for tests and `lvcli aurix --sim`.
//!
//! It answers telegrams the way the captures of real TC29x and TC39x
//! devices show (reply positions, acknowledge bits, `IOINFO` values around
//! a reset and after a refused access). It has a sparse memory and no cores.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;

use crate::bits::Bits;
use crate::telegram::{READ_32, Telegram, block_crc32, cmd, crc6, reg};
use crate::wire::{Op, Wire, WireError};

/// JTAG ID of a TC39x B-step, as a capture shows it.
pub const SIM_JTAG_ID: u32 = 0x2020_5083;

const IOINFO_AFTER_RESET: u16 = 0x00C0;
const IOINFO_LOCKED: u16 = 0x0080;
const IOINFO_READY: u16 = 0x0400;
/// `IOINFO` after an access the device refused (TC29x; a TC39x shows more bits).
const IOINFO_ERROR: u16 = 0x0020;

/// What the tests may ask the device to do wrong.
#[derive(Debug, Clone, Default)]
pub struct Faults {
    /// Accesses that touch these addresses are refused (bus error).
    pub bad: Vec<Range<u32>>,
    /// Flip one bit of this many block read answers.
    pub corrupt_block_reads: usize,
    /// Leave the first `IOCONF` write after attaching unanswered.
    pub ignore_first_ioconf: bool,
    /// The device wants the debug password after a reset.
    pub locked: bool,
    /// Answer nothing at all (no device on the wires).
    pub dead: bool,
    /// Extra zero bits before every answer.
    pub delay_bits: usize,
}

#[derive(Debug, Default)]
pub struct SimWire {
    pub faults: Faults,
    memory: HashMap<u32, u8>,
    clock: u32,
    reset_low: bool,
    /// Resets seen (falling edges of the reset line).
    pub resets: u32,
    /// The last `OJCONF` written after a reset asked for halted cores.
    pub halted_after_reset: bool,
    /// Every telegram received, for tests.
    pub log: Vec<Telegram>,
    io_client: u8,
    ioaddr: u32,
    ioinfo: u16,
    /// `IOINFO` reads left before a reset device reports ready.
    settling: u32,
    /// A refused access: the IO client answers nothing until `IOINFO` is read.
    failed: bool,
    /// Words still to come of a block write, and where the next one goes.
    block_write: Option<(u32, usize)>,
    jtag_ir: u8,
    jtag_id: u32,
    /// Set up with the short busy time of the reset sequence: answers come
    /// eight bits sooner.
    early: bool,
    answer: VecDeque<bool>,
}

impl SimWire {
    pub fn new() -> SimWire {
        let mut sim = SimWire {
            clock: 400_000,
            ioinfo: IOINFO_READY,
            jtag_ir: 0x04,
            ..SimWire::default()
        };
        sim.set_jtag_id(SIM_JTAG_ID);
        sim
    }

    /// Makes the device another one, as far as its ID code goes.
    pub fn set_jtag_id(&mut self, id: u32) {
        self.jtag_id = id;
        // CBS_JTAGID, where tools read the ID code through the bus.
        self.poke(0xF000_0464, &id.to_le_bytes());
    }

    pub fn with_faults(faults: Faults) -> SimWire {
        SimWire {
            faults,
            ..SimWire::new()
        }
    }

    pub fn poke(&mut self, address: u32, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.memory.insert(address.wrapping_add(i as u32), b);
        }
    }

    pub fn peek(&self, address: u32, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| {
                self.memory
                    .get(&address.wrapping_add(i as u32))
                    .copied()
                    .unwrap_or(0)
            })
            .collect()
    }

    fn is_bad(&self, address: u32, len: usize) -> bool {
        self.faults
            .bad
            .iter()
            .any(|r| address < r.end && address.wrapping_add(len as u32) > r.start)
    }

    fn refuse(&mut self) {
        self.failed = true;
        self.ioinfo |= IOINFO_ERROR;
    }

    /// Queues an answer whose start bit comes after `lead` zero bits, or
    /// eight bits sooner; then the start bit may be gone before the tool
    /// listens.
    fn answer(&mut self, lead: usize, value: Option<(u64, usize)>) {
        let lead = lead as isize + self.faults.delay_bits as isize
            - if self.early && lead >= 7 { 8 } else { 0 };
        let mut bits = Bits::zeros(lead.max(0) as usize);
        if lead >= 0 {
            bits.push(true);
        }
        if let Some((value, n)) = value {
            let mut v = Bits::new();
            v.push_value(value, n);
            bits.extend(&v);
            bits.push_value(crc6(v.iter()) as u64, 6);
        }
        self.answer = bits.iter().collect();
    }

    fn device_reset(&mut self) {
        self.resets += 1;
        self.ioinfo = IOINFO_AFTER_RESET;
        self.settling = 0;
        self.failed = false;
        self.block_write = None;
        self.halted_after_reset = false;
        self.io_client = 0;
        self.jtag_ir = 0x04;
        self.faults.ignore_first_ioconf = false;
    }

    fn payload(&mut self, bits: &Bits) {
        let Some((address, left)) = self.block_write else {
            return;
        };
        let start = bits.find_one(0).unwrap_or(0);
        let word = bits.value(start + 1, 32) as u32;
        if self.is_bad(address, 4) {
            self.block_write = None;
            self.refuse();
            return;
        }
        self.poke(address, &word.to_le_bytes());
        self.block_write = (left > 1).then_some((address.wrapping_add(4), left - 1));
        // A lone bit after each word; more bits after the last one.
        let mut ack = Bits::zeros(1 + self.faults.delay_bits);
        ack.push(true);
        if left == 1 {
            ack.push_value(0b11_0000_0000, 12);
        }
        self.answer = ack.iter().collect();
    }

    fn telegram(&mut self, t: Telegram) {
        self.log.push(t);
        let arg = t.arg.map_or(0, |(_, a)| a);
        let len = t.arg.map_or(0, |(l, _)| l);
        match t.cmd {
            cmd::SYNC => self.answer(2, Some((0xAAAA_AAAA, 32))),
            cmd::DAPISC if len == 48 => {
                self.early = arg & 0xFFFF == 0x0400;
                self.answer(2, Some((arg & 0xFFFF, 16)));
            }
            cmd::DAPISC => {}
            cmd::JTAG_RESET => {
                self.jtag_ir = 0x04;
                self.answer(8, None);
            }
            cmd::JTAG_SET_IR => {
                self.jtag_ir = arg as u8;
                self.answer(17, None);
            }
            cmd::JTAG_SWAP_DR => {
                if self.jtag_ir == 0x04 {
                    self.answer(7, Some((self.jtag_id as u64, 32)));
                } else {
                    self.answer(7, Some((0, 32)));
                }
            }
            cmd::SET_IO_CLIENT => {
                self.io_client = arg as u8;
                self.answer(1, None);
            }
            cmd::READ_REG => self.read_reg(arg as u8),
            cmd::WRITE_REG => self.write_reg((arg & 0xF) as u8, arg >> 4),
            cmd::READ_MEM | cmd::WRITE_MEM => self.block(t),
            _ => {}
        }
    }

    fn read_reg(&mut self, arg: u8) {
        let register = arg & 0x0F;
        let wide = arg & 0xF0 == READ_32;
        if self.io_client == 2 {
            // What the other IO client of a TC29x and TC39x reports.
            let v = if register == reg::IOINFO {
                0x1040
            } else {
                0x0260
            };
            return self.answer(11, Some((v, 16)));
        }
        if register == reg::IOINFO {
            let v = self.ioinfo;
            // Reading it ends a refused access.
            self.failed = false;
            self.ioinfo &= !IOINFO_ERROR;
            if self.settling > 0 {
                self.settling -= 1;
                if self.settling == 0 && !self.faults.locked {
                    self.ioinfo = IOINFO_READY;
                }
            }
            return self.answer(11, Some((v as u64, 16)));
        }
        if self.failed || !wide {
            return;
        }
        let (bytes, repeat) = match register {
            reg::READ32 => (4, 1),
            reg::READ16 => (2, 2),
            reg::READ8 => (1, 4),
            _ => return,
        };
        if self.is_bad(self.ioaddr, bytes) || self.ioinfo != IOINFO_READY {
            return self.refuse();
        }
        let mut data = self.peek(self.ioaddr, bytes).repeat(repeat);
        data.resize(4, 0);
        let v = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        self.answer(11, Some((v as u64, 32)));
    }

    fn write_reg(&mut self, register: u8, value: u64) {
        if self.failed {
            return;
        }
        match register {
            reg::IOCONF => {
                if std::mem::take(&mut self.faults.ignore_first_ioconf) {
                    return self.refuse();
                }
            }
            reg::IOADDR => self.ioaddr = value as u32,
            reg::OJCONF => {
                if self.ioinfo == IOINFO_AFTER_RESET {
                    // Shows 0x80 for a few reads, then ready (or stays locked).
                    self.ioinfo = IOINFO_LOCKED;
                    self.settling = 3;
                } else {
                    self.halted_after_reset = value & 0x4000 != 0;
                }
            }
            reg::WRITE32 | reg::WRITE16 | reg::WRITE8 => {
                let bytes = match register {
                    reg::WRITE32 => 4,
                    reg::WRITE16 => 2,
                    _ => 1,
                };
                if self.ioinfo != IOINFO_READY {
                    // A key word while the device is locked: taken, no effect.
                } else if self.is_bad(self.ioaddr, bytes) {
                    return self.refuse();
                } else {
                    let v = (value as u32).to_le_bytes();
                    self.poke(self.ioaddr, &v[..bytes]);
                }
            }
            _ => {}
        }
        self.answer(2, None);
    }

    fn block(&mut self, t: Telegram) {
        let Some((address, words)) = t.block_range() else {
            return;
        };
        if self.failed {
            return;
        }
        if self.ioinfo != IOINFO_READY {
            return self.refuse();
        }
        if t.cmd == cmd::WRITE_MEM {
            self.block_write = Some((address, words));
            return self.answer(2, None);
        }
        if self.is_bad(address, words * 4) {
            return self.refuse();
        }
        let data = self.peek(address, words * 4);
        // Zero bits, then each word behind a start bit with three zero bits
        // after it, then the CRC32 the same way.
        let lead = if self.early { 12 } else { 20 };
        let mut bits = Bits::zeros(lead + self.faults.delay_bits);
        for word in data.as_chunks::<4>().0 {
            bits.push(true);
            bits.push_value(u32::from_le_bytes(*word) as u64, 32);
            bits.push_value(0, 3);
        }
        bits.truncate(bits.len() - 2);
        bits.push(true);
        bits.push_value(block_crc32(&data) as u64, 32);
        let mut answer: VecDeque<bool> = bits.iter().collect();
        if self.faults.corrupt_block_reads > 0 {
            self.faults.corrupt_block_reads -= 1;
            answer[lead + 5 + self.faults.delay_bits] ^= true;
        }
        self.answer = answer;
    }
}

impl Wire for SimWire {
    fn name(&self) -> String {
        "가짜 타깃".into()
    }

    fn set_clock(&mut self, hz: u32) -> Result<u32, WireError> {
        self.clock = hz.clamp(1_000, 30_000_000);
        Ok(self.clock)
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn run(&mut self, ops: &[Op]) -> Result<Vec<Bits>, WireError> {
        let mut read = Vec::new();
        for op in ops {
            match op {
                Op::Out(bits) => {
                    self.answer.clear();
                    if self.faults.dead || self.reset_low {
                        continue;
                    }
                    if self.block_write.is_some() && bits.len() == 40 {
                        self.payload(bits);
                    } else if let Ok(t) = Telegram::parse(bits) {
                        self.block_write = None;
                        self.telegram(t);
                    }
                }
                Op::In(n) => {
                    let mut bits = Bits::new();
                    for _ in 0..*n {
                        bits.push(self.answer.pop_front().unwrap_or(false));
                    }
                    read.push(bits);
                }
                Op::Idle(_) => self.answer.clear(),
                Op::Reset(low) => {
                    if *low && !self.reset_low {
                        self.device_reset();
                    }
                    self.reset_low = *low;
                }
                Op::Sleep(_) => {}
            }
        }
        Ok(read)
    }
}
