//! Firmware update through the board's IAP bootloader, like the WeAct
//! U3LogicAnalyzer's "FW Update" (`pulseview/pv/dialogs/iapdialog.cpp`).
//!
//! The board is switched into its bootloader with the `0xAE` command
//! ([`crate::U3::enter_iap`]); it comes back as the HID device `1a86:fe17`.
//! Over HID (64-byte reports, report ID 0) the image is then erased,
//! programmed, verified and the board restarted:
//!
//! ```text
//! erase    [0x81, 0, addr LE32]                   -> [0, 0] (up to 5 s)
//! program  [0x80, n, data(n <= 62)] ... [0x80, 0] -> [0, 0] each
//! verify   [0x82, n, addr LE32, data(n <= 56)]    -> [0, 0] each
//! end      [0x83, 0]                              -> [0x84, ..] or a restart
//! ```
//!
//! The bootloader only takes an image that starts at one of the three app
//! slots and fits in one.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::Error;

pub const IAP_VID: u16 = 0x1a86;
pub const IAP_PID: u16 = 0xfe17;

const CMD_PROGRAM: u8 = 0x80;
const CMD_ERASE: u8 = 0x81;
const CMD_VERIFY: u8 = 0x82;
const CMD_END: u8 = 0x83;
const RESP_SUCCESS: u8 = 0x84;

pub const FLASH_BASE: u32 = 0x0800_0000;
/// App slots: logic analyzer, USB-serial + DAP, your own app.
pub const SLOTS: [(u32, &str); 3] = [
    (0x0800_6000, "슬롯 0: 로직 애널라이저"),
    (0x0805_2000, "슬롯 1: USB-시리얼 + DAP"),
    (0x0809_e000, "슬롯 2: 사용자 앱"),
];
pub const SLOT_SIZE: usize = 0x0004_c000;

const PROGRAM_CHUNK: usize = 62;
const VERIFY_CHUNK: usize = 56;
const HID_TIMEOUT: Duration = Duration::from_millis(500);
const ERASE_TIMEOUT: Duration = Duration::from_secs(5);
const END_TIMEOUT: Duration = Duration::from_secs(8);
/// How long the bootloader may take to show up after the switch.
const APPEAR_TIMEOUT: Duration = Duration::from_secs(8);

/// A firmware image and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub addr: u32,
    pub data: Vec<u8>,
}

impl Image {
    /// The slot index the image is for.
    pub fn slot(&self) -> Option<usize> {
        SLOTS.iter().position(|&(a, _)| a == self.addr)
    }
}

/// Read a `.hex` (it carries its address) or a `.bin` (written to `bin_slot`).
pub fn load_image(path: &Path, bin_slot: usize) -> Result<Image, Error> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::Iap(format!("{}을(를) 읽을 수 없습니다: {e}", path.display())))?;
    let is_hex = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("hex"));
    let image = if is_hex {
        let text = String::from_utf8_lossy(&bytes);
        let (addr, data) = parse_hex(&text)?;
        Image { addr, data }
    } else {
        let &(addr, _) = SLOTS
            .get(bin_slot)
            .ok_or_else(|| Error::Iap(format!("슬롯 {bin_slot}은 없습니다 (0~2)")))?;
        Image { addr, data: bytes }
    };
    check_image(image)
}

/// The bootloader's rules: a slot start, no larger than a slot, not empty.
pub fn check_image(mut image: Image) -> Result<Image, Error> {
    if image.data.is_empty() {
        return Err(Error::Iap("펌웨어 파일이 비어 있습니다".into()));
    }
    if image.addr < FLASH_BASE {
        // Linked against the flash alias at 0.
        image.addr += FLASH_BASE;
    }
    if image.slot().is_none() {
        return Err(Error::Iap(format!(
            "이미지가 0x{:08x}에서 시작합니다. 슬롯 시작 주소(0x{:08x}, 0x{:08x}, 0x{:08x})여야 합니다 \
             (코어 하나의 .hex가 아니라 합친 .bin을 쓸 것)",
            image.addr, SLOTS[0].0, SLOTS[1].0, SLOTS[2].0
        )));
    }
    if image.data.len() > SLOT_SIZE {
        return Err(Error::Iap(format!(
            "이미지가 {} 바이트로 슬롯({SLOT_SIZE} 바이트)보다 큽니다",
            image.data.len()
        )));
    }
    Ok(image)
}

