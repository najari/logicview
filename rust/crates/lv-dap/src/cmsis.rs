//! The DAP wires through a CMSIS-DAP v2 probe, such as the slot 1 firmware
//! of the WeAct Logic Analyzer V2.
//!
//! CMSIS-DAP is made for ARM targets, but its SWD pins are a clock and a
//! data line that turns around, like DAP0 and DAP1, and `DAP_SWD_Sequence`
//! clocks any bits out or in on them. SWCLK is DAP0, SWDIO is DAP1 and
//! nRESET is the reset line. The probe changes SWDIO after the falling
//! clock edge and reads it before the rising one; between sequences the
//! clock rests high.

use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{Buffer, Bulk, In, Out, TransferError};

use crate::bits::Bits;
use crate::wire::{Op, Wire, WireError};

const DAP_INFO: u8 = 0x00;
const DAP_HOST_STATUS: u8 = 0x01;
const DAP_CONNECT: u8 = 0x02;
const DAP_DISCONNECT: u8 = 0x03;
const DAP_SWJ_PINS: u8 = 0x10;
const DAP_SWJ_CLOCK: u8 = 0x11;
const DAP_SWD_SEQUENCE: u8 = 0x1D;
/// The answer to a command the firmware does not know.
const DAP_INVALID: u8 = 0xFF;

const INFO_PRODUCT: u8 = 0x02;
const INFO_FIRMWARE: u8 = 0x04;
const INFO_PACKET_SIZE: u8 = 0xFF;

const PORT_SWD: u8 = 1;
const PIN_NRESET: u8 = 0x80;
/// A sequence carries at most 64 clock cycles.
const SEQUENCE_BITS: usize = 64;
/// A sequence that reads instead of writing.
const SEQUENCE_INPUT: u8 = 0x80;

const TIMEOUT: Duration = Duration::from_secs(2);

/// Carries one command packet to the probe and its answer back.
pub trait Transport: Send {
    fn transfer(&mut self, request: &[u8]) -> Result<Vec<u8>, WireError>;
}

/// One sequence of a `DAP_SWD_Sequence` command.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sequence {
    /// Bits to drive.
    Out(Bits),
    /// Bits to read; `keep` is whether the caller wants them.
    In { bits: usize, keep: bool },
}

impl Sequence {
    fn request_len(&self) -> usize {
        match self {
            Sequence::Out(bits) => 1 + bits.len().div_ceil(8),
            Sequence::In { .. } => 1,
        }
    }

    fn response_len(&self) -> usize {
        match self {
            Sequence::Out(_) => 0,
            Sequence::In { bits, .. } => bits.div_ceil(8),
        }
    }
}

/// A CMSIS-DAP v2 probe as DAP wires.
pub struct CmsisDap<T: Transport> {
    transport: T,
    name: String,
    /// The largest command packet the probe takes.
    packet: usize,
    clock: u32,
}

