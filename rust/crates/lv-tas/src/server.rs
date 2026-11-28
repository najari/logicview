//! The TAS server: tools made for Infineon's TasServer (the probe-rs
//! TriCore fork, MCD clients) connect over TCP and reach the device through
//! a [`Target`].
//!
//! One device, any number of sessions. Every connection has its own thread;
//! the device is behind one lock, so the frames of different sessions run
//! one after the other.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lv_dap::link::{LinkError, Reset, Target};

use crate::proto::{self, Frame, TargetInfo, Transfer};

/// Messages larger than this are not TAS.
const MAX_MESSAGE: usize = 0x1_0010;
/// How often the accept loop looks whether it should stop.
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// Counters of what the server did, for logs and tests.
#[derive(Debug, Default)]
pub struct Stats {
    pub connections: AtomicUsize,
    pub frames: AtomicUsize,
    pub transfers: AtomicUsize,
    pub failed_transfers: AtomicUsize,
    pub device_connects: AtomicUsize,
    pub resets: AtomicUsize,
}

struct Shared {
    target: Mutex<Box<dyn Target>>,
    identifier: String,
    /// Counts the device resets asked for through the server.
    reset_generation: AtomicU64,
    /// Sessions started and still connected.
    sessions: AtomicUsize,
    next_con_id: AtomicU8,
    start_time_us: u64,
    stats: Stats,
}

impl Shared {
    fn target(&self) -> MutexGuard<'_, Box<dyn Target>> {
        // A panic in another session does not take the device with it.
        self.target.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Connects without a reset if nothing is connected yet.
    fn ensure_attached(&self, target: &mut Box<dyn Target>) -> Result<u32, LinkError> {
        match target.jtag_id() {
            Some(id) => Ok(id),
            None => {
                self.stats.device_connects.fetch_add(1, Ordering::Relaxed);
                target.connect(Reset::None)
            }
        }
    }

    fn target_info(&self, attach: bool) -> TargetInfo {
        let mut target = self.target();
        let device_type = if attach {
            self.ensure_attached(&mut target).unwrap_or_else(|e| {
                log::debug!("TAS: the device is not reachable: {e}");
                0
            })
        } else {
            target.jtag_id().unwrap_or(0)
        };
        TargetInfo {
            identifier: self.identifier.clone(),
            device_type,
            clients: self.sessions.load(Ordering::Relaxed).min(255) as u8,
        }
    }
}

