//! The TAS wire format, server side.
//!
//! - PL2: every message is `[u32 LE size, itself included][payload]`.
//! - PL1: the payload is a sequence of elements `[wl][cmd][...]` of
//!   `(wl + 1) * 4` bytes. Control messages are one element.
//! - PL0: a memory access frame is `START`, context and transfer elements,
//!   `END`.
//!
//! The layouts follow Infineon's reference client (`tas_client_api`,
//! Apache-2.0) and captures of TasServer V2.0 (aurix_debugger,
//! `docs/TAS_PROTOCOL_CAPTURE.md`).

pub const DEFAULT_PORT: u16 = 24817;

pub const CMD_SERVER_CONNECT: u8 = 0x80;
pub const CMD_GET_TARGETS: u8 = 0x86;
pub const CMD_SESSION_START: u8 = 0xA1;
pub const CMD_PING: u8 = 0xA2;
pub const CMD_DEVICE_CONNECT: u8 = 0xA8;
pub const CMD_SET_DEVICE_KEY: u8 = 0xAD;
pub const CMD_PL0_START: u8 = 0xB0;
pub const CMD_PL0_END: u8 = 0xB1;

pub const PL0_ACCESS_MODE: u8 = 0x10;
pub const PL0_ADDR_MAP: u8 = 0x11;
pub const PL0_BASE_ADDR32: u8 = 0x15;
pub const PL0_WR8: u8 = 0x20;
pub const PL0_RD8: u8 = 0x21;
pub const PL0_WR16: u8 = 0x22;
pub const PL0_RD16: u8 = 0x23;
pub const PL0_WR32: u8 = 0x24;
pub const PL0_RD32: u8 = 0x25;
pub const PL0_WR64: u8 = 0x26;
pub const PL0_RD64: u8 = 0x27;
pub const PL0_WRBLK: u8 = 0x2A;
pub const PL0_RDBLK: u8 = 0x2C;
pub const PL0_RDBLK1KB: u8 = 0x2D;

/// PL1 error codes.
pub const PL1_OK: u8 = 0x08;
/// A parameter the server does not take.
pub const PL1_ERR_PARAM: u8 = 0x20;
/// The device was reset since this session's last request.
pub const PL1_ERR_DEVICE_RESET: u8 = 0x44;
/// The device could not be connected.
pub const PL1_ERR_DEVICE_ACCESS: u8 = 0x46;

/// PL0 (per transfer) codes.
pub const PL0_OK: u8 = 0x80;
/// The device refused the access.
pub const PL0_ERR_DATA: u8 = 0x8D;
/// Not run, because a transfer before it failed.
pub const PL0_ERR_CONSEQUENTIAL: u8 = 0x98;

/// `DEVICE_CONNECT` options.
pub const CONNECT_HOT_ATTACH: u16 = 0x0000;
pub const CONNECT_RESET: u16 = 0x0001;
pub const CONNECT_RESET_AND_HALT: u16 = 0x0003;

/// Address maps.
pub const MAP_DEVICE: u8 = 0;
pub const MAP_SERVER: u8 = 15;

/// Server register (map 15): the clock of the access hardware in Hz.
pub const ACC_HW_FREQUENCY: u32 = 0x000C_0100;
/// Text blocks of map 15 that tools show: server, access hardware, device.
pub const SERVER_INFO_BLOCK: u32 = 0x0000;
pub const HARDWARE_INFO_BLOCK: u32 = 0x8000;
pub const DEVICE_INFO_BLOCK: u32 = 0xD000;

/// The message sizes TasServer reports, and so do we.
pub const MAX_PL2_REQUEST: u32 = 1060;
pub const MAX_PL2_RESPONSE: u32 = 1040;
pub const MAX_TRANSFERS: u8 = 32;
/// Reset with halt after reset, and unlocking.
const DEVICE_CONNECT_FEATURES: u16 = 0x0013;
/// How the device is reached; TasServer reports 1 for JTAG.
const PHYSICAL_DAP: u8 = 2;

fn put_str(buf: &mut [u8], s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(buf.len().saturating_sub(1));
    buf[..n].copy_from_slice(&bytes[..n]);
}