/// Intel HEX -> (address of the first data byte, bytes from there on, gaps
/// filled with 0xFF). Handles segment (02) and linear (04) base records.
pub fn parse_hex(text: &str) -> Result<(u32, Vec<u8>), Error> {
    let bad = |n: usize, what: &str| Error::Iap(format!("HEX {n}번째 줄: {what}"));
    // Segment (02) and linear (04) bases add up, as in the vendor tool.
    let (mut segment, mut linear) = (0u32, 0u32);
    let mut first: Option<u32> = None;
    let mut out: Vec<u8> = Vec::new();
    for (n, line) in text.lines().enumerate().map(|(i, l)| (i + 1, l.trim())) {
        let Some(hex) = line.strip_prefix(':') else {
            continue;
        };
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16))
            .collect::<Result<_, _>>()
            .map_err(|_| bad(n, "16진수가 아님"))?;
        if bytes.len() < 5 || bytes.len() != 5 + bytes[0] as usize {
            return Err(bad(n, "길이가 맞지 않음"));
        }
        if bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b)) != 0 {
            return Err(bad(n, "체크섬이 맞지 않음"));
        }
        let count = bytes[0] as usize;
        let offset = u32::from(u16::from_be_bytes([bytes[1], bytes[2]]));
        let data = &bytes[4..4 + count];
        match bytes[3] {
            0x00 => {
                let addr = segment + linear + offset;
                let start = *first.get_or_insert(addr);
                let at = addr
                    .checked_sub(start)
                    .ok_or_else(|| bad(n, "앞쪽 주소로 돌아감"))? as usize;
                if at + count > SLOT_SIZE * 4 {
                    return Err(bad(n, "주소가 너무 멀리 떨어짐"));
                }
                if out.len() < at + count {
                    out.resize(at + count, 0xff);
                }
                out[at..at + count].copy_from_slice(data);
            }
            0x01 => break,
            0x02 if count == 2 => segment = u32::from(u16::from_be_bytes([data[0], data[1]])) << 4,
            0x04 if count == 2 => linear = u32::from(u16::from_be_bytes([data[0], data[1]])) << 16,
            0x03 | 0x05 => {}
            t => return Err(bad(n, &format!("모르는 레코드 형식 {t:02x}"))),
        }
    }
    match first {
        Some(a) if !out.is_empty() => Ok((a, out)),
        _ => Err(Error::Iap("HEX 파일에 데이터가 없습니다".into())),
    }
}

/// The packets of an update, in order (without the report ID).
pub fn erase_packet(addr: u32) -> Vec<u8> {
    let mut p = vec![CMD_ERASE, 0];
    p.extend_from_slice(&addr.to_le_bytes());
    p
}

pub fn program_packets(data: &[u8]) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = data
        .chunks(PROGRAM_CHUNK)
        .map(|c| {
            let mut p = vec![CMD_PROGRAM, c.len() as u8];
            p.extend_from_slice(c);
            p
        })
        .collect();
    v.push(vec![CMD_PROGRAM, 0]);
    v
}

pub fn verify_packets(addr: u32, data: &[u8]) -> Vec<Vec<u8>> {
    data.chunks(VERIFY_CHUNK)
        .enumerate()
        .map(|(i, c)| {
            let mut p = vec![CMD_VERIFY, c.len() as u8];
            p.extend_from_slice(&(addr + (i * VERIFY_CHUNK) as u32).to_le_bytes());
            p.extend_from_slice(c);
            p
        })
        .collect()
}

/// What an update is doing, for a progress bar (0..=100) and a log.
#[derive(Debug, Clone)]
pub struct Progress {
    pub percent: u8,
    pub text: String,
}

struct Hid {
    dev: hidapi::HidDevice,
}

