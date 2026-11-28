//! Reads USB captures of an Infineon miniWiggler (USBPcap, `.pcap` or
//! `.pcapng`) down to DAP telegrams. The adapter is an FTDI chip in MPSSE
//! mode: the tool sets the DAP1 direction with a GPIO write, clocks bytes out
//! (`0x19`), turns the direction around and clocks the answer in (`0x28`).
//!
//! This is the reference the protocol code is checked against: captures of
//! the Infineon tools are public in epozzobon/tricore-things (`traces/`).

use crate::bits::Bits;
use crate::telegram::{Reply, Telegram, cmd, parse_payloads, parse_reply};

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("not a pcap or pcapng file")]
    Format,
    #[error("the capture is cut short at byte {0}")]
    Short(usize),
    #[error("MPSSE command {0:#04x} at byte {1} of the stream is not known")]
    Mpsse(u8, usize),
}

/// One USB packet of a capture: its endpoint and the data it carried.
#[derive(Debug, Clone, PartialEq)]
pub struct UsbData {
    pub seconds: f64,
    pub endpoint: u8,
    pub data: Vec<u8>,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// The data of one USBPcap packet, if it carries any.
fn usbpcap(packet: &[u8], seconds: f64) -> Option<UsbData> {
    // USBPCAP_BUFFER_PACKET_HEADER: headerLen u16, irpId u64, status u32,
    // function u16, info u8, bus u16, device u16, endpoint u8, transfer u8,
    // dataLength u32.
    if packet.len() < 27 {
        return None;
    }
    let header = u16_at(packet, 0) as usize;
    let length = u32_at(packet, 23) as usize;
    let data = packet.get(header..header + length)?;
    (!data.is_empty()).then(|| UsbData {
        seconds,
        endpoint: packet[21],
        data: data.to_vec(),
    })
}

/// The USB packets with data of a USBPcap capture file.
pub fn read_capture(file: &[u8]) -> Result<Vec<UsbData>, TraceError> {
    match file.get(..4) {
        Some([0xD4, 0xC3, 0xB2, 0xA1]) => read_pcap(file, 1e-6),
        Some([0x4D, 0x3C, 0xB2, 0xA1]) => read_pcap(file, 1e-9),
        Some([0x0A, 0x0D, 0x0D, 0x0A]) => read_pcapng(file),
        _ => Err(TraceError::Format),
    }
}

fn read_pcap(file: &[u8], fraction: f64) -> Result<Vec<UsbData>, TraceError> {
    let mut out = Vec::new();
    let mut at = 24;
    while at < file.len() {
        let header = file.get(at..at + 16).ok_or(TraceError::Short(at))?;
        let seconds = u32_at(header, 0) as f64 + u32_at(header, 4) as f64 * fraction;
        let length = u32_at(header, 8) as usize;
        let packet = file
            .get(at + 16..at + 16 + length)
            .ok_or(TraceError::Short(at))?;
        out.extend(usbpcap(packet, seconds));
        at += 16 + length;
    }
    Ok(out)
}

fn read_pcapng(file: &[u8]) -> Result<Vec<UsbData>, TraceError> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < file.len() {
        let header = file.get(at..at + 8).ok_or(TraceError::Short(at))?;
        let kind = u32_at(header, 0);
        let total = u32_at(header, 4) as usize;
        let block = file
            .get(at..at + total)
            .filter(|b| b.len() >= 12)
            .ok_or(TraceError::Short(at))?;
        match kind {
            // Enhanced packet block: interface, timestamp (microseconds by
            // default), captured length, original length, data.
            6 if block.len() >= 28 => {
                let ticks = (u32_at(block, 12) as u64) << 32 | u32_at(block, 16) as u64;
                let length = u32_at(block, 20) as usize;
                let packet = block.get(28..28 + length).ok_or(TraceError::Short(at))?;
                out.extend(usbpcap(packet, ticks as f64 * 1e-6));
            }
            // Simple packet block: original length, data.
            3 if block.len() >= 12 => {
                let length = (u32_at(block, 8) as usize).min(block.len() - 16);
                out.extend(usbpcap(&block[12..12 + length], 0.0));
            }
            _ => {}
        }
        at += total.max(12);
    }
    Ok(out)
}

