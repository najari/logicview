//! The two ways to reach the board: the WCH CH375 driver through
//! `CH375DLL64.DLL` (Windows default) or WinUSB/libusb through nusb.

use std::time::Duration;

use crate::Error;

/// Commands and the sample stream of one opened board.
pub(crate) trait Link: Send {
    /// Send a 64-byte command packet.
    fn write(&mut self, packet: &[u8]) -> Result<(), Error>;
    /// Read one reply packet.
    fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, Error>;
    /// Throw away replies left over from an earlier program or command.
    fn flush_replies(&mut self);
    /// bcdUSB of the link the board is on (0x0300 for SuperSpeed).
    fn bcd_usb(&self) -> u16;
    /// Start receiving samples in blocks of `block` bytes.
    fn start_upload(&mut self, block: usize) -> Result<(), Error>;
    /// The next block of samples, or `None` if none came within `wait`.
    fn next_data(&mut self, wait: Duration) -> Result<Option<&[u8]>, Error>;
    /// Stop receiving and drop what is still buffered.
    fn stop_upload(&mut self);
}

#[cfg(windows)]
pub(crate) mod ch375 {
    //! `CH375DLL64.DLL` comes with the WCH driver (CH375WDM) the board is
    //! bound to by default; it is loaded from the system folder, not shipped.

