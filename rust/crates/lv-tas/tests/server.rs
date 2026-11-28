//! The TAS server over TCP, with the pretend device behind it. The client
//! here is written from the protocol description; `interop.rs` in the docs
//! describes the check with the probe-rs fork's own client.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::Ordering;
use std::time::Duration;

use lv_dap::link::Link;
use lv_dap::sim::{Faults, SIM_JTAG_ID, SimWire};
use lv_tas::Server;

fn server(faults: Faults) -> Server {
    let link = Link::new(SimWire::with_faults(faults));
    Server::start(Box::new(link), "127.0.0.1:0".parse::<SocketAddr>().unwrap()).unwrap()
}

struct Client {
    stream: TcpStream,
    counter: u16,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl Client {
    fn connect(server: &Server) -> Client {
        let stream = TcpStream::connect(server.address()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Client {
            stream,
            counter: 0xFFC0,
        }
    }

    fn request(&mut self, payload: &[u8]) -> Vec<u8> {
        let mut message = ((payload.len() + 4) as u32).to_le_bytes().to_vec();
        message.extend_from_slice(payload);
        self.stream.write_all(&message).unwrap();
        let mut length = [0u8; 4];
        self.stream.read_exact(&mut length).unwrap();
        let mut response = vec![0; u32::from_le_bytes(length) as usize - 4];
        self.stream.read_exact(&mut response).unwrap();
        response
    }

    fn server_connect(&mut self) -> Vec<u8> {
        let mut p = [0u8; 56];
        p[0] = 0x0D;
        p[1] = 0x80;
        p[4..8].copy_from_slice(b"test");
        self.request(&p)
    }

    fn get_targets(&mut self) -> Vec<u8> {
        self.request(&[0x00, 0x86, 0x00, 0x00])
    }

    fn session_start(&mut self, identifier: &str) -> Vec<u8> {
        let mut p = [0u8; 112];
        p[0] = 0x1B;
        p[1] = 0xA1;
        p[3] = 1;
        p[16..16 + identifier.len()].copy_from_slice(identifier.as_bytes());
        self.request(&p)
    }

    fn device_connect(&mut self, option: u16) -> Vec<u8> {
        let mut p = [0u8; 8];
        p[0] = 0x01;
        p[1] = 0xA8;
        p[4..6].copy_from_slice(&option.to_le_bytes());
        self.request(&p)
    }

    /// A session on the server's target, attached without a reset.
    fn attached(server: &Server) -> Client {
        let mut client = Client::connect(server);
        assert_eq!(client.server_connect()[3], 0x08);
        assert_eq!(client.session_start(server.identifier())[3], 0x08);
        assert_eq!(client.request(&[0x01, 0xAD, 0, 0, 1, 0, 0, 0])[3], 0x08);
        let r = client.device_connect(0);
        assert_eq!((r[3], u32_at(&r, 8)), (0x08, SIM_JTAG_ID));
        client
    }

    /// Sends the elements as one PL0 frame for address map `map`.
    fn frame(&mut self, map: u8, elements: &[u8], transfers: u16) -> Vec<u8> {
        self.counter = self.counter.wrapping_add(1);
        let mut rq = vec![0x01, 0xB0, 0xFF, 0x00];
        rq.extend(self.counter.to_le_bytes());
        rq.extend((1u16 << map).to_le_bytes());
        if map != 0 {
            rq.extend([0x00, 0x11, map, 0x00]);
        }
        rq.extend_from_slice(elements);
        rq.extend([0x00, 0xB1]);
        rq.extend(transfers.to_le_bytes());
        let r = self.request(&rq);
        assert_eq!(r[1], 0xB0);
        if r[3] == 0x08 {
            let end = &r[r.len() - 4..];
            assert_eq!(end[..2], [0x00, 0xB1]);
            assert_eq!(u16::from_le_bytes([end[2], end[3]]), self.counter);
        }
        r
    }

    fn write32(&mut self, address: u32, value: u32) -> u8 {
        let mut e = vec![0x00, 0x15];
        e.extend(((address >> 16) as u16).to_le_bytes());
        e.extend([0x01, 0x24]);
        e.extend((address as u16).to_le_bytes());
        e.extend(value.to_le_bytes());
        self.frame(0, &e, 1)[7]
    }

    fn read32(&mut self, address: u32) -> Result<u32, u8> {
        let mut e = vec![0x00, 0x15];
        e.extend(((address >> 16) as u16).to_le_bytes());
        e.extend([0x00, 0x25]);
        e.extend((address as u16).to_le_bytes());
        let r = self.frame(0, &e, 1);
        if r[3] != 0x08 {
            return Err(r[3]);
        }
        match r[7] {
            0x80 => Ok(u32_at(&r, 8)),
            code => Err(code),
        }
    }
}

#[test]
fn a_tool_lists_the_target_attaches_and_reads_what_it_wrote() {
    let server = server(Faults::default());
    let mut client = Client::connect(&server);

    let r = client.server_connect();
    assert_eq!((r.len(), r[1], r[3]), (136, 0x80, 0x08));
    assert!(String::from_utf8_lossy(&r[8..72]).starts_with("TasServer"));

    // Listing attaches to the device, so the list names its JTAG ID.
    let r = client.get_targets();
    assert_eq!((r.len(), r[3], r[6]), (96, 0x08, 1));
    assert!(String::from_utf8_lossy(&r[8..72]).starts_with("LV DAP"));
    assert_eq!(u32_at(&r, 72), SIM_JTAG_ID);
    assert_eq!(r[8 + 85], 0);

    let r = client.session_start(server.identifier());
    assert_eq!((r.len(), r[3]), (132, 0x08));
    assert_eq!(u32_at(&r, 8), 1060);
    assert_eq!(u32_at(&r, 16), SIM_JTAG_ID);
    assert_eq!(server.sessions(), 1);

    let r = client.device_connect(0);
    assert_eq!(r[..8], [0x02, 0xA8, 0x81, 0x08, 0, 0, 0, 0]);
    assert_eq!(u32_at(&r, 8), SIM_JTAG_ID);
    // The device was attached once, by the listing.
    assert_eq!(server.stats().device_connects.load(Ordering::Relaxed), 1);

    assert_eq!(client.write32(0x7000_0000, 0x1234_5678), 0x80);
    assert_eq!(client.read32(0x7000_0000), Ok(0x1234_5678));

    // Bytes, half words, a 64 bit value and a block in one frame.
    let mut block = vec![0u8; 240];
    block.iter_mut().enumerate().for_each(|(i, b)| *b = i as u8);
    let mut e = vec![0x00, 0x15, 0x00, 0xD0];
    e.extend([0x01, 0x20, 0x01, 0x00, 0xAB, 0, 0, 0]); // WR8 @1
    e.extend([0x01, 0x22, 0x02, 0x00, 0xCD, 0xEF, 0, 0]); // WR16 @2
    e.extend([0x02, 0x26, 0x08, 0x00, 1, 2, 3, 4, 5, 6, 7, 8]); // WR64 @8
    e.extend([60, 0x2A, 0x00, 0x01]); // WRBLK @0x100, 60 words
    e.extend(&block);
    e.extend([0x00, 0x21, 0x01, 0x00]); // RD8 @1
    e.extend([0x00, 0x23, 0x02, 0x00]); // RD16 @2
    e.extend([0x00, 0x27, 0x08, 0x00]); // RD64 @8
    e.extend([0x01, 0x2C, 0x00, 0x01, 60, 0, 0, 0]); // RDBLK @0x100
    let r = client.frame(0, &e, 8);
    let mut expected = vec![0x00, 0xB0, 0x81, 0x08];
    expected.extend([0, 0x20, 1, 0x80, 0, 0x22, 1, 0x80, 0, 0x26, 2, 0x80]);
    expected.extend([0, 0x2A, 60, 0x80]);
    expected.extend([1, 0x21, 1, 0x80, 0xAB, 0, 0, 0]);
    expected.extend([1, 0x23, 1, 0x80, 0xCD, 0xEF, 0, 0]);
    expected.extend([2, 0x27, 2, 0x80, 1, 2, 3, 4, 5, 6, 7, 8]);
    expected.extend([60, 0x2C, 60, 0x80]);
    expected.extend(&block);
    assert_eq!(r[..r.len() - 4], expected);
    assert_eq!(server.stats().frames.load(Ordering::Relaxed), 3);

    drop(client);
    for _ in 0..100 {
        if server.sessions() == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(server.sessions(), 0);
}

#[test]
fn a_full_kilobyte_goes_in_one_transfer() {
    let server = server(Faults::default());
    let mut client = Client::attached(&server);
    let data: Vec<u8> = (0..1024u32).map(|i| ((i * 31) >> 3) as u8).collect();
    let mut e = vec![0x00, 0x15, 0x00, 0x70, 0x00, 0x2A, 0x00, 0x00];
    e.extend(&data);
    assert_eq!(client.frame(0, &e, 1)[4..8], [0, 0x2A, 0, 0x80]);
    let r = client.frame(
        0,
        &[0x00, 0x15, 0x00, 0x70, 0x01, 0x2C, 0x00, 0x00, 0, 0, 0, 0],
        1,
    );
    assert_eq!(r[4..8], [0, 0x2D, 0, 0x80]);
    assert_eq!(r[8..8 + 1024], data[..]);
    assert_eq!(r.len(), 8 + 1024 + 4);
}

#[test]
fn a_refused_access_fails_alone_and_stops_its_frame() {
    let server = server(Faults {
        bad: vec![0xDEAD_0000..0xDEAD_8000, 0xDEAD_8000..0xDEAE_0000],
        ..Faults::default()
    });
    let mut client = Client::attached(&server);
    assert_eq!(client.write32(0x7000_0000, 7), 0x80);
    // Three reads at a bad address: the first fails, the others are not run.
    let e = [
        0x00, 0x15, 0xAD, 0xDE, 0x00, 0x21, 0xEF, 0xBE, 0x00, 0x23, 0xF0, 0xBE, 0x00, 0x21, 0xF2,
        0xBE,
    ];
    let r = client.frame(0, &e, 3);
    assert_eq!(
        r[4..16],
        [0, 0x21, 0, 0x8D, 0, 0x23, 0, 0x98, 0, 0x21, 0, 0x98]
    );
    assert_eq!(client.read32(0x7000_0000), Ok(7));
    assert_eq!(client.write32(0xDEAD_0000, 1), 0x8D);
    assert_eq!(client.read32(0x7000_0000), Ok(7));
    assert_eq!(server.stats().failed_transfers.load(Ordering::Relaxed), 2);
}

#[test]
fn a_reset_is_told_to_the_other_sessions_once() {
    let server = server(Faults::default());
    let mut first = Client::attached(&server);
    let mut second = Client::attached(&server);
    assert_eq!(server.sessions(), 2);
    assert_eq!(first.get_targets()[8 + 85], 2);
    assert_eq!(first.write32(0x7000_0000, 0xAA), 0x80);

    let r = first.device_connect(3);
    assert_eq!(r[..6], [0x02, 0xA8, 0x81, 0x08, 3, 0]);
    assert_eq!(u32_at(&r, 8), SIM_JTAG_ID);
    assert_eq!(server.stats().resets.load(Ordering::Relaxed), 1);

    // The session that reset goes on; the other one is refused once.
    assert_eq!(first.read32(0x7000_0000), Ok(0xAA));
    assert_eq!(second.read32(0x7000_0000), Err(0x44));
    assert_eq!(second.read32(0x7000_0000), Ok(0xAA));

    // A ping hears of a reset too.
    assert_eq!(second.device_connect(1)[3], 0x08);
    assert_eq!(
        first.request(&[0x00, 0xA2, 0x00, 0x00])[..4],
        [0x00, 0xA2, 0x81, 0x44]
    );
    let r = first.request(&[0x00, 0xA2, 0x00, 0x00]);
    assert_eq!((r.len(), r[3]), (132, 0x08));
    assert_eq!(first.read32(0x7000_0000), Ok(0xAA));
}

#[test]
fn the_clock_is_a_server_register() {
    let server = server(Faults::default());
    let mut client = Client::attached(&server);
    let read = [0x00, 0x15, 0x0C, 0x00, 0x00, 0x25, 0x00, 0x01];
    let r = client.frame(15, &read, 1);
    assert_eq!((r[7], u32_at(&r, 8)), (0x80, 5_000_000));

    let mut write = vec![0x00, 0x15, 0x0C, 0x00, 0x01, 0x24, 0x00, 0x01];
    write.extend(6_000_000u32.to_le_bytes());
    assert_eq!(client.frame(15, &write, 1)[7], 0x80);
    let r = client.frame(15, &read, 1);
    assert_eq!(u32_at(&r, 8), 6_000_000);

    // The block that names the device family, as MCD clients read it.
    let r = client.frame(15, &[0x01, 0x2C, 0x00, 0xD0, 26, 0, 0, 0], 1);
    assert_eq!(r[4..8], [26, 0x2C, 26, 0x80]);
    assert_eq!(u32_at(&r, 8), SIM_JTAG_ID);
    assert_eq!(&r[16..30], b"TriCore-Family");
    // Other server addresses and the emulation device are refused.
    assert_eq!(client.frame(15, &[0x00, 0x25, 0x90, 0x00], 1)[7], 0x8D);
    assert_eq!(client.frame(1, &[0x00, 0x25, 0x00, 0x00], 1)[7], 0x8D);
    assert_eq!(client.read32(0x7000_0000), Ok(0));
}

#[test]
fn what_the_server_does_not_know_is_an_error_not_a_hangup() {
    let server = server(Faults::default());
    let mut client = Client::connect(&server);
    client.server_connect();
    assert_eq!(
        client.session_start("another target")[..4],
        [0, 0xA1, 0, 0x20]
    );
    assert_eq!(server.sessions(), 0);
    assert_eq!(
        client.request(&[0x00, 0x90, 0x00, 0x00])[..4],
        [0, 0x90, 0, 0x20]
    );
    assert_eq!(client.session_start(server.identifier())[3], 0x08);
    assert_eq!(client.device_connect(0x0100)[3], 0x20);
    // A frame with an element we do not run, then a good one.
    let fill = [0x03, 0x2B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(client.frame(0, &fill, 1)[3], 0x20);
    assert_eq!(client.write32(0x7000_0000, 3), 0x80);
    assert_eq!(client.read32(0x7000_0000), Ok(3));
}

#[test]
fn a_device_that_is_not_there_is_reported() {
    let server = server(Faults {
        dead: true,
        ..Faults::default()
    });
    let mut client = Client::connect(&server);
    client.server_connect();
    let r = client.get_targets();
    assert_eq!((r[6], u32_at(&r, 72)), (1, 0));
    assert_eq!(client.session_start(server.identifier())[3], 0x08);
    assert_eq!(client.device_connect(0)[..4], [0x00, 0xA8, 0x81, 0x46]);
    assert_eq!(client.device_connect(3)[3], 0x46);
    assert_eq!(client.read32(0x7000_0000), Err(0x8D));
}

#[test]
fn a_locked_device_refuses_the_reset_connect() {
    let server = server(Faults {
        locked: true,
        ..Faults::default()
    });
    let mut client = Client::attached(&server);
    assert_eq!(client.device_connect(3)[3], 0x46);
}

#[test]
fn stopping_the_server_ends_its_sessions() {
    let server = server(Faults::default());
    let mut client = Client::attached(&server);
    let address = server.address();
    drop(server);
    let mut byte = [0u8; 1];
    // The session thread closes the connection within its poll interval.
    assert!(matches!(client.stream.read(&mut byte), Ok(0) | Err(_)));
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err());
}