/// One MPSSE command with what the chip answered.
#[derive(Debug, Clone, PartialEq)]
pub enum Mpsse {
    /// `0x19`: bytes out on DAP1, LSB first, changing on the falling edge.
    ClockOut(Vec<u8>),
    /// `0x1B`: 1 to 8 bits out.
    ClockOutBits(u8, u8),
    /// `0x28`: bytes in, LSB first, taken on the rising edge.
    ClockIn(Vec<u8>),
    /// `0x80`, `0x82`: value and direction of the low or the high pins.
    SetLow(u8, u8),
    SetHigh(u8, u8),
    /// `0x81`, `0x83`.
    GetLow(u8),
    GetHigh(u8),
    /// `0x86`: the clock is the base clock / (divisor + 1) / 2.
    Divisor(u16),
    /// `0x87`: send the answers now.
    Flush(f64),
    Other(u8, Vec<u8>),
}

/// Pairs the commands written to the chip (endpoint `0x02`) with the bytes
/// it sent back (endpoint `0x81`, two status bytes in front of each packet).
pub fn mpsse_commands(packets: &[UsbData]) -> Result<Vec<Mpsse>, TraceError> {
    let mut out_stream = Vec::new();
    let mut in_stream = Vec::new();
    // Where in the out stream each packet ended, to date the flushes.
    let mut out_times = Vec::new();
    for p in packets {
        match p.endpoint {
            0x02 => {
                out_stream.extend_from_slice(&p.data);
                out_times.push((out_stream.len(), p.seconds));
            }
            0x81 => {
                // The chip sends packets of 512 bytes at high speed (64 at
                // full speed), each with its own two status bytes.
                for chunk in p.data.chunks(in_packet_size(&p.data)) {
                    in_stream.extend_from_slice(chunk.get(2..).unwrap_or_default());
                }
            }
            _ => {}
        }
    }
    let time_of = |at: usize| {
        out_times
            .iter()
            .find(|(end, _)| *end > at)
            .map_or(0.0, |(_, s)| *s)
    };

    let mut commands = Vec::new();
    let (mut o, mut i) = (0, 0);
    let take_in = |n: usize, i: &mut usize| -> Vec<u8> {
        let got = in_stream.get(*i..*i + n).unwrap_or_default().to_vec();
        *i += got.len();
        got
    };
    while o < out_stream.len() {
        let op = out_stream[o];
        let arg = |n: usize| out_stream.get(o + 1..o + 1 + n).ok_or(TraceError::Short(o));
        let (command, used) = match op {
            0x19 => {
                let n = u16_at(arg(2)?, 0) as usize + 1;
                (Mpsse::ClockOut(arg(2 + n)?[2..].to_vec()), 3 + n)
            }
            0x1B => (Mpsse::ClockOutBits(arg(2)?[0] + 1, arg(2)?[1]), 3),
            0x28 => {
                let n = u16_at(arg(2)?, 0) as usize + 1;
                (Mpsse::ClockIn(take_in(n, &mut i)), 3)
            }
            0x80 => (Mpsse::SetLow(arg(2)?[0], arg(2)?[1]), 3),
            0x82 => (Mpsse::SetHigh(arg(2)?[0], arg(2)?[1]), 3),
            0x81 => (
                Mpsse::GetLow(take_in(1, &mut i).first().copied().unwrap_or(0)),
                1,
            ),
            0x83 => (
                Mpsse::GetHigh(take_in(1, &mut i).first().copied().unwrap_or(0)),
                1,
            ),
            0x86 => (Mpsse::Divisor(u16_at(arg(2)?, 0)), 3),
            0x87 => (Mpsse::Flush(time_of(o)), 1),
            // Loopback, clock divide by 5, 3-phase and adaptive clocking.
            0x84 | 0x85 | 0x8A | 0x8B | 0x8C | 0x8D | 0x96 | 0x97 => (Mpsse::Other(op, vec![]), 1),
            // Bad commands the tool sends to see the chip answer `FA xx`.
            0xAA | 0xAB => (Mpsse::Other(op, take_in(2, &mut i)), 1),
            other => return Err(TraceError::Mpsse(other, o)),
        };
        commands.push(command);
        o += used;
    }
    Ok(commands)
}