    use std::ffi::{CStr, c_char, c_void};
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, Instant};

    use libloading::Library;

    use super::Link;
    use crate::Error;
    use crate::protocol::{PACKET_IN, PIPE_CMD, PIPE_LOGIC};

    /// `unsigned long` on Windows.
    type Ul = u32;

    #[allow(non_snake_case)]
    struct Dll {
        _lib: Library,
        OpenDevice: unsafe extern "system" fn(Ul) -> *mut c_void,
        CloseDevice: unsafe extern "system" fn(Ul),
        GetUsbID: unsafe extern "system" fn(Ul) -> Ul,
        GetDeviceName: unsafe extern "system" fn(Ul) -> *const c_char,
        GetDeviceDescr: unsafe extern "system" fn(Ul, *mut c_void, *mut Ul) -> i32,
        SetTimeoutEx: unsafe extern "system" fn(Ul, Ul, Ul, Ul, Ul) -> i32,
        WriteEndP: unsafe extern "system" fn(Ul, Ul, *mut c_void, *mut Ul) -> i32,
        ReadEndP: unsafe extern "system" fn(Ul, Ul, *mut c_void, *mut Ul) -> i32,
        SetBufUploadEx: unsafe extern "system" fn(Ul, Ul, Ul, Ul) -> i32,
        QueryBufUploadEx: unsafe extern "system" fn(Ul, Ul, *mut Ul, *mut Ul) -> i32,
        ClearBufUpload: unsafe extern "system" fn(Ul, Ul) -> i32,
        SetBufDownloadEx: Option<unsafe extern "system" fn(Ul, Ul, Ul, Ul) -> i32>,
        DriverCommand: Option<unsafe extern "system" fn(Ul, *mut c_void) -> i32>,
    }

    /// The DLL keeps one table of devices per process; opening and closing
    /// by index must not interleave.
    static INDEX_LOCK: Mutex<()> = Mutex::new(());

    fn dll() -> Result<&'static Dll, Error> {
        static DLL: OnceLock<Result<Dll, String>> = OnceLock::new();
        DLL.get_or_init(load)
            .as_ref()
            .map_err(|e| Error::Ch375(e.clone()))
    }

    fn load() -> Result<Dll, String> {
        // SAFETY: loading the WCH DLL runs its DllMain, which only sets up
        // its own tables.
        let lib = unsafe { Library::new("CH375DLL64.DLL") }.map_err(|e| {
            format!(
                "CH375DLL64.DLL을 불러올 수 없습니다 (WCH CH375 드라이버가 설치돼 있어야 함): {e}"
            )
        })?;
        macro_rules! sym {
            ($name:literal) => {
                // SAFETY: the signature matches CH375DLL.H.
                *unsafe { lib.get($name) }.map_err(|e| {
                    format!(
                        "CH375DLL64.DLL에 {} 함수가 없습니다: {e}",
                        String::from_utf8_lossy(&$name[..$name.len() - 1])
                    )
                })?
            };
        }
        macro_rules! opt {
            ($name:literal) => {
                // SAFETY: as above.
                unsafe { lib.get($name) }.ok().map(|s| *s)
            };
        }
        Ok(Dll {
            OpenDevice: sym!(b"CH375OpenDevice\0"),
            CloseDevice: sym!(b"CH375CloseDevice\0"),
            GetUsbID: sym!(b"CH375GetUsbID\0"),
            GetDeviceName: sym!(b"CH375GetDeviceName\0"),
            GetDeviceDescr: sym!(b"CH375GetDeviceDescr\0"),
            SetTimeoutEx: sym!(b"CH375SetTimeoutEx\0"),
            WriteEndP: sym!(b"CH375WriteEndP\0"),
            ReadEndP: sym!(b"CH375ReadEndP\0"),
            SetBufUploadEx: sym!(b"CH375SetBufUploadEx\0"),
            QueryBufUploadEx: sym!(b"CH375QueryBufUploadEx\0"),
            ClearBufUpload: sym!(b"CH375ClearBufUpload\0"),
            SetBufDownloadEx: opt!(b"CH375SetBufDownloadEx\0"),
            DriverCommand: opt!(b"CH375DriverCommand\0"),
            _lib: lib,
        })
    }

    /// The serial number in a device path such as
    /// `\\?\usb#vid_1a86&pid_5537#b5e339e3…#{guid}`.
    pub(crate) fn serial_of_path(path: &str) -> Option<&str> {
        let mut parts = path.split('#');
        parts.next()?;
        parts.next()?;
        parts.next().filter(|s| !s.is_empty())
    }

    pub(crate) struct Ch375 {
        index: Ul,
        bcd_usb: u16,
        buf: Vec<u8>,
        /// A failed read is only a lost board once reads keep failing.
        failing_since: Option<Instant>,
    }

    const INVALID: *mut c_void = usize::MAX as *mut c_void;
    const TIMEOUT_MS: Ul = 500;

    impl Ch375 {
        /// Open the board whose serial number is `serial`.
        pub(crate) fn open(serial: &str) -> Result<Ch375, Error> {
            let d = dll()?;
            let _g = INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            for i in 0..16 {
                // SAFETY: plain calls into the DLL with an index.
                let h = unsafe { (d.OpenDevice)(i) };
                if h.is_null() || h == INVALID {
                    continue;
                }
                let id = unsafe { (d.GetUsbID)(i) };
                let name = unsafe { (d.GetDeviceName)(i) };
                let path = if name.is_null() {
                    String::new()
                } else {
                    // SAFETY: the DLL returns a NUL-terminated string.
                    unsafe { CStr::from_ptr(name) }
                        .to_string_lossy()
                        .into_owned()
                };
                let ours = id == (u32::from(crate::PID) << 16 | u32::from(crate::VID))
                    && serial_of_path(&path).is_some_and(|s| s.eq_ignore_ascii_case(serial));
                if !ours {
                    unsafe { (d.CloseDevice)(i) };
                    continue;
                }
                let mut link = Ch375 {
                    index: i,
                    bcd_usb: 0x0300,
                    buf: Vec::new(),
                    failing_since: None,
                };
                link.setup(d);
                return Ok(link);
            }
            Err(Error::Ch375(format!(
                "CH375 드라이버로 {serial} 장치를 열 수 없습니다 (다른 프로그램이 쓰고 있지 않은지 확인)"
            )))
        }

        fn setup(&mut self, d: &Dll) {
            let i = self.index;
            // Synchronous reads, no buffered download for commands (as the
            // vendor driver does).
            if let Some(cmd) = d.DriverCommand {
                #[repr(C, packed)]
                struct Win32Command {
                    function: Ul,
                    length: Ul,
                    buffer: [u8; 64],
                }
                let mut c = Win32Command {
                    function: 0x12, // mFuncSetIOMode
                    length: 1,
                    buffer: [0; 64],
                };
                unsafe { cmd(i, (&raw mut c).cast()) };
            }
            if let Some(f) = d.SetBufDownloadEx {
                unsafe { f(i, 0, Ul::from(PIPE_CMD), 0) };
            }
            let mut descr = [0u8; 18];
            let mut n: Ul = descr.len() as Ul;
            if unsafe { (d.GetDeviceDescr)(i, descr.as_mut_ptr().cast(), &mut n) } != 0 && n >= 4 {
                self.bcd_usb = u16::from_le_bytes([descr[2], descr[3]]);
            }
            unsafe { (d.SetTimeoutEx)(i, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS) };
        }
    }

    impl Drop for Ch375 {
        fn drop(&mut self) {
            if let Ok(d) = dll() {
                let _g = INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                unsafe { (d.CloseDevice)(self.index) };
            }
        }
    }

    impl Link for Ch375 {
        fn write(&mut self, packet: &[u8]) -> Result<(), Error> {
            let d = dll()?;
            let mut p = packet.to_vec();
            let mut n = p.len() as Ul;
            let ok = unsafe {
                (d.WriteEndP)(
                    self.index,
                    Ul::from(PIPE_CMD),
                    p.as_mut_ptr().cast(),
                    &mut n,
                )
            };
            if ok == 0 {
                return Err(Error::Ch375("명령을 보내지 못했습니다".into()));
            }
            Ok(())
        }

        fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, Error> {
            let d = dll()?;
            let ms = timeout.as_millis().clamp(1, 60_000) as Ul;
            let i = self.index;
            if ms != TIMEOUT_MS {
                unsafe { (d.SetTimeoutEx)(i, ms, ms, ms, ms) };
            }
            let mut b = [0u8; PACKET_IN];
            let mut n = b.len() as Ul;
            let ok = unsafe { (d.ReadEndP)(i, Ul::from(PIPE_CMD), b.as_mut_ptr().cast(), &mut n) };
            if ms != TIMEOUT_MS {
                unsafe { (d.SetTimeoutEx)(i, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS) };
            }
            if ok == 0 || n == 0 {
                return Err(Error::Timeout);
            }
            Ok(b[..(n as usize).min(b.len())].to_vec())
        }

        fn flush_replies(&mut self) {
            let Ok(d) = dll() else { return };
            let i = self.index;
            unsafe { (d.SetTimeoutEx)(i, 1, 1, 1, 1) };
            for _ in 0..10 {
                let mut b = [0u8; PACKET_IN];
                let mut n = b.len() as Ul;
                let ok =
                    unsafe { (d.ReadEndP)(i, Ul::from(PIPE_CMD), b.as_mut_ptr().cast(), &mut n) };
                if ok == 0 || n == 0 {
                    break;
                }
                log::debug!("dropped a stale reply {:02x?}", &b[..n as usize]);
            }
            unsafe { (d.SetTimeoutEx)(i, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS) };
        }

        fn bcd_usb(&self) -> u16 {
            self.bcd_usb
        }

        fn start_upload(&mut self, block: usize) -> Result<(), Error> {
            let d = dll()?;
            self.buf.resize(block, 0);
            self.failing_since = None;
            let i = self.index;
            let pipe = Ul::from(PIPE_LOGIC);
            if unsafe { (d.SetBufUploadEx)(i, 1, pipe, block as Ul) } == 0 {
                return Err(Error::Ch375("버퍼 업로드를 켜지 못했습니다".into()));
            }
            unsafe { (d.ClearBufUpload)(i, pipe) };
            Ok(())
        }

        fn next_data(&mut self, wait: Duration) -> Result<Option<&[u8]>, Error> {
            let d = dll()?;
            let i = self.index;
            let pipe = Ul::from(PIPE_LOGIC);
            let deadline = Instant::now() + wait;
            loop {
                let (mut count, mut total): (Ul, Ul) = (0, 0);
                let ok = unsafe { (d.QueryBufUploadEx)(i, pipe, &mut count, &mut total) };
                if ok == 0 {
                    return self.failed("버퍼 상태를 읽지 못했습니다");
                }
                if count > 0 {
                    break;
                }
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                thread::sleep(Duration::from_millis(1));
            }
            let mut n = self.buf.len() as Ul;
            let ok = unsafe { (d.ReadEndP)(i, pipe, self.buf.as_mut_ptr().cast(), &mut n) };
            if ok == 0 {
                // A pulled board keeps reporting a stale count and fails
                // every read.
                return self.failed("샘플을 읽지 못했습니다");
            }
            self.failing_since = None;
            let n = (n as usize).min(self.buf.len());
            Ok(Some(&self.buf[..n]))
        }

        fn stop_upload(&mut self) {
            let Ok(d) = dll() else { return };
            let pipe = Ul::from(PIPE_LOGIC);
            unsafe {
                (d.ClearBufUpload)(self.index, pipe);
                (d.SetBufUploadEx)(self.index, 0, pipe, 0);
            }
        }
    }

    impl Ch375 {
        fn failed(&mut self, what: &str) -> Result<Option<&[u8]>, Error> {
            let since = *self.failing_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_millis(500) {
                return Err(Error::Disconnected(what.into()));
            }
            thread::sleep(Duration::from_millis(1));
            Ok(None)
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn u3_serial_from_device_path() {
            let p = r"\\?\usb#vid_1a86&pid_5537#b5e339e339534f50f8ad7154e6#{5e7f6bdf-1ce5-4d78-bbcf-d20c44329f7d}";
            assert_eq!(super::serial_of_path(p), Some("b5e339e339534f50f8ad7154e6"));
            assert_eq!(super::serial_of_path("nothing"), None);
        }
    }
}

