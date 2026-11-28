//! Checks the telegram code and the link against USB captures of the
//! Infineon tools talking to real TC29x and TC39x devices through a
//! miniWiggler. The captures are public (epozzobon/tricore-things,
//! `traces/`) but not ours to copy: point `LV_TRICORE_TRACES` at that folder
//! to run these tests; without it they pass without checking anything.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use lv_dap::Bits;
use lv_dap::link::{Link, Reset, block_read_bytes};
use lv_dap::mwtrace::{self, Event};
use lv_dap::telegram::{
    Reply, Telegram, block_crc32, cmd, parse_payloads, parse_reply, write_payload,
};
use lv_dap::wire::{Op, Wire, WireError};

fn events(name: &str) -> Option<Vec<Event>> {
    let Some(folder) = std::env::var_os("LV_TRICORE_TRACES") else {
        eprintln!("LV_TRICORE_TRACES is not set: {name} not checked");
        return None;
    };
    let file = std::fs::read(PathBuf::from(folder).join(name)).expect(name);
    let packets = mwtrace::read_capture(&file).expect("capture");
    let commands = mwtrace::mpsse_commands(&packets).expect("MPSSE stream");
    Some(mwtrace::dap_events(&commands))
}

const CAPTURES: [&str; 6] = [
    "tc297b_connect_memtool.pcap",
    "tc297_unlock_and_connect.pcap",
    "tc297_lock_debug.pcap",
    "tc397b_debugging.pcapng",
    "tc397b_erasing.pcapng",
    "tc397b_flashing.pcapng",
];

#[test]
fn our_telegrams_are_the_bytes_the_tools_send() {
    let mut telegrams = 0;
    let mut payloads = 0;
    for name in CAPTURES {
        let Some(events) = events(name) else { return };
        for event in &events {
            match event {
                Event::Telegram { telegram, sent, .. } => {
                    assert_eq!(telegram.padded().as_bytes(), sent, "{name}: {telegram:?}");
                    telegrams += 1;
                }
                Event::Payload { word, sent, .. } => {
                    assert_eq!(write_payload(*word).as_bytes(), sent, "{name}: {word:#x}");
                    payloads += 1;
                }
                _ => {}
            }
        }
    }
    // Enough of both to mean something.
    assert!(telegrams > 90_000, "{telegrams} telegrams");
    assert!(payloads > 20_000, "{payloads} payloads");
}

#[test]
fn the_answers_of_real_devices_parse_and_their_checksums_hold() {
    let (mut values, mut bad_crc6, mut blocks, mut cut_short) = (0, 0, 0, 0);
    for name in CAPTURES {
        let Some(events) = events(name) else { return };
        for event in &events {
            let Event::Telegram {
                telegram, answer, ..
            } = event
            else {
                continue;
            };
            let bits = Bits::from_bytes(answer);
            if telegram.cmd == cmd::READ_MEM {
                let (_, words) = telegram.block_range().unwrap();
                // The tools clock in as much as we do.
                if !answer.is_empty() {
                    assert_eq!(answer.len(), block_read_bytes(words), "{name}");
                }
                let got = parse_payloads(&bits, words + 1);
                if got.len() == words + 1 {
                    let data: Vec<u8> = got[..words].iter().flat_map(|w| w.to_le_bytes()).collect();
                    assert_eq!(got[words], block_crc32(&data), "{name}: {telegram:?}");
                    blocks += 1;
                } else {
                    // A refused read answers nothing at all.
                    assert!(got.is_empty() || got.len() == words, "{name}: {telegram:?}");
                    cut_short += got.len().min(1);
                }
                continue;
            }
            match parse_reply(&bits, mwtrace::reply_bits(telegram)) {
                Ok(Reply::Value(_)) => values += 1,
                Ok(_) => {}
                Err(_) => bad_crc6 += 1,
            }
        }
    }
    assert!(values > 10_000, "{values} values");
    assert!(blocks > 9_000, "{blocks} block reads");
    // Two answers in all captures are damaged (both to DAPISC, at a clock
    // the tool was trying out).
    assert!(bad_crc6 <= 2, "{bad_crc6} answers with a wrong CRC6");
    assert_eq!(cut_short, 0, "block reads that stop before their CRC");
}

/// Answers each telegram with what the real device answered to the same
/// bytes, in the order the capture has them.
struct Replay {
    answers: HashMap<Vec<u8>, VecDeque<Vec<u8>>>,
    sent: Vec<(Vec<u8>, usize)>,
    clock: u32,
}

impl Replay {
    fn new(events: &[Event]) -> Replay {
        let mut answers: HashMap<Vec<u8>, VecDeque<Vec<u8>>> = HashMap::new();
        for event in events {
            if let Event::Telegram { sent, answer, .. } | Event::Payload { sent, answer, .. } =
                event
            {
                answers
                    .entry(sent.clone())
                    .or_default()
                    .push_back(answer.clone());
            }
        }
        Replay {
            answers,
            sent: Vec::new(),
            clock: 0,
        }
    }
}

impl Wire for Replay {
    fn name(&self) -> String {
        "replay".into()
    }