pub fn c_string(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// A one-element answer without data.
pub fn status(cmd: u8, con_id: u8, error: u8) -> Vec<u8> {
    vec![0x00, cmd, con_id, error]
}

/// `SERVER_CONNECT` answer: the server information and a zero challenge.
pub fn server_connect(name: &str, start_time_us: u64) -> Vec<u8> {
    let mut r = vec![0x21, CMD_SERVER_CONNECT, 0x00, PL1_OK, 0, 0, 0, 0];
    let mut info = [0u8; 120];
    put_str(&mut info[0..64], name);
    info[66..68].copy_from_slice(&2u16.to_le_bytes()); // major version
    info[68..72].copy_from_slice(&2u32.to_le_bytes()); // protocol version
    put_str(&mut info[96..112], env!("CARGO_PKG_VERSION"));
    info[112..120].copy_from_slice(&start_time_us.to_le_bytes());
    r.extend(info);
    r.extend([0u8; 8]);
    r
}

/// What the answers say about the one target of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetInfo {
    pub identifier: String,
    /// The JTAG ID, 0 while unknown.
    pub device_type: u32,
    pub clients: u8,
}

/// `GET_TARGETS` answer.
pub fn get_targets(target: Option<&TargetInfo>) -> Vec<u8> {
    let n = target.is_some() as u8;
    let mut r = vec![
        if n == 0 { 0x01 } else { 0x17 },
        CMD_GET_TARGETS,
        0x00,
        PL1_OK,
        n,
        0,
        n,
        0,
    ];
    if let Some(t) = target {
        let mut info = [0u8; 88];
        put_str(&mut info[0..64], &t.identifier);
        info[64..68].copy_from_slice(&t.device_type.to_le_bytes());
        info[84] = PHYSICAL_DAP;
        info[85] = t.clients;
        r.extend(info);
    }
    r
}

/// `SESSION_START` and `PING` answer: the connection information.
pub fn con_info(cmd: u8, target: &TargetInfo) -> Vec<u8> {
    let mut r = vec![0x20, cmd, 0x00, PL1_OK, 1, 1, 0, 0];
    let mut ci = [0u8; 124];
    ci[0..4].copy_from_slice(&MAX_PL2_REQUEST.to_le_bytes());
    ci[4..8].copy_from_slice(&MAX_PL2_RESPONSE.to_le_bytes());
    ci[8..12].copy_from_slice(&target.device_type.to_le_bytes());
    ci[32..34].copy_from_slice(&DEVICE_CONNECT_FEATURES.to_le_bytes());
    ci[34] = PHYSICAL_DAP;
    ci[35] = MAX_TRANSFERS;
    let maps: u16 = 1 << MAP_DEVICE | 1 << MAP_SERVER;
    ci[38..40].copy_from_slice(&maps.to_le_bytes());
    put_str(&mut ci[48..112], &target.identifier);
    r.extend(ci);
    r
}

/// `DEVICE_CONNECT` answer.
pub fn device_connect(con_id: u8, option: u16, device_type: u32) -> Vec<u8> {
    let mut r = vec![0x02, CMD_DEVICE_CONNECT, con_id, PL1_OK];
    r.extend(option.to_le_bytes());
    r.extend([0, 0]);
    r.extend(device_type.to_le_bytes());
    r
}

/// One transfer of a PL0 frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub cmd: u8,
    pub map: u8,
    pub address: u32,
    /// The bytes to write; empty for a read.
    pub data: Vec<u8>,
    /// The bytes to read; 0 for a write.
    pub read_len: usize,
}

impl Transfer {
    pub fn is_read(&self) -> bool {
        self.read_len > 0
    }
}

/// A PL0 frame as the client sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The frame counter, echoed in the answer.
    pub counter: u16,
    pub transfers: Vec<Transfer>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FrameError(pub String);

/// Reads a PL0 frame (a PL2 payload that starts with `PL0_START`).
pub fn parse_frame(rq: &[u8]) -> Result<Frame, FrameError> {
    let short = || FrameError("the frame is cut short".into());
    if rq.len() < 8 || rq[1] != CMD_PL0_START {
        return Err(short());
    }
    let counter = u16::from_le_bytes([rq[4], rq[5]]);
    let (mut map, mut base, mut p) = (MAP_DEVICE, 0u32, 8);
    let mut transfers = Vec::new();
    loop {
        let element = rq.get(p..p + 4).ok_or_else(short)?;
        let (wl, cmd) = (element[0] as usize, element[1]);
        let offset = u16::from_le_bytes([element[2], element[3]]) as u32;
        let address = base | offset;
        // A block write of 256 words has a word count of 0.
        let size = match (cmd, wl) {
            (PL0_WRBLK, 0) => 4 + 1024,
            _ => (wl + 1) * 4,
        };
        let body = rq.get(p + 4..p + size).ok_or_else(short)?;
        p += size;
        let write = |n: usize| Transfer {
            cmd,
            map,
            address,
            data: body[..n].to_vec(),
            read_len: 0,
        };
        let read = |n: usize| Transfer {
            cmd,
            map,
            address,
            data: Vec::new(),
            read_len: n,
        };
        match cmd {
            CMD_PL0_END => break,
            PL0_ACCESS_MODE => {}
            PL0_ADDR_MAP => map = element[2],
            PL0_BASE_ADDR32 => base = offset << 16,
            PL0_WR8 | PL0_WR16 | PL0_WR32 if body.len() >= 4 => transfers.push(write(match cmd {
                PL0_WR8 => 1,
                PL0_WR16 => 2,
                _ => 4,
            })),
            PL0_WR64 if body.len() >= 8 => transfers.push(write(8)),
            PL0_WRBLK if !body.is_empty() => transfers.push(write(body.len())),
            PL0_RD8 => transfers.push(read(1)),
            PL0_RD16 => transfers.push(read(2)),
            PL0_RD32 => transfers.push(read(4)),
            PL0_RD64 => transfers.push(read(8)),
            PL0_RDBLK if !body.is_empty() => transfers.push(read(match body[0] {
                0 => 1024,
                words => words as usize * 4,
            })),
            PL0_RDBLK1KB => transfers.push(read(1024)),
            other => {
                return Err(FrameError(format!(
                    "PL0 element {other:#04x} is not supported"
                )));
            }
        }
    }
    Ok(Frame { counter, transfers })
}

