//! The link against the pretend device.

use lv_dap::link::{Link, LinkError, Reset, Target, block_read_bytes, split_access};
use lv_dap::sim::{Faults, SIM_JTAG_ID, SimWire};
use lv_dap::telegram::{Telegram, cmd, reg};
use lv_dap::wire::Wire;

fn connected(faults: Faults) -> Link<SimWire> {
    let mut link = Link::new(SimWire::with_faults(faults));
    assert_eq!(link.connect(Reset::None).unwrap(), SIM_JTAG_ID);
    link
}

#[test]
fn attaching_reads_the_jtag_id_and_sets_the_clock() {
    let mut link = Link::new(SimWire::new());
    assert!(matches!(
        link.read32(0x7000_0000),
        Err(LinkError::NotConnected)
    ));
    assert_eq!(link.connect(Reset::None).unwrap(), 0x2020_5083);
    assert_eq!(link.speed(), 5_000_000);
    assert_eq!(link.wire().resets, 0);
    assert_eq!(Target::jtag_id(&link), Some(SIM_JTAG_ID));
    // Sync, port setup and JTAG reset come first, as the Infineon tools send them.
    let log = &link.wire().log;
    assert_eq!(log[0], Telegram::bare(cmd::SYNC));
    assert_eq!(log[1], Telegram::new(cmd::DAPISC, 48, 0x4ABB_AF53_0F00));
    assert_eq!(log[2], Telegram::bare(cmd::JTAG_RESET));
}

#[test]
fn an_unanswered_first_preamble_is_tried_again() {
    let link = connected(Faults {
        ignore_first_ioconf: true,
        ..Faults::default()
    });
    let ioconf = link
        .wire()
        .log
        .iter()
        .filter(|t| **t == Telegram::write_reg(reg::IOCONF, 12, 0xC1))
        .count();
    assert_eq!(ioconf, 2);
}

#[test]
fn nothing_on_the_wires_is_reported_as_such() {
    let mut link = Link::new(SimWire::with_faults(Faults {
        dead: true,
        ..Faults::default()
    }));
    let error = link.connect(Reset::None).unwrap_err();
    assert!(matches!(error, LinkError::NoAnswer("SYNC")), "{error}");
    assert!(error.to_string().contains("배선"));
    assert!(matches!(
        link.connect(Reset::Halt),
        Err(LinkError::NoAnswer(_))
    ));
}

#[test]
fn reset_and_halt_asks_for_halted_cores() {
    let mut link = Link::new(SimWire::new());
    link.wire_mut().poke(0x7000_0000, &[1, 2, 3, 4]);
    assert_eq!(link.connect(Reset::Halt).unwrap(), SIM_JTAG_ID);
    assert_eq!(link.wire().resets, 1);
    assert!(link.wire().halted_after_reset);
    assert_eq!(link.read32(0x7000_0000).unwrap(), 0x0403_0201);

    assert_eq!(link.connect(Reset::Run).unwrap(), SIM_JTAG_ID);
    assert_eq!(link.wire().resets, 2);
    assert!(!link.wire().halted_after_reset);
}

#[test]
fn a_locked_device_is_reported() {
    let mut link = Link::new(SimWire::with_faults(Faults {
        locked: true,
        ..Faults::default()
    }));
    assert!(matches!(
        link.connect(Reset::Halt),
        Err(LinkError::Locked(0x80))
    ));
    assert!(!link.is_connected());
}

#[test]
fn words_bytes_and_half_words_round_trip() {
    let mut link = connected(Faults::default());
    link.write32(0x7000_0010, 0xCAFE_BABE).unwrap();
    assert_eq!(link.read32(0x7000_0010).unwrap(), 0xCAFE_BABE);
    assert_eq!(link.read8(0x7000_0011).unwrap(), 0xBA);
    assert_eq!(link.read16(0x7000_0012).unwrap(), 0xCAFE);
    link.write8(0x7000_0013, 0x12).unwrap();
    link.write16(0x7000_0010, 0x3456).unwrap();
    assert_eq!(link.read32(0x7000_0010).unwrap(), 0x12FE_3456);
    assert_eq!(link.wire().peek(0x7000_0010, 4), [0x56, 0x34, 0xFE, 0x12]);
    // A word that reads as zero still has an answer with a CRC.
    assert_eq!(link.read32(0x7000_0100).unwrap(), 0);
    assert_eq!(link.read8(0x7000_0100).unwrap(), 0);
}