    fn set_clock(&mut self, hz: u32) -> Result<u32, WireError> {
        self.clock = hz;
        Ok(hz)
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn run(&mut self, ops: &[Op]) -> Result<Vec<Bits>, WireError> {
        let mut read = Vec::new();
        let mut last: Option<Vec<u8>> = None;
        for op in ops {
            match op {
                Op::Out(bits) => last = Some(bits.as_bytes().to_vec()),
                Op::In(n) => {
                    let sent = last.take().expect("an answer follows a telegram");
                    let queue = self.answers.get_mut(&sent);
                    let answer = match queue {
                        Some(q) if q.len() > 1 => q.pop_front().unwrap(),
                        Some(q) => q.front().cloned().unwrap_or_default(),
                        None => panic!("the capture has no {sent:02x?}"),
                    };
                    // We clock in exactly what the tool did.
                    assert_eq!(answer.len() * 8, *n, "answer length to {sent:02x?}");
                    self.sent.push((sent, *n));
                    read.push(Bits::from_bytes(&answer));
                }
                _ => {}
            }
        }
        Ok(read)
    }
}

#[test]
fn attaching_to_the_captured_tc297_works_as_it_did_for_the_tool() {
    let Some(events) = events("tc297b_connect_memtool.pcap") else {
        return;
    };
    let mut link = Link::new(Replay::new(&events));
    // The device left the first IOCONF write unanswered, for the tool too.
    assert_eq!(link.connect(Reset::None).unwrap(), 0x201E_9083);
    let ioconf = Telegram::write_reg(0, 12, 0xC1).padded().into_bytes();
    assert_eq!(
        link.wire()
            .sent
            .iter()
            .filter(|(s, _)| *s == ioconf)
            .count(),
        2
    );
}

#[test]
fn reset_and_halt_on_the_captured_tc397_works_as_it_did_for_the_tool() {
    let Some(events) = events("tc397b_flashing.pcapng") else {
        return;
    };
    // From where the tool pulls the reset line.
    let reset = events
        .iter()
        .position(|e| {
            matches!(
                e,
                Event::Pins {
                    high: true,
                    value: 0x55,
                    ..
                }
            )
        })
        .expect("a reset in the capture");
    let mut link = Link::new(Replay::new(&events[reset..]));
    assert_eq!(link.connect(Reset::Halt).unwrap(), 0x2020_5083);

    // The same telegrams in the same order as the tool, up to its first
    // memory access (the tool also looks at the other IO client and repeats
    // a DAPISC; we do not).
    let ours: Vec<Vec<u8>> = link.wire().sent.iter().map(|(s, _)| s.clone()).collect();
    let mut theirs = events[reset..].iter().filter_map(|e| match e {
        Event::Telegram { sent, .. } => Some(sent),
        _ => None,
    });
    let ojconf_halt = Telegram::write_reg(0xE, 16, 0x4501).padded().into_bytes();
    let end = ours.iter().position(|s| *s == ojconf_halt).expect("OJCONF") + 2;
    for sent in &ours[..end] {
        assert!(
            theirs.any(|t| t == sent),
            "the tool did not send {sent:02x?} at this point"
        );
    }
}

#[test]
fn block_transfers_go_as_the_tool_sends_them() {
    let Some(events) = events("tc397b_flashing.pcapng") else {
        return;
    };
    // The first block write of several words and the first complete block
    // read of several words in the capture.
    let write_at = events
        .iter()
        .position(|e| {
            matches!(e, Event::Telegram { telegram, .. }
                if telegram.cmd == cmd::WRITE_MEM && telegram.block_range().unwrap().1 > 1)
        })
        .expect("a block write");
    let Event::Telegram { telegram, .. } = &events[write_at] else {
        unreachable!()
    };
    let (address, words) = telegram.block_range().unwrap();
    let data: Vec<u8> = events[write_at + 1..write_at + 1 + words]
        .iter()
        .flat_map(|e| match e {
            Event::Payload { word, .. } => word.to_le_bytes(),
            other => panic!("expected a payload, got {other:?}"),
        })
        .collect();
    // What the tool sent around it: two telegrams before, one after.
    let theirs: Vec<(Vec<u8>, usize)> = events[write_at - 2..write_at + words + 2]
        .iter()
        .map(|e| match e {
            Event::Telegram { sent, answer, .. } | Event::Payload { sent, answer, .. } => {
                (sent.clone(), answer.len() * 8)
            }
            other => panic!("expected an exchange, got {other:?}"),
        })
        .collect();

    let mut link = Link::new(Replay::new(&events[write_at - 2..]));
    link.assume_connected();
    link.write_block(address, &data).unwrap();
    assert_eq!(link.wire().sent, theirs, "block write of {words} words");

    let read_at = events
        .iter()
        .position(|e| {
            matches!(e, Event::Telegram { telegram, answer, .. }
                if telegram.cmd == cmd::READ_MEM
                    && telegram.block_range().unwrap().1 > 2
                    && parse_payloads(&Bits::from_bytes(answer), 300).len() > 3)
        })
        .expect("a block read");
    let Event::Telegram {
        telegram, answer, ..
    } = &events[read_at]
    else {
        unreachable!()
    };
    let (address, words) = telegram.block_range().unwrap();
    let expected: Vec<u8> = parse_payloads(&Bits::from_bytes(answer), words)
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    let theirs: Vec<(Vec<u8>, usize)> = events[read_at - 2..=read_at]
        .iter()
        .map(|e| match e {
            Event::Telegram { sent, answer, .. } => (sent.clone(), answer.len() * 8),
            other => panic!("expected a telegram, got {other:?}"),
        })
        .collect();
    let mut link = Link::new(Replay::new(&events[read_at - 2..]));
    link.assume_connected();
    assert_eq!(link.read_block(address, words).unwrap(), expected);
    assert_eq!(link.wire().sent, theirs, "block read of {words} words");
}