/// The answer to a PL0 frame the server did not run.
pub fn frame_refused(con_id: u8, error: u8) -> Vec<u8> {
    status(CMD_PL0_START, con_id, error)
}

/// The answer to a PL0 frame: for each transfer its status and, for a read
/// that worked, the data.
pub fn frame_response(con_id: u8, frame: &Frame, results: &[Result<Vec<u8>, u8>]) -> Vec<u8> {
    let mut r = status(CMD_PL0_START, con_id, PL1_OK);
    for (transfer, result) in frame.transfers.iter().zip(results) {
        match result {
            Ok(data) if transfer.is_read() => {
                let words = transfer.read_len.div_ceil(4);
                if words == 256 {
                    // A full block counts 256 words: 0 in both byte fields.
                    r.extend([0, PL0_RDBLK1KB, 0, PL0_OK]);
                } else {
                    r.extend([words as u8, transfer.cmd, words as u8, PL0_OK]);
                }
                let mut padded = data.clone();
                padded.resize(words * 4, 0);
                r.extend(padded);
            }
            Ok(_) => {
                let words = transfer.data.len().div_ceil(4);
                r.extend([0, transfer.cmd, words as u8, PL0_OK]);
            }
            Err(code) => r.extend([0, transfer.cmd, 0, *code]),
        }
    }
    r.extend([0x00, CMD_PL0_END]);
    r.extend(frame.counter.to_le_bytes());
    r
}

/// The text block of map 15 that names the server.
pub fn server_info_block() -> Vec<u8> {
    let mut b = vec![0u8; 96];
    put_str(&mut b[0..32], "logicview DAP");
    put_str(&mut b[32..64], "logicview");
    put_str(&mut b[72..96], env!("CARGO_PKG_VERSION"));
    b
}

/// The text block of map 15 that names the access hardware.
pub fn hardware_info_block(probe: &str) -> Vec<u8> {
    let mut b = vec![0u8; 168];
    put_str(&mut b[0..64], probe);
    put_str(&mut b[64..96], "logicview");
    b
}