/// The USB packet size of an IN transfer: 512 bytes if the status bytes
/// repeat there, else the whole transfer is one packet.
fn in_packet_size(data: &[u8]) -> usize {
    for size in [64, 512] {
        if data.len() > size
            && data
                .chunks(size)
                .all(|c| c.len() < 2 || (c[0] == data[0] && c[1] == data[1]))
        {
            // Prefer the larger size when both fit (64 divides 512).
            if size == 64 && data.len() > 512 && data.chunks(512).all(|c| c[0] == data[0]) {
                continue;
            }
            return size;
        }
    }
    data.len().max(1)
}

/// The pin values of the miniWiggler that turn DAP1 to the device and back.
const DRIVE_OUT: (u8, u8) = (0x67, 0xF7);
const DRIVE_IN: (u8, u8) = (0x57, 0xF7);

/// What a capture shows at the DAP level.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A telegram, the raw bytes clocked in after it, and the bytes it was
    /// clocked out as.
    Telegram {
        telegram: Telegram,
        sent: Vec<u8>,
        answer: Vec<u8>,
    },
    /// One word of a block write.
    Payload {
        word: u32,
        sent: Vec<u8>,
        answer: Vec<u8>,
    },
    /// Bytes clocked out that are no telegram.
    Raw {
        sent: Vec<u8>,
        answer: Vec<u8>,
    },
    /// Clock set to this many Hz (30 MHz base).
    Clock(u32),
    Pins {
        high: bool,
        value: u8,
        direction: u8,
    },
    Flush(f64),
}

/// Folds the MPSSE commands into DAP exchanges.
pub fn dap_events(commands: &[Mpsse]) -> Vec<Event> {
    let mut events = Vec::new();
    let mut payloads_left = 0usize;
    let mut i = 0;
    while i < commands.len() {
        let exchange = match commands.get(i..i + 4) {
            Some(
                [
                    Mpsse::SetHigh(v0, d0),
                    Mpsse::ClockOut(sent),
                    Mpsse::SetHigh(v1, d1),
                    Mpsse::ClockIn(answer),
                ],
            ) if (*v0, *d0) == DRIVE_OUT && (*v1, *d1) == DRIVE_IN => Some((sent, answer)),
            _ => None,
        };
        if let Some((sent, answer)) = exchange {
            i += 4;
            let bits = Bits::from_bytes(sent);
            if payloads_left > 0 && sent.len() == 5 {
                payloads_left -= 1;
                let start = bits.find_one(0).unwrap_or(0);
                events.push(Event::Payload {
                    word: bits.value(start + 1, 32) as u32,
                    sent: sent.clone(),
                    answer: answer.clone(),
                });
                continue;
            }
            match Telegram::parse(&bits) {
                Ok(telegram) => {
                    if telegram.cmd == cmd::WRITE_MEM {
                        payloads_left = telegram.block_range().map_or(0, |(_, words)| words);
                    }
                    events.push(Event::Telegram {
                        telegram,
                        sent: sent.clone(),
                        answer: answer.clone(),
                    });
                }
                Err(_) => events.push(Event::Raw {
                    sent: sent.clone(),
                    answer: answer.clone(),
                }),
            }
            continue;
        }
        match &commands[i] {
            Mpsse::Divisor(d) => events.push(Event::Clock(30_000_000 / (*d as u32 + 1))),
            Mpsse::SetHigh(value, direction) => events.push(Event::Pins {
                high: true,
                value: *value,
                direction: *direction,
            }),
            Mpsse::SetLow(value, direction) => events.push(Event::Pins {
                high: false,
                value: *value,
                direction: *direction,
            }),
            Mpsse::Flush(t) => events.push(Event::Flush(*t)),
            Mpsse::ClockOut(sent) => events.push(Event::Raw {
                sent: sent.clone(),
                answer: vec![],
            }),
            Mpsse::ClockIn(answer) => events.push(Event::Raw {
                sent: vec![],
                answer: answer.clone(),
            }),
            _ => {}
        }
        i += 1;
    }
    events
}