pub(crate) mod usb {
    //! WinUSB (firmware 0.2 binds it once the CH375 driver is removed),
    //! libusb on Linux and macOS.

    use std::time::Duration;

    use nusb::MaybeFuture;
    use nusb::transfer::{Buffer, Bulk, Completion, In, Out, TransferError};

    use super::Link;
    use crate::Error;
    use crate::protocol::{PIPE_CMD, PIPE_LOGIC};

    /// Transfers kept queued on the sample endpoint (the vendor driver keeps
    /// about 8 MB in flight at 200 MHz).
    const QUEUED: usize = 16;

    pub(crate) struct Usb {
        _device: nusb::Device,
        cmd_out: nusb::Endpoint<Bulk, Out>,
        cmd_in: nusb::Endpoint<Bulk, In>,
        data: Option<nusb::Endpoint<Bulk, In>>,
        bcd_usb: u16,
        /// The completion handed out last; resubmitted on the next read.
        done: Option<Completion>,
        block: usize,
    }

    fn transfer(e: TransferError) -> Error {
        match e {
            TransferError::Disconnected => Error::Disconnected("장치가 빠짐".into()),
            e => Error::Transfer(e),
        }
    }

    impl Usb {
        pub(crate) fn open(info: &nusb::DeviceInfo) -> Result<Usb, Error> {
            let device = info.open().wait()?;
            let bcd_usb = device.device_descriptor().usb_version();
            let intf = device.claim_interface(0).wait()?;
            let cmd_out = intf.endpoint::<Bulk, Out>(PIPE_CMD)?;
            let cmd_in = intf.endpoint::<Bulk, In>(0x80 | PIPE_CMD)?;
            let data = intf.endpoint::<Bulk, In>(0x80 | PIPE_LOGIC)?;
            Ok(Usb {
                _device: device,
                cmd_out,
                cmd_in,
                data: Some(data),
                bcd_usb,
                done: None,
                block: 0,
            })
        }
    }