/// The block of map 15 that names the device family.
pub fn device_info_block(device_type: u32) -> Vec<u8> {
    let mut b = vec![0u8; 104];
    b[0..4].copy_from_slice(&device_type.to_le_bytes());
    put_str(&mut b[8..40], "TriCore-Family");
    b[40..44].copy_from_slice(&2u32.to_le_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    // The request and response bytes are from captures of the reference
    // client against TasServer (probe-rs fork, `tas/protocol.rs` tests).

    #[test]
    fn a_read32_frame_is_parsed_and_answered_like_tasserver() {
        let frame = parse_frame(&hex(
            "01 B0 FF 00 C4 FF 01 00 00 15 00 70 00 25 00 00 00 B1 01 00",
        ))
        .unwrap();
        assert_eq!(frame.counter, 0xFFC4);
        assert_eq!(
            frame.transfers,
            [Transfer {
                cmd: PL0_RD32,
                map: 0,
                address: 0x7000_0000,
                data: vec![],
                read_len: 4,
            }]
        );
        assert_eq!(
            frame_response(0x81, &frame, &[Ok(vec![0x78, 0x56, 0x34, 0x12])]),
            hex("00 B0 81 08 01 25 01 80 78 56 34 12 00 B1 C4 FF")
        );
    }

    #[test]
    fn a_write32_frame_is_parsed_and_answered_like_tasserver() {
        let frame = parse_frame(&hex(
            "01 B0 FF 00 C3 FF 01 00 00 15 00 70 01 24 00 00 78 56 34 12 00 B1 01 00",
        ))
        .unwrap();
        assert_eq!(frame.transfers[0].data, [0x78, 0x56, 0x34, 0x12]);
        assert_eq!(frame.transfers[0].address, 0x7000_0000);
        assert_eq!(
            frame_response(0x81, &frame, &[Ok(vec![])]),
            hex("00 B0 81 08 00 24 01 80 00 B1 C3 FF")
        );
    }

    #[test]
    fn block_reads_and_the_server_map_are_parsed() {
        let frame = parse_frame(&hex(
            "01 B0 FF 00 C6 FF 01 00 00 15 00 70 01 2C 00 00 3C 00 00 00 00 B1 01 00",
        ))
        .unwrap();
        assert_eq!(frame.transfers[0].read_len, 240);

        let frame = parse_frame(&hex(
            "01 B0 FF 00 C1 FF 00 80 00 11 0F 00 01 2C 00 D0 1A 00 00 00 00 B1 01 00",
        ))
        .unwrap();
        assert_eq!(
            (
                frame.transfers[0].map,
                frame.transfers[0].address,
                frame.transfers[0].read_len
            ),
            (15, 0xD000, 104)
        );
    }

    #[test]
    fn small_reads_share_a_base_and_failures_carry_their_codes() {
        let frame = parse_frame(&hex(
            "01 B0 FF 00 C7 FF 01 00 00 15 AD DE 00 21 EF BE 00 23 F0 BE 00 21 F2 BE 00 B1 03 00",
        ))
        .unwrap();
        let addresses: Vec<u32> = frame.transfers.iter().map(|t| t.address).collect();
        assert_eq!(addresses, [0xDEAD_BEEF, 0xDEAD_BEF0, 0xDEAD_BEF2]);
        assert_eq!(
            frame_response(
                0x81,
                &frame,
                &[
                    Err(PL0_ERR_DATA),
                    Err(PL0_ERR_CONSEQUENTIAL),
                    Err(PL0_ERR_CONSEQUENTIAL)
                ]
            ),
            hex("00 B0 81 08 00 21 00 8D 00 23 00 98 00 21 00 98 00 B1 C7 FF")
        );
    }

    #[test]
    fn full_blocks_count_256_words() {
        let mut rq = hex("01 B0 FF 00 01 00 01 00 00 15 00 70 00 2A 00 01");
        rq.extend(vec![0xAB; 1024]);
        rq.extend(hex("01 2C 00 01 00 00 00 00 00 B1 02 00"));
        let frame = parse_frame(&rq).unwrap();
        assert_eq!(frame.transfers[0].data.len(), 1024);
        assert_eq!(frame.transfers[0].address, 0x7000_0100);
        assert_eq!(frame.transfers[1].read_len, 1024);
        let r = frame_response(0x82, &frame, &[Ok(vec![]), Ok(vec![0xCD; 1024])]);
        assert_eq!(r[..8], hex("00 B0 82 08 00 2A 00 80"));
        assert_eq!(r[8..12], hex("00 2D 00 80"));
        assert_eq!(r.len(), 12 + 1024 + 4);
    }

    #[test]
    fn broken_frames_are_errors() {
        assert!(parse_frame(&hex("01 B0 FF 00 01 00 01 00 00 25 00 00")).is_err());
        assert!(parse_frame(&hex("01 B0 FF 00 01 00 01 00 03 2B 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 B1 01 00")).is_err());
    }

    #[test]
    fn control_answers_have_the_sizes_of_tasserver() {
        assert_eq!(server_connect("x", 1).len(), 136);
        let target = TargetInfo {
            identifier: "LV DAP test".into(),
            device_type: 0x2020_5083,
            clients: 2,
        };
        let r = get_targets(Some(&target));
        assert_eq!(r.len(), 96);
        assert_eq!((r[4], r[6], r[8 + 85]), (1, 1, 2));
        assert_eq!(c_string(&r[8..72]), "LV DAP test");
        assert_eq!(get_targets(None).len(), 8);
        let r = con_info(CMD_SESSION_START, &target);
        assert_eq!(r.len(), 132);
        assert_eq!(r[8 + 35], 32);
        assert_eq!(r[8 + 38..8 + 40], [0x01, 0x80]);
        assert_eq!(
            device_connect(0x81, 3, 0x401D_A083),
            hex("02 A8 81 08 03 00 00 00 83 A0 1D 40")
        );
    }
}