/// The value bits a telegram's answer carries.
pub fn reply_bits(telegram: &Telegram) -> usize {
    match (telegram.cmd, telegram.arg) {
        (cmd::READ_REG, Some((_, arg))) if arg as u8 & 0xF0 == crate::telegram::READ_32 => 32,
        (cmd::READ_REG, _) => 16,
        (cmd::DAPISC, _) => 16,
        (cmd::SYNC | cmd::JTAG_SWAP_DR, _) => 32,
        _ => 0,
    }
}

/// One line of text for an event, for `lvcli aurix trace`.
pub fn describe(event: &Event) -> String {
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    match event {
        Event::Telegram {
            telegram, answer, ..
        } => {
            let bits = Bits::from_bytes(answer);
            let what = match (telegram.cmd, telegram.arg) {
                (cmd::WRITE_REG, Some((len, arg))) => {
                    format!(
                        "write reg {:#x} <- {:#x} ({} bits)",
                        arg & 0xF,
                        arg >> 4,
                        len - 4
                    )
                }
                (cmd::READ_REG, Some((_, arg))) => {
                    format!("read reg {:#x} ({} bits)", arg & 0xF, reply_bits(telegram))
                }
                (cmd::SET_IO_CLIENT, Some((_, arg))) => format!("io client {arg}"),
                (cmd::DAPISC, Some((len, arg))) => format!("dapisc {len} bits {arg:#x}"),
                (cmd::SYNC, _) => "sync".into(),
                (cmd::JTAG_SET_IR, Some((_, arg))) => format!("jtag ir {arg:#04x}"),
                (cmd::JTAG_SWAP_DR, Some((len, arg))) => format!("jtag dr {len} bits {arg:#x}"),
                (cmd::JTAG_RESET, _) => "jtag reset".into(),
                (cmd::READ_MEM | cmd::WRITE_MEM, Some((_, arg))) => {
                    let (address, words) = telegram.block_range().unwrap_or((0, 0));
                    format!(
                        "{} block {address:#010x} {words} words flags {}",
                        if telegram.cmd == cmd::READ_MEM {
                            "read"
                        } else {
                            "write"
                        },
                        arg & 3
                    )
                }
                (c, Some((len, arg))) => format!("cmd {c} {len} bits {arg:#x}"),
                (c, None) => format!("cmd {c}"),
            };
            let answer = if telegram.cmd == cmd::READ_MEM {
                let words = telegram.block_range().map_or(0, |(_, w)| w);
                let got = parse_payloads(&bits, words + 1);
                format!(
                    "{} words{}",
                    got.len().min(words),
                    if got.len() > words { " + crc" } else { "" }
                )
            } else {
                match parse_reply(&bits, reply_bits(telegram)) {
                    Ok(Reply::Silent) => "silent".into(),
                    Ok(Reply::Ack) => format!("ack at bit {}", bits.find_one(0).unwrap_or(0)),
                    Ok(Reply::Value(v)) => {
                        format!("{v:#x} at bit {}", bits.find_one(0).unwrap_or(0))
                    }
                    Err(e) => format!("{e} ({})", hex(answer)),
                }
            };
            format!("{what} -> {answer} [{} bytes in]", bits.len() / 8)
        }
        Event::Payload { word, answer, .. } => format!("  payload {word:#010x} -> {}", hex(answer)),
        Event::Raw { sent, answer } => format!("raw out {} in {}", hex(sent), hex(answer)),
        Event::Clock(hz) => format!("clock {hz} Hz"),
        Event::Pins {
            high,
            value,
            direction,
        } => format!(
            "pins {} value {value:#04x} direction {direction:#04x}",
            if *high { "high" } else { "low" }
        ),
        Event::Flush(t) => format!("-- flush at {t:.6} s"),
    }
}