/// A running TAS server. Dropping it stops it.
pub struct Server {
    address: SocketAddr,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl Server {
    /// Starts serving `target` on `address` (port 0 picks a free one).
    pub fn start(target: Box<dyn Target>, address: SocketAddr) -> io::Result<Server> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let mut identifier = format!("LV DAP {}", target.name());
        // The identifier field is 64 bytes with its end mark.
        while identifier.len() > 63 {
            identifier.pop();
        }
        let shared = Arc::new(Shared {
            target: Mutex::new(target),
            identifier,
            reset_generation: AtomicU64::new(0),
            sessions: AtomicUsize::new(0),
            next_con_id: AtomicU8::new(0),
            start_time_us: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_micros() as u64),
            stats: Stats::default(),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let (shared, stop) = (shared.clone(), stop.clone());
            thread::Builder::new()
                .name("tas-accept".into())
                .spawn(move || accept_loop(listener, shared, stop))?
        };
        Ok(Server {
            address,
            shared,
            stop,
            accept: Some(accept),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The target name tools select the device by.
    pub fn identifier(&self) -> &str {
        &self.shared.identifier
    }

    pub fn stats(&self) -> &Stats {
        &self.shared.stats
    }

    /// Sessions connected right now.
    pub fn sessions(&self) -> usize {
        self.shared.sessions.load(Ordering::Relaxed)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                shared.stats.connections.fetch_add(1, Ordering::Relaxed);
                let (shared, stop) = (shared.clone(), stop.clone());
                let spawned = thread::Builder::new()
                    .name("tas-session".into())
                    .spawn(move || {
                        log::debug!("TAS: {peer} connected");
                        let mut session = Session::new(shared, stop);
                        if let Err(e) = session.serve(stream) {
                            log::debug!("TAS: {peer}: {e}");
                        }
                        log::debug!("TAS: {peer} disconnected");
                    });
                if let Err(e) = spawned {
                    log::warn!("TAS: no thread for {peer}: {e}");
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
            Err(e) => {
                log::warn!("TAS: accept: {e}");
                thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

/// One client connection.
struct Session {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    /// `SESSION_START` was answered: the session counts as a client.
    started: bool,
    con_id: u8,
    /// The device reset this session knows of.
    reset_seen: u64,
    client: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.started {
            self.shared.sessions.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Session {
    fn new(shared: Arc<Shared>, stop: Arc<AtomicBool>) -> Session {
        let reset_seen = shared.reset_generation.load(Ordering::Relaxed);
        Session {
            shared,
            stop,
            started: false,
            con_id: 0,
            reset_seen,
            client: String::new(),
        }
    }

    fn serve(&mut self, mut stream: TcpStream) -> io::Result<()> {
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        // Wake up now and then to see whether the server stops.
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        loop {
            let Some(request) = self.receive(&mut stream)? else {
                return Ok(());
            };
            let response = self.handle(&request);
            let mut message = ((response.len() + 4) as u32).to_le_bytes().to_vec();
            message.extend_from_slice(&response);
            stream.write_all(&message)?;
        }
    }

    /// The next message, or `None` when the client is gone or the server
    /// stops.
    fn receive(&self, stream: &mut TcpStream) -> io::Result<Option<Vec<u8>>> {
        let mut length = [0u8; 4];
        if !self.read_full(stream, &mut length, true)? {
            return Ok(None);
        }
        let length = u32::from_le_bytes(length) as usize;
        if !(8..=MAX_MESSAGE).contains(&length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("message length {length}"),
            ));
        }
        let mut payload = vec![0; length - 4];
        if !self.read_full(stream, &mut payload, false)? {
            return Ok(None);
        }
        Ok(Some(payload))
    }

    /// Fills `buf`; false if the connection ended (or, between messages,
    /// the server stops).
    fn read_full(&self, stream: &mut TcpStream, buf: &mut [u8], idle: bool) -> io::Result<bool> {
        let mut at = 0;
        while at < buf.len() {
            match stream.read(&mut buf[at..]) {
                Ok(0) => return Ok(false),
                Ok(n) => at += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    // Between messages a quiet client is fine; a stopping
                    // server ends the session either way.
                    let _ = idle;
                    if self.stop.load(Ordering::Relaxed) {
                        return Ok(false);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// After a reset by another session, the next frame or ping is refused
    /// once with `DEV_RESET`, as TasServer does.
    fn reset_pending(&mut self) -> bool {
        let now = self.shared.reset_generation.load(Ordering::Relaxed);
        let pending = now != self.reset_seen;
        self.reset_seen = now;
        pending
    }

    fn handle(&mut self, rq: &[u8]) -> Vec<u8> {
        let cmd = rq[1];
        match cmd {
            proto::CMD_SERVER_CONNECT => {
                self.client = proto::c_string(rq.get(4..36).unwrap_or_default());
                log::debug!("TAS: client \"{}\"", self.client);
                proto::server_connect("TasServer (logicview)", self.shared.start_time_us)
            }
            proto::CMD_GET_TARGETS => proto::get_targets(Some(&self.shared.target_info(true))),
            proto::CMD_SESSION_START => self.session_start(rq),
            proto::CMD_PING => {
                if self.reset_pending() {
                    return proto::status(cmd, self.con_id, proto::PL1_ERR_DEVICE_RESET);
                }
                proto::con_info(cmd, &self.shared.target_info(false))
            }
            proto::CMD_SET_DEVICE_KEY => proto::status(cmd, self.con_id, proto::PL1_OK),
            proto::CMD_DEVICE_CONNECT => self.device_connect(rq),
            proto::CMD_PL0_START => self.frame(rq),
            other => {
                log::debug!("TAS: command {other:#04x} is not supported");
                proto::status(cmd, self.con_id, proto::PL1_ERR_PARAM)
            }
        }
    }

    fn session_start(&mut self, rq: &[u8]) -> Vec<u8> {
        let cmd = proto::CMD_SESSION_START;
        let client_type = rq.get(3).copied().unwrap_or(0);
        let identifier = proto::c_string(rq.get(16..80).unwrap_or_default());
        if client_type != 1 || !identifier.eq_ignore_ascii_case(&self.shared.identifier) {
            log::debug!("TAS: session refused: type {client_type}, target \"{identifier}\"");
            return proto::status(cmd, 0, proto::PL1_ERR_PARAM);
        }
        if !self.started {
            self.started = true;
            self.shared.sessions.fetch_add(1, Ordering::Relaxed);
            let n = self.shared.next_con_id.fetch_add(1, Ordering::Relaxed);
            self.con_id = 0x81 + n % 0x7E;
        }
        proto::con_info(cmd, &self.shared.target_info(true))
    }

    fn device_connect(&mut self, rq: &[u8]) -> Vec<u8> {
        let cmd = proto::CMD_DEVICE_CONNECT;
        let option = u16::from_le_bytes([
            rq.get(4).copied().unwrap_or(0),
            rq.get(5).copied().unwrap_or(0),
        ]);
        let shared = self.shared.clone();
        let mut target = shared.target();
        let result = match option {
            proto::CONNECT_HOT_ATTACH => shared.ensure_attached(&mut target),
            proto::CONNECT_RESET | proto::CONNECT_RESET_AND_HALT => {
                let reset = if option == proto::CONNECT_RESET_AND_HALT {
                    Reset::Halt
                } else {
                    Reset::Run
                };
                shared.stats.device_connects.fetch_add(1, Ordering::Relaxed);
                shared.stats.resets.fetch_add(1, Ordering::Relaxed);
                let result = target.connect(reset);
                // The other sessions hear of the reset; this one caused it.
                self.reset_seen = shared.reset_generation.fetch_add(1, Ordering::Relaxed) + 1;
                result
            }
            other => {
                log::debug!("TAS: DEVICE_CONNECT option {other:#06x} is not supported");
                return proto::status(cmd, self.con_id, proto::PL1_ERR_PARAM);
            }
        };
        match result {
            Ok(id) => proto::device_connect(self.con_id, option, id),
            Err(e) => {
                log::warn!("TAS: DEVICE_CONNECT {option:#06x}: {e}");
                proto::status(cmd, self.con_id, proto::PL1_ERR_DEVICE_ACCESS)
            }
        }
    }

    fn frame(&mut self, rq: &[u8]) -> Vec<u8> {
        if self.reset_pending() {
            return proto::frame_refused(self.con_id, proto::PL1_ERR_DEVICE_RESET);
        }
        let frame = match proto::parse_frame(rq) {
            Ok(frame) => frame,
            Err(e) => {
                log::debug!("TAS: {}", e.0);
                return proto::frame_refused(self.con_id, proto::PL1_ERR_PARAM);
            }
        };
        let shared = self.shared.clone();
        shared.stats.frames.fetch_add(1, Ordering::Relaxed);
        let results = run_frame(&shared, &frame);
        proto::frame_response(self.con_id, &frame, &results)
    }
}

/// Runs the transfers of a frame in order; after the first failure the rest
/// is not run.
fn run_frame(shared: &Shared, frame: &Frame) -> Vec<Result<Vec<u8>, u8>> {
    let mut target = shared.target();
    let mut results = Vec::with_capacity(frame.transfers.len());
    let mut failed = false;
    for transfer in &frame.transfers {
        if failed {
            results.push(Err(proto::PL0_ERR_CONSEQUENTIAL));
            continue;
        }
        shared.stats.transfers.fetch_add(1, Ordering::Relaxed);
        let result = match transfer.map {
            proto::MAP_DEVICE => device_transfer(shared, &mut target, transfer),
            proto::MAP_SERVER => server_transfer(shared, &mut target, transfer),
            // The emulation device (map 1) is not reachable.
            _ => Err(proto::PL0_ERR_DATA),
        };
        if result.is_err() {
            failed = true;
            shared
                .stats
                .failed_transfers
                .fetch_add(1, Ordering::Relaxed);
        }
        results.push(result);
    }
    results
}

fn device_transfer(
    shared: &Shared,
    target: &mut Box<dyn Target>,
    transfer: &Transfer,
) -> Result<Vec<u8>, u8> {
    let run = |target: &mut Box<dyn Target>| {
        shared.ensure_attached(target)?;
        if transfer.is_read() {
            let mut data = vec![0; transfer.read_len];
            target.read(transfer.address, &mut data).map(|()| data)
        } else {
            target
                .write(transfer.address, &transfer.data)
                .map(|()| Vec::new())
        }
    };
    run(target).map_err(|e| {
        if !e.is_access() {
            log::warn!("TAS: {:#010x}: {e}", transfer.address);
        }
        proto::PL0_ERR_DATA
    })
}

/// The server's own address map: the clock and the text blocks tools show.
fn server_transfer(
    shared: &Shared,
    target: &mut Box<dyn Target>,
    transfer: &Transfer,
) -> Result<Vec<u8>, u8> {
    if transfer.address == proto::ACC_HW_FREQUENCY {
        return if transfer.is_read() && transfer.read_len == 4 {
            Ok(target.clock_hz().to_le_bytes().to_vec())
        } else if transfer.data.len() == 4 {
            let d = &transfer.data;
            let hz = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);
            match target.set_clock_hz(hz) {
                Ok(taken) => {
                    log::debug!("TAS: clock {hz} Hz asked, {taken} Hz taken");
                    Ok(Vec::new())
                }
                Err(e) => {
                    log::warn!("TAS: clock {hz} Hz: {e}");
                    Err(proto::PL0_ERR_DATA)
                }
            }
        } else {
            Err(proto::PL0_ERR_DATA)
        };
    }
    if !transfer.is_read() {
        return Err(proto::PL0_ERR_DATA);
    }
    let block = match transfer.address {
        proto::SERVER_INFO_BLOCK => proto::server_info_block(),
        proto::HARDWARE_INFO_BLOCK => proto::hardware_info_block(&shared.identifier),
        proto::DEVICE_INFO_BLOCK => proto::device_info_block(target.jtag_id().unwrap_or(0)),
        _ => return Err(proto::PL0_ERR_DATA),
    };
    if transfer.read_len > block.len() {
        return Err(proto::PL0_ERR_DATA);
    }
    Ok(block[..transfer.read_len].to_vec())
}