impl Hid {
    /// The bootloader of the board with `serial` (any, if none is known),
    /// waiting for it to appear.
    fn find(serial: Option<&str>, cancel: &AtomicBool) -> Result<Hid, Error> {
        let mut api = hidapi::HidApi::new().map_err(|e| Error::Iap(format!("hidapi: {e}")))?;
        let start = Instant::now();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Iap("취소했습니다".into()));
            }
            let _ = api.refresh_devices();
            let list: Vec<_> = api
                .device_list()
                .filter(|d| d.vendor_id() == IAP_VID && d.product_id() == IAP_PID)
                .cloned()
                .collect();
            let pick = list
                .iter()
                .find(|d| {
                    serial.is_some_and(|s| {
                        d.serial_number().is_some_and(|n| n.eq_ignore_ascii_case(s))
                    })
                })
                .or(list.first());
            if let Some(info) = pick
                && let Ok(dev) = info.open_device(&api)
            {
                return Ok(Hid { dev });
            }
            if start.elapsed() > APPEAR_TIMEOUT {
                return Err(Error::Iap(format!(
                    "업데이트 모드의 보드({IAP_VID:04x}:{IAP_PID:04x})를 찾지 못했습니다"
                )));
            }
            thread::sleep(Duration::from_millis(300));
        }
    }

    fn write(&self, packet: &[u8]) -> Result<(), Error> {
        let mut buf = [0u8; 65]; // report ID 0 + 64 bytes
        buf[1..1 + packet.len()].copy_from_slice(packet);
        self.dev
            .write(&buf)
            .map(|_| ())
            .map_err(|e| Error::Iap(format!("HID 쓰기 실패: {e}")))
    }

    /// The first two bytes of the next report.
    fn read(&self, timeout: Duration) -> Result<[u8; 2], Error> {
        let mut buf = [0u8; 65];
        let n = self
            .dev
            .read_timeout(&mut buf, timeout.as_millis() as i32)
            .map_err(|e| Error::Iap(format!("HID 읽기 실패: {e}")))?;
        if n == 0 {
            return Err(Error::Iap("보드가 답하지 않습니다".into()));
        }
        // Same as the vendor tool: the reply follows the report ID byte.
        Ok([buf[1], buf[2]])
    }

    /// Send and expect `[0, 0]`, three tries like the vendor tool.
    fn exchange(&self, packet: &[u8], timeout: Duration, what: &str) -> Result<(), Error> {
        let mut last = String::new();
        for _ in 0..4 {
            match self.write(packet).and_then(|()| self.read(timeout)) {
                Ok([0, 0]) => return Ok(()),
                Ok([_, code]) => last = format!("{what}: 오류 코드 {code}"),
                Err(e) => last = format!("{what}: {e}"),
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(Error::Iap(last))
    }
}

/// Write `image` to a board that is in (or switching to) its bootloader.
/// `serial` picks the board when several are in update mode.
pub fn upgrade(
    image: &Image,
    serial: Option<&str>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<(), Error> {
    let mut say = |percent: u8, text: String| {
        log::info!("iap: {text}");
        progress(Progress { percent, text });
    };
    let check = || {
        if cancel.load(Ordering::Relaxed) {
            Err(Error::Iap("취소했습니다".into()))
        } else {
            Ok(())
        }
    };
    say(10, "업데이트 모드의 보드를 찾는 중…".into());
    let hid = Hid::find(serial, cancel)?;
    say(
        20,
        format!(
            "지우는 중… (0x{:08x}, {} 바이트)",
            image.addr,
            image.data.len()
        ),
    );
    hid.exchange(&erase_packet(image.addr), ERASE_TIMEOUT, "지우기")?;
    let packets = program_packets(&image.data);
    let n = packets.len();
    for (i, p) in packets.iter().enumerate() {
        check()?;
        hid.exchange(p, HID_TIMEOUT, "쓰기")?;
        if i % 64 == 0 || i + 1 == n {
            say(
                (30 + 40 * (i + 1) / n) as u8,
                format!("쓰는 중… {}%", 100 * (i + 1) / n),
            );
        }
    }
    let packets = verify_packets(image.addr, &image.data);
    let n = packets.len();
    for (i, p) in packets.iter().enumerate() {
        check()?;
        hid.exchange(p, HID_TIMEOUT, "확인")?;
        if i % 64 == 0 || i + 1 == n {
            say(
                (70 + 25 * (i + 1) / n) as u8,
                format!("확인하는 중… {}%", 100 * (i + 1) / n),
            );
        }
    }
    say(95, "마무리하는 중…".into());
    hid.write(&[CMD_END, 0])?;
    let note = match hid.read(END_TIMEOUT) {
        Ok([RESP_SUCCESS, _]) => "보드가 업데이트를 확인했습니다",
        _ => "보드가 다시 시작했습니다",
    };
    say(100, format!("완료: {note}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u8, addr: u16, data: &[u8]) -> String {
        let mut b = vec![data.len() as u8];
        b.extend_from_slice(&addr.to_be_bytes());
        b.push(kind);
        b.extend_from_slice(data);
        let sum = b.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        b.push(sum.wrapping_neg());
        format!(
            ":{}",
            b.iter().map(|x| format!("{x:02X}")).collect::<String>()
        )
    }

    #[test]
    fn iap_hex_with_linear_base_and_gap() {
        let text = [
            record(0x04, 0, &[0x08, 0x00]),
            record(0x00, 0x6000, &[1, 2, 3, 4]),
            record(0x00, 0x6006, &[7, 8]),
            record(0x05, 0, &[0, 0, 0, 0]),
            record(0x01, 0, &[]),
        ]
        .join("\n");
        let (addr, data) = parse_hex(&text).unwrap();
        assert_eq!(addr, 0x0800_6000);
        assert_eq!(data, [1, 2, 3, 4, 0xff, 0xff, 7, 8]);
        assert_eq!(check_image(Image { addr, data }).unwrap().slot(), Some(0));
    }

    #[test]
    fn iap_hex_with_segment_base() {
        // 0x0800_0000 has no 16-bit segment; the alias at 0 is moved up.
        let text = [
            record(0x02, 0, &[0x5e, 0x00]), // 0x5e000
            record(0x04, 0, &[0x00, 0x04]), // + 0x40000
            record(0x00, 0x0000, &[0xaa]),
            record(0x01, 0, &[]),
        ]
        .join("\n");
        let (addr, data) = parse_hex(&text).unwrap();
        assert_eq!(addr, 0x0009_e000);
        let img = check_image(Image { addr, data }).unwrap();
        assert_eq!((img.addr, img.slot()), (0x0809_e000, Some(2)));
    }

    #[test]
    fn iap_rejects_bad_files() {
        let mut bad = record(0x00, 0x6000, &[1, 2]);
        bad.replace_range(bad.len() - 2.., "00");
        assert!(parse_hex(&bad).is_err(), "checksum");
        assert!(parse_hex(":zz").is_err());
        assert!(parse_hex(&record(0x01, 0, &[])).is_err(), "no data");
        let off_slot = Image {
            addr: 0x0800_7000,
            data: vec![0; 16],
        };
        assert!(check_image(off_slot).is_err());
        let too_big = Image {
            addr: SLOTS[1].0,
            data: vec![0; SLOT_SIZE + 1],
        };
        assert!(check_image(too_big).is_err());
        assert!(
            check_image(Image {
                addr: SLOTS[0].0,
                data: Vec::new()
            })
            .is_err()
        );
    }

    #[test]
    fn iap_packets() {
        assert_eq!(erase_packet(0x0800_6000), [0x81, 0, 0x00, 0x60, 0x00, 0x08]);
        let data: Vec<u8> = (0..130u8).collect();
        let p = program_packets(&data);
        assert_eq!(p.len(), 4); // 62 + 62 + 6, then the empty end packet
        assert_eq!(&p[0][..3], &[0x80, 62, 0]);
        assert_eq!(&p[2][..2], &[0x80, 6]);
        assert_eq!(p[3], [0x80, 0]);
        let v = verify_packets(0x0800_6000, &data);
        assert_eq!(v.len(), 3); // 56 + 56 + 18
        assert_eq!(&v[1][..6], &[0x82, 56, 0x38, 0x60, 0x00, 0x08]);
        assert_eq!(v[1][6], 56);
        assert_eq!(v[2][1], 18);
        assert!(p.iter().chain(&v).all(|x| x.len() <= 64));
    }
}
