//! The CMSIS-DAP wire against a pretend probe firmware that drives the
//! pretend device: link -> CMSIS-DAP commands -> SWD sequences -> telegrams.

use lv_dap::Bits;
use lv_dap::cmsis::{CmsisDap, Transport};
use lv_dap::link::{Link, LinkError, Reset, Target};
use lv_dap::sim::{Faults, SIM_JTAG_ID, SimWire};
use lv_dap::wire::{Op, Wire, WireError};

/// What a CMSIS-DAP v2 firmware does with the commands we use.
struct FakeProbe {
    device: SimWire,
    packet: usize,
    /// A firmware older than CMSIS-DAP 1.2.
    no_swd_sequence: bool,
    /// Bits driven since the data line last turned around.
    driven: Bits,
    connected: bool,
    clock: u32,
    requests: Vec<Vec<u8>>,
    largest_answer: usize,
}

impl FakeProbe {
    fn new(packet: usize) -> FakeProbe {
        FakeProbe {
            device: SimWire::new(),
            packet,
            no_swd_sequence: false,
            driven: Bits::new(),
            connected: false,
            clock: 0,
            requests: Vec::new(),
            largest_answer: 0,
        }
    }

    /// The device sees a telegram when the data line turns around.
    fn turn_around(&mut self) {
        if !self.driven.is_empty() {
            let bits = std::mem::take(&mut self.driven);
            self.device.run(&[Op::Out(bits)]).unwrap();
        }
    }
}

impl Transport for FakeProbe {
    fn transfer(&mut self, request: &[u8]) -> Result<Vec<u8>, WireError> {
        assert!(
            request.len() < self.packet,
            "request of {} bytes",
            request.len()
        );
        self.requests.push(request.to_vec());
        let answer = match request[0] {
            0x00 => match request[1] {
                0xFF => vec![0x00, 2, self.packet as u8, (self.packet >> 8) as u8],
                0x04 => vec![0x00, 6, b'2', b'.', b'1', b'.', b'0', 0],
                _ => vec![0x00, 0],
            },
            0x01 => vec![0x01, 0],
            0x02 => {
                self.connected = request[1] == 1;
                vec![0x02, request[1]]
            }
            0x03 => vec![0x03, 0],
            0x10 => {
                self.turn_around();
                if request[2] & 0x80 != 0 {
                    let low = request[1] & 0x80 == 0;
                    self.device.run(&[Op::Reset(low)]).unwrap();
                }
                vec![0x10, request[1]]
            }
            0x11 => {
                self.clock = u32::from_le_bytes([request[1], request[2], request[3], request[4]]);
                vec![0x11, 0]
            }
            0x1D if !self.no_swd_sequence => {
                assert!(self.connected);
                let mut answer = vec![0x1D, 0];
                let mut at = 2;
                for _ in 0..request[1] {
                    let info = request[at];
                    at += 1;
                    let bits = match (info & 0x3F) as usize {
                        0 => 64,
                        n => n,
                    };
                    if info & 0x80 != 0 {
                        self.turn_around();
                        let read = self.device.run(&[Op::In(bits)]).unwrap();
                        answer.extend_from_slice(read[0].as_bytes());
                    } else {
                        let bytes = bits.div_ceil(8);
                        self.driven
                            .extend(&Bits::from_bytes_len(&request[at..at + bytes], bits));
                        at += bytes;
                    }
                }
                assert_eq!(at, request.len());
                answer
            }
            _ => vec![0xFF],
        };
        assert!(
            answer.len() <= self.packet,
            "answer of {} bytes",
            answer.len()
        );
        self.largest_answer = self.largest_answer.max(answer.len());
        Ok(answer)
    }
}

fn link(packet: usize) -> Link<CmsisDap<FakeProbe>> {
    Link::new(CmsisDap::new(FakeProbe::new(packet), "fake").unwrap())
}

#[test]
fn the_link_works_through_a_cmsis_dap_probe() {
    for packet in [64, 512, 1024] {
        let mut link = link(packet);
        assert_eq!(link.connect(Reset::None).unwrap(), SIM_JTAG_ID);
        assert_eq!(link.wire().transport().clock, 5_000_000);

        let data: Vec<u8> = (0..1024u32).map(|i| ((i * 13) >> 2) as u8).collect();
        Target::write(&mut link, 0x7000_0000, &data).unwrap();
        let mut back = vec![0; data.len()];
        Target::read(&mut link, 0x7000_0000, &mut back).unwrap();
        assert_eq!(back, data, "packets of {packet} bytes");
        assert_eq!(link.read8(0x7000_0003).unwrap(), data[3]);
        link.write16(0x7000_0400, 0xBEEF).unwrap();
        assert_eq!(link.read16(0x7000_0400).unwrap(), 0xBEEF);
        // Large transfers fill the packets.
        assert!(link.wire().transport().largest_answer > packet - 10);
    }
}

#[test]
fn reset_goes_through_the_reset_pin() {
    let mut link = link(512);
    assert_eq!(link.connect(Reset::Halt).unwrap(), SIM_JTAG_ID);
    let probe = link.wire().transport();
    assert_eq!(probe.device.resets, 1);
    assert!(probe.device.halted_after_reset);
    // nRESET low, then high: only that pin is selected.
    let pins: Vec<&Vec<u8>> = probe.requests.iter().filter(|r| r[0] == 0x10).collect();
    assert_eq!(pins.len(), 2);
    assert_eq!(pins[0][..3], [0x10, 0x00, 0x80]);
    assert_eq!(pins[1][..3], [0x10, 0x80, 0x80]);
    link.write32(0x7000_0000, 5).unwrap();
    assert_eq!(link.read32(0x7000_0000).unwrap(), 5);
}

#[test]
fn a_refused_access_and_a_silent_device_are_told_apart() {
    let mut probe = FakeProbe::new(512);
    probe.device = SimWire::with_faults(Faults {
        bad: vec![0..8, 8..16],
        ..Faults::default()
    });
    let mut link = Link::new(CmsisDap::new(probe, "fake").unwrap());
    link.connect(Reset::None).unwrap();
    assert!(link.read32(0).unwrap_err().is_access());
    assert_eq!(link.read32(0x100).unwrap(), 0);

    let mut probe = FakeProbe::new(512);
    probe.device = SimWire::with_faults(Faults {
        dead: true,
        ..Faults::default()
    });
    let mut link = Link::new(CmsisDap::new(probe, "fake").unwrap());
    assert!(matches!(
        link.connect(Reset::None),
        Err(LinkError::NoAnswer("SYNC"))
    ));
}

#[test]
fn a_firmware_without_swd_sequences_is_refused() {
    let mut probe = FakeProbe::new(64);
    probe.no_swd_sequence = true;
    let error = CmsisDap::new(probe, "old").err().expect("refused");
    assert!(matches!(error, WireError::Unsupported(_)), "{error}");
    assert!(error.to_string().contains("DAP_SWD_Sequence"));
}