#[test]
fn blocks_of_every_size_round_trip() {
    let mut link = connected(Faults {
        delay_bits: 5,
        ..Faults::default()
    });
    for words in [1usize, 2, 3, 4, 8, 16, 36, 102, 255, 256] {
        let data: Vec<u8> = (0..words * 4).map(|i| (i * 7 + words) as u8).collect();
        link.write_block(0xD000_0000, &data).unwrap();
        assert_eq!(
            link.read_block(0xD000_0000, words).unwrap(),
            data,
            "{words} words"
        );
    }
}

#[test]
fn any_address_and_length_is_split() {
    assert_eq!(split_access(0x1000, 4), [(0, 4)]);
    assert_eq!(split_access(0x1001, 1), [(0, 1)]);
    assert_eq!(split_access(0x1002, 2), [(0, 2)]);
    assert_eq!(split_access(0x1001, 9), [(0, 1), (1, 2), (3, 4), (7, 2)]);
    assert_eq!(
        split_access(0x1000, 1030),
        [(0, 1024), (1024, 4), (1028, 2)]
    );
    assert_eq!(split_access(0x1003, 2), [(0, 1), (1, 1)]);

    let mut link = connected(Faults::default());
    let data: Vec<u8> = (0..2051u32).map(|i| (i ^ (i >> 8)) as u8).collect();
    Target::write(&mut link, 0x7000_0001, &data).unwrap();
    assert_eq!(link.wire().peek(0x7000_0001, data.len()), data);
    let mut back = vec![0; data.len()];
    Target::read(&mut link, 0x7000_0001, &mut back).unwrap();
    assert_eq!(back, data);
}

#[test]
fn a_refused_access_fails_alone() {
    let mut link = connected(Faults {
        bad: vec![0..0x1000, 0xA000_0000..0xA000_0100],
        ..Faults::default()
    });
    link.write32(0x7000_0000, 0x1122_3344).unwrap();
    let attempts: [fn(&mut Link<SimWire>) -> LinkError; 6] = [
        |l| l.read32(0).unwrap_err(),
        |l| l.read_block(0xA000_0000, 16).unwrap_err(),
        |l| l.read8(3).unwrap_err(),
        |l| l.write32(0, 1).unwrap_err(),
        |l| l.write16(2, 1).unwrap_err(),
        // The second word of the block is refused.
        |l| l.write_block(0x9FFF_FFFC, &[0; 8]).unwrap_err(),
    ];
    for attempt in attempts {
        let error = attempt(&mut link);
        assert!(error.is_access(), "{error}");
        assert!(
            matches!(error, LinkError::Access { ioinfo: Some(v), .. } if v & 0x20 != 0),
            "{error}"
        );
        // The link works again right away.
        assert_eq!(link.read32(0x7000_0000).unwrap(), 0x1122_3344);
    }
    let text = link.read32(0).unwrap_err().to_string();
    assert!(text.contains("0x00000000 읽기 실패"), "{text}");
}

#[test]
fn a_damaged_block_read_is_read_again() {
    let mut link = connected(Faults {
        corrupt_block_reads: 1,
        ..Faults::default()
    });
    link.wire_mut().poke(0x7000_0000, &[9, 8, 7, 6, 5, 4, 3, 2]);
    assert_eq!(
        link.read_block(0x7000_0000, 2).unwrap(),
        [9, 8, 7, 6, 5, 4, 3, 2]
    );

    link.wire_mut().faults.corrupt_block_reads = 2;
    assert!(matches!(
        link.read_block(0x7000_0000, 2),
        Err(LinkError::BlockCrc(0x7000_0000))
    ));
    assert_eq!(link.read32(0x7000_0004).unwrap(), 0x0203_0405);
}

#[test]
fn the_speed_is_kept_across_connects() {
    let mut link = Link::new(SimWire::new());
    assert_eq!(link.set_speed(2_000_000).unwrap(), 2_000_000);
    link.connect(Reset::None).unwrap();
    assert_eq!(link.wire().clock(), 2_000_000);
    assert_eq!(
        Target::set_clock_hz(&mut link, 100_000_000).unwrap(),
        30_000_000
    );
    assert_eq!(Target::clock_hz(&link), 30_000_000);
}

#[test]
fn block_reads_clock_in_what_the_miniwiggler_does() {
    // Sizes seen in captures of the Infineon tools.
    for (words, bytes) in [
        (1, 16),
        (2, 21),
        (3, 26),
        (4, 31),
        (8, 50),
        (16, 88),
        (36, 183),
        (92, 449),
        (102, 496),
        (164, 791),
        (256, 1228),
    ] {
        assert_eq!(block_read_bytes(words), bytes, "{words} words");
    }
}