    impl Link for Usb {
        fn write(&mut self, packet: &[u8]) -> Result<(), Error> {
            self.cmd_out.submit(Buffer::from(packet.to_vec()));
            match self.cmd_out.wait_next_complete(Duration::from_millis(500)) {
                Some(c) => c.status.map_err(transfer),
                None => {
                    self.cmd_out.cancel_all();
                    let _ = self.cmd_out.wait_next_complete(Duration::from_millis(100));
                    Err(Error::Timeout)
                }
            }
        }

        fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, Error> {
            // WinUSB fails a read smaller than the packet size.
            let size = self.cmd_in.max_packet_size().max(64);
            self.cmd_in.submit(Buffer::new(size));
            match self.cmd_in.wait_next_complete(timeout) {
                Some(c) => {
                    c.status.map_err(transfer)?;
                    Ok(c.buffer[..c.actual_len.min(c.buffer.len())].to_vec())
                }
                None => {
                    self.cmd_in.cancel_all();
                    let _ = self.cmd_in.wait_next_complete(Duration::from_millis(100));
                    Err(Error::Timeout)
                }
            }
        }

        fn flush_replies(&mut self) {
            for _ in 0..10 {
                match self.read_reply(Duration::from_millis(2)) {
                    Ok(r) if !r.is_empty() => log::debug!("dropped a stale reply {r:02x?}"),
                    _ => break,
                }
            }
        }

        fn bcd_usb(&self) -> u16 {
            self.bcd_usb
        }

        fn start_upload(&mut self, block: usize) -> Result<(), Error> {
            let ep = self.data.as_mut().expect("data endpoint");
            let packet = ep.max_packet_size().max(1);
            self.block = block.div_ceil(packet) * packet;
            self.done = None;
            for _ in 0..QUEUED {
                ep.submit(Buffer::new(self.block));
            }
            Ok(())
        }

        fn next_data(&mut self, wait: Duration) -> Result<Option<&[u8]>, Error> {
            let ep = self.data.as_mut().expect("data endpoint");
            if let Some(c) = self.done.take() {
                let mut b = c.buffer;
                b.clear();
                b.set_requested_len(self.block);
                ep.submit(b);
            }
            let Some(c) = ep.wait_next_complete(wait) else {
                return Ok(None);
            };
            match c.status {
                Ok(()) => {}
                Err(TransferError::Cancelled) => return Ok(None),
                Err(e) => return Err(transfer(e)),
            }
            let c = self.done.insert(c);
            Ok(Some(&c.buffer[..c.actual_len.min(c.buffer.len())]))
        }

        fn stop_upload(&mut self) {
            self.done = None;
            if let Some(ep) = self.data.as_mut() {
                ep.cancel_all();
                while ep.pending() > 0 {
                    if ep.wait_next_complete(Duration::from_secs(1)).is_none() {
                        log::warn!("{} transfers did not return after cancel", ep.pending());
                        break;
                    }
                }
            }
        }
    }
}