impl<T: Transport> CmsisDap<T> {
    /// Sets the probe up for SWD pins: reads its packet size, connects the
    /// port and checks that it knows `DAP_SWD_Sequence`.
    pub fn new(transport: T, name: &str) -> Result<CmsisDap<T>, WireError> {
        let mut probe = CmsisDap {
            transport,
            name: name.to_string(),
            packet: 64,
            clock: 0,
        };
        let size = probe.info(INFO_PACKET_SIZE)?;
        if size.len() >= 2 {
            probe.packet = (u16::from_le_bytes([size[0], size[1]]) as usize).clamp(64, 4096);
        }
        let firmware = String::from_utf8_lossy(&probe.info(INFO_FIRMWARE)?)
            .trim_end_matches('\0')
            .to_string();
        if probe.name.is_empty() {
            probe.name = String::from_utf8_lossy(&probe.info(INFO_PRODUCT)?)
                .trim_end_matches('\0')
                .to_string();
        }
        log::debug!(
            "CMSIS-DAP: {}, protocol {firmware}, packets of {} bytes",
            probe.name,
            probe.packet
        );

        let answer = probe.command(&[DAP_CONNECT, PORT_SWD])?;
        if answer.get(1) != Some(&PORT_SWD) {
            return Err(WireError::Unsupported("SWD 포트".into()));
        }
        let _ = probe.command(&[DAP_HOST_STATUS, 0, 1]);
        // One clock cycle with the data line released: harmless on the
        // wires, and tells whether the firmware has the command at all.
        let answer = probe
            .transport
            .transfer(&[DAP_SWD_SEQUENCE, 1, SEQUENCE_INPUT | 1])?;
        if answer.first() != Some(&DAP_SWD_SEQUENCE) || answer.get(1) != Some(&0) {
            return Err(WireError::Unsupported(format!(
                "DAP_SWD_Sequence 명령 (CMSIS-DAP 1.2 이상에 있습니다. 이 프로브: {firmware})"
            )));
        }
        Ok(probe)
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Sends a command and checks that the answer is to it.
    fn command(&mut self, request: &[u8]) -> Result<Vec<u8>, WireError> {
        let answer = self.transport.transfer(request)?;
        match answer.first() {
            Some(&id) if id == request[0] => Ok(answer),
            Some(&DAP_INVALID) => Err(WireError::Unsupported(format!(
                "CMSIS-DAP 명령 {:#04x}",
                request[0]
            ))),
            _ => Err(WireError::Protocol(format!(
                "명령 {:#04x}의 응답이 {:02x?}",
                request[0],
                &answer[..answer.len().min(8)]
            ))),
        }
    }

    fn info(&mut self, id: u8) -> Result<Vec<u8>, WireError> {
        let answer = self.command(&[DAP_INFO, id])?;
        let len = *answer.get(1).unwrap_or(&0) as usize;
        Ok(answer.get(2..2 + len).unwrap_or_default().to_vec())
    }

    fn reset_line(&mut self, low: bool) -> Result<(), WireError> {
        let level = if low { 0 } else { PIN_NRESET };
        self.command(&[DAP_SWJ_PINS, level, PIN_NRESET, 0, 0, 0, 0])?;
        Ok(())
    }

    /// Sends the sequences in as few commands as the packet size allows and
    /// returns the bits of the reading sequences that are kept.
    fn sequences(&mut self, sequences: &[Sequence]) -> Result<Vec<Bits>, WireError> {
        let mut kept = Vec::new();
        let mut at = 0;
        while at < sequences.len() {
            let (mut request, mut response) = (2, 2);
            let mut end = at;
            while end < sequences.len() && end - at < 255 {
                let s = &sequences[end];
                // One byte short of the packet size, so a full packet never
                // looks like the start of a longer transfer.
                if end > at
                    && (request + s.request_len() >= self.packet
                        || response + s.response_len() >= self.packet)
                {
                    break;
                }
                request += s.request_len();
                response += s.response_len();
                end += 1;
            }
            let batch = &sequences[at..end];
            let mut packet = vec![DAP_SWD_SEQUENCE, batch.len() as u8];
            for s in batch {
                match s {
                    Sequence::Out(bits) => {
                        packet.push((bits.len() % SEQUENCE_BITS) as u8);
                        packet.extend_from_slice(bits.as_bytes());
                    }
                    Sequence::In { bits, .. } => {
                        packet.push(SEQUENCE_INPUT | (bits % SEQUENCE_BITS) as u8)
                    }
                }
            }
            let answer = self.command(&packet)?;
            if answer.get(1) != Some(&0) {
                return Err(WireError::Protocol(format!(
                    "DAP_SWD_Sequence 상태 {:02x?}",
                    answer.get(1)
                )));
            }
            let mut data = answer.get(2..).unwrap_or_default();
            for s in batch {
                if let Sequence::In { bits, keep } = s {
                    let n = bits.div_ceil(8);
                    let bytes = data.get(..n).ok_or_else(|| {
                        WireError::Protocol("DAP_SWD_Sequence 응답이 짧습니다".into())
                    })?;
                    data = &data[n..];
                    if *keep {
                        kept.push(Bits::from_bytes_len(bytes, *bits));
                    }
                }
            }
            at = end;
        }
        Ok(kept)
    }
}

/// Cuts the wire steps that clock bits into sequences of at most 64 bits.
/// Returns them with, for each [`Op::In`], how many kept sequences it took.
fn to_sequences(ops: &[Op]) -> (Vec<Sequence>, Vec<usize>) {
    let mut sequences = Vec::new();
    let mut reads = Vec::new();
    for op in ops {
        match op {
            Op::Out(bits) => {
                for start in (0..bits.len()).step_by(SEQUENCE_BITS) {
                    sequences.push(Sequence::Out(bits.slice(start, start + SEQUENCE_BITS)));
                }
            }
            Op::In(n) | Op::Idle(n) => {
                let keep = matches!(op, Op::In(_));
                let mut left = *n;
                let mut count = 0;
                while left > 0 {
                    let bits = left.min(SEQUENCE_BITS);
                    sequences.push(Sequence::In { bits, keep });
                    left -= bits;
                    count += 1;
                }
                if keep {
                    reads.push(count);
                }
            }
            Op::Reset(_) | Op::Sleep(_) => unreachable!("not a clocked step"),
        }
    }
    (sequences, reads)
}

impl<T: Transport> Wire for CmsisDap<T> {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn set_clock(&mut self, hz: u32) -> Result<u32, WireError> {
        let mut request = vec![DAP_SWJ_CLOCK];
        request.extend(hz.to_le_bytes());
        let answer = self.command(&request)?;
        if answer.get(1) != Some(&0) {
            return Err(WireError::Unsupported(format!("클럭 {hz} Hz")));
        }
        // The probe does not tell which clock it really makes.
        self.clock = hz;
        Ok(hz)
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn run(&mut self, ops: &[Op]) -> Result<Vec<Bits>, WireError> {
        let mut read = Vec::new();
        let mut at = 0;
        while at < ops.len() {
            match &ops[at] {
                Op::Reset(low) => {
                    self.reset_line(*low)?;
                    at += 1;
                }
                Op::Sleep(ms) => {
                    std::thread::sleep(Duration::from_millis(*ms as u64));
                    at += 1;
                }
                _ => {
                    let end = ops[at..]
                        .iter()
                        .position(|op| matches!(op, Op::Reset(_) | Op::Sleep(_)))
                        .map_or(ops.len(), |n| at + n);
                    let (sequences, reads) = to_sequences(&ops[at..end]);
                    let mut kept = self.sequences(&sequences)?.into_iter();
                    for count in reads {
                        let mut bits = Bits::new();
                        for part in kept.by_ref().take(count) {
                            bits.extend(&part);
                        }
                        read.push(bits);
                    }
                    at = end;
                }
            }
        }
        Ok(read)
    }
}

impl<T: Transport> Drop for CmsisDap<T> {
    fn drop(&mut self) {
        let _ = self.command(&[DAP_HOST_STATUS, 0, 0]);
        let _ = self.command(&[DAP_DISCONNECT]);
    }
}

/// A CMSIS-DAP v2 probe seen on USB.
#[derive(Debug, Clone)]
pub struct ProbeInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub product: String,
    pub serial: String,
    /// The interface with the CMSIS-DAP bulk endpoints.
    pub interface: u8,
    info: nusb::DeviceInfo,
}

impl ProbeInfo {
    pub fn name(&self) -> String {
        format!("{} {}", self.product, self.serial)
            .trim()
            .to_string()
    }
}

/// CMSIS-DAP v2 probes: devices with a vendor interface whose name has
/// "CMSIS-DAP" in it, as the CMSIS-DAP specification asks.
pub fn list_probes() -> Result<Vec<ProbeInfo>, WireError> {
    let devices = nusb::list_devices()
        .wait()
        .map_err(|e| WireError::Usb(e.to_string()))?;
    let mut probes = Vec::new();
    for info in devices {
        let interface = info.interfaces().find(|i| {
            i.class() == 0xFF
                && i.interface_string()
                    .is_some_and(|s| s.to_ascii_uppercase().contains("CMSIS-DAP"))
        });
        if let Some(interface) = interface {
            probes.push(ProbeInfo {
                vendor_id: info.vendor_id(),
                product_id: info.product_id(),
                product: info.product_string().unwrap_or("CMSIS-DAP").to_string(),
                serial: info.serial_number().unwrap_or_default().to_string(),
                interface: interface.interface_number(),
                info,
            });
        }
    }
    Ok(probes)
}

/// The probe with this serial number, or the only one there is.
pub fn find_probe(serial: Option<&str>) -> Result<ProbeInfo, WireError> {
    let mut probes = list_probes()?;
    if let Some(serial) = serial {
        probes.retain(|p| p.serial == serial);
    }
    match probes.len() {
        0 => Err(WireError::Usb(
            "CMSIS-DAP 프로브가 없습니다. WeAct 보드라면 버튼을 2초 눌러 슬롯 1(USB-시리얼 + DAP, 파란색)으로 바꾸세요"
                .into(),
        )),
        1 => Ok(probes.remove(0)),
        n => Err(WireError::Usb(format!(
            "CMSIS-DAP 프로브가 {n}개입니다. --probe <일련번호>로 고르세요: {}",
            probes
                .iter()
                .map(|p| p.serial.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The bulk endpoints of a probe.
pub struct UsbTransport {
    out: nusb::Endpoint<Bulk, Out>,
    input: nusb::Endpoint<Bulk, In>,
    _device: nusb::Device,
}

fn transfer_error(e: TransferError) -> WireError {
    match e {
        TransferError::Disconnected => WireError::Disconnected,
        TransferError::Cancelled => WireError::Timeout,
        e => WireError::Usb(e.to_string()),
    }
}

impl UsbTransport {
    pub fn open(probe: &ProbeInfo) -> Result<UsbTransport, WireError> {
        let usb = |e: nusb::Error| WireError::Usb(e.to_string());
        let device = probe.info.open().wait().map_err(usb)?;
        let interface = device
            .claim_interface(probe.interface)
            .wait()
            .map_err(usb)?;
        // CMSIS-DAP v2: the first bulk OUT endpoint takes commands, the
        // first bulk IN endpoint answers.
        let descriptor = interface
            .descriptor()
            .ok_or_else(|| WireError::Usb("인터페이스 설명자가 없습니다".into()))?;
        let bulk = |direction| {
            descriptor
                .endpoints()
                .find(|e| {
                    e.transfer_type() == nusb::descriptors::TransferType::Bulk
                        && e.direction() == direction
                })
                .map(|e| e.address())
                .ok_or_else(|| WireError::Usb("bulk 엔드포인트가 없습니다".into()))
        };
        let out = interface
            .endpoint::<Bulk, Out>(bulk(nusb::transfer::Direction::Out)?)
            .map_err(usb)?;
        let input = interface
            .endpoint::<Bulk, In>(bulk(nusb::transfer::Direction::In)?)
            .map_err(usb)?;
        Ok(UsbTransport {
            out,
            input,
            _device: device,
        })
    }
}

impl Transport for UsbTransport {
    fn transfer(&mut self, request: &[u8]) -> Result<Vec<u8>, WireError> {
        let sent = self
            .out
            .transfer_blocking(Buffer::from(request.to_vec()), TIMEOUT);
        sent.status.map_err(transfer_error)?;
        // A whole number of USB packets, large enough for any answer.
        let size = 4096usize.next_multiple_of(self.input.max_packet_size().max(1));
        let answer = self.input.transfer_blocking(Buffer::new(size), TIMEOUT);
        answer.status.map_err(transfer_error)?;
        Ok(answer.buffer[..answer.actual_len.min(answer.buffer.len())].to_vec())
    }
}

/// Opens the CMSIS-DAP probe with this serial number (or the only one).
pub fn open(serial: Option<&str>) -> Result<CmsisDap<UsbTransport>, WireError> {
    let probe = find_probe(serial)?;
    CmsisDap::new(UsbTransport::open(&probe)?, &probe.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_cut_into_sequences_of_64_bits() {
        let mut out = Bits::new();
        out.push_value(0x1234_5678_9ABC_DEF0, 64);
        out.push_value(0x5A, 8);
        let (sequences, reads) = to_sequences(&[Op::Idle(8), Op::Out(out.clone()), Op::In(130)]);
        assert_eq!(
            sequences,
            [
                Sequence::In {
                    bits: 8,
                    keep: false
                },
                Sequence::Out(out.slice(0, 64)),
                Sequence::Out(out.slice(64, 72)),
                Sequence::In {
                    bits: 64,
                    keep: true
                },
                Sequence::In {
                    bits: 64,
                    keep: true
                },
                Sequence::In {
                    bits: 2,
                    keep: true
                },
            ]
        );
        assert_eq!(reads, [3]);
        assert_eq!(sequences[1].request_len(), 9);
        assert_eq!(sequences[5].response_len(), 1);
    }
}
