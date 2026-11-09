//! A capture session (CAP-4, CAP-5, CAP-7):
//!
//! ```text
//! USB thread --blocks--> [bounded queue] --> writer thread (chunk, zstd, summary, SQLite)
//! ```
//!
//! The USB thread only copies each completed transfer into a block and hands
//! it over. If the queue is full the capture stops rather than dropping data.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use lv_fx2lafw::{Fx2lafw, StreamConfig, StreamEnd, StreamStats};
use lv_store::{CaptureInfo, CaptureWriter, NewCapture, Status, WriteStats};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Device(#[from] lv_fx2lafw::Error),
    #[error(transparent)]
    Store(#[from] lv_store::Error),
    #[error("capture thread stopped unexpectedly")]
    ThreadPanicked,
}

#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub samplerate: u64,
    pub limit_samples: Option<u64>,
    pub output: PathBuf,
    pub overwrite: bool,
    pub channel_names: Vec<String>,
    /// Queue capacity in bytes (rounded to whole transfers). About 2.5 s at
    /// 24 MHz by default.
    pub queue_bytes: usize,
    pub transfer_size: Option<usize>,
    pub transfer_count: Option<usize>,
}

impl CaptureRequest {
    pub fn new(samplerate: u64, limit_samples: Option<u64>, output: PathBuf) -> Self {
        Self {
            samplerate,
            limit_samples,
            output,
            overwrite: false,
            channel_names: (0..8).map(|i| format!("D{i}")).collect(),
            queue_bytes: 64 << 20,
            transfer_size: None,
            transfer_count: None,
        }
    }
}

/// Live counters, readable from any thread (CAP-7).
#[derive(Debug, Default)]
pub struct Progress {
    pub received: AtomicU64,
    pub written: AtomicU64,
    pub queued: AtomicUsize,
    pub queue_peak: AtomicUsize,
    pub queue_capacity: AtomicUsize,
}

impl Progress {
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            received: self.received.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            queued: self.queued.load(Ordering::Relaxed),
            queue_peak: self.queue_peak.load(Ordering::Relaxed),
            queue_capacity: self.queue_capacity.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProgressSnapshot {
    pub received: u64,
    pub written: u64,
    pub queued: usize,
    pub queue_peak: usize,
    pub queue_capacity: usize,
}

/// How a capture ended, for people.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// All requested samples were stored.
    Complete,
    /// Stopped by the user; everything received was stored.
    Stopped,
    /// Storage could not keep up (CAP-5).
    Overflow,
    /// The device stopped sending, stalled or went away.
    DeviceError(StreamEnd),
}

impl Outcome {
    pub fn describe(self) -> &'static str {
        match self {
            Outcome::Complete => "complete",
            Outcome::Stopped => "stopped by user",
            Outcome::Overflow => "stopped: storage could not keep up with the sample rate",
            Outcome::DeviceError(StreamEnd::DeviceStopped) => {
                "stopped: the device stopped sending data, its buffer overflowed \
                 (this USB port could not keep up: use a lower rate, a port on the PC \
                 without a hub, or another PC port)"
            }
            Outcome::DeviceError(StreamEnd::Stalled) => {
                "stopped: the USB pipe stalled (use a port on the PC without a hub, \
                 or another PC port)"
            }
            Outcome::DeviceError(StreamEnd::Disconnected) => "stopped: the device was unplugged",
            Outcome::DeviceError(_) => "stopped: device error",
        }
    }

    pub fn is_ok(self) -> bool {
        matches!(self, Outcome::Complete | Outcome::Stopped)
    }
}

#[derive(Debug, Clone)]
pub struct CaptureReport {
    pub outcome: Outcome,
    pub stream: StreamStats,
    pub info: CaptureInfo,
    pub write: WriteStats,
    pub queue_peak_bytes: usize,
    pub queue_capacity_bytes: usize,
    /// Time from the end of streaming until the file was closed.
    pub finish_time: Duration,
}

type UsbResult = (Fx2lafw, Result<StreamStats, lv_fx2lafw::Error>);

pub struct Capture {
    stop: Arc<AtomicBool>,
    progress: Arc<Progress>,
    usb: JoinHandle<UsbResult>,
    writer: JoinHandle<Result<(CaptureInfo, WriteStats, Duration), lv_store::Error>>,
    transfer_bytes: usize,
}

impl Capture {
    /// Create the output file and start streaming from `device`.
    pub fn start(mut device: Fx2lafw, req: CaptureRequest) -> Result<Capture, Error> {
        // Validate the rate before touching the file.
        lv_fx2lafw::protocol::start_command(req.samplerate)?;
        let writer = CaptureWriter::create(
            &req.output,
            &NewCapture {
                device: format!("{} {}", device.name, device.location),
                samplerate_hz: req.samplerate,
                channel_names: req.channel_names.clone(),
            },
            req.overwrite,
        )?;

        let cfg = StreamConfig {
            samplerate: req.samplerate,
            limit_samples: req.limit_samples,
            transfer_size: req.transfer_size,
            transfer_count: req.transfer_count,
        };
        let transfer_bytes = cfg
            .transfer_size
            .unwrap_or_else(|| lv_fx2lafw::protocol::transfer_size(req.samplerate));
        let capacity = (req.queue_bytes / transfer_bytes).max(4);

        let stop = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Progress::default());
        progress.queue_capacity.store(capacity, Ordering::Relaxed);
        let (tx, rx) = bounded::<Vec<u8>>(capacity);
        // Emptied blocks come back for reuse.
        let (recycle_tx, recycle_rx) = bounded::<Vec<u8>>(capacity);

        // Set by the USB thread before it closes the queue, so the writer
        // knows whether the capture is complete.
        let ended_ok = Arc::new(AtomicBool::new(false));

        let writer = {
            let progress = progress.clone();
            let stop = stop.clone();
            let ended_ok = ended_ok.clone();
            thread::Builder::new()
                .name("lv-writer".into())
                .spawn(move || writer_loop(writer, rx, recycle_tx, &progress, &stop, &ended_ok))
                .expect("spawn writer thread")
        };
        let usb = {
            let stop = stop.clone();
            let progress = progress.clone();
            let ended_ok = ended_ok.clone();
            thread::Builder::new()
                .name("lv-usb".into())
                .spawn(move || {
                    let r = device.stream(&cfg, &stop, |data| {
                        let mut block = recycle_rx
                            .try_recv()
                            .unwrap_or_else(|_| Vec::with_capacity(transfer_bytes));
                        block.clear();
                        block.extend_from_slice(data);
                        match tx.try_send(block) {
                            Ok(()) => {
                                progress
                                    .received
                                    .fetch_add(data.len() as u64, Ordering::Relaxed);
                                let q = tx.len();
                                progress.queued.store(q, Ordering::Relaxed);
                                progress.queue_peak.fetch_max(q, Ordering::Relaxed);
                                true
                            }
                            Err(TrySendError::Full(_)) => false,
                            Err(TrySendError::Disconnected(_)) => false,
                        }
                    });
                    let ok = r.as_ref().is_ok_and(|s| s.end.is_complete());
                    ended_ok.store(ok, Ordering::SeqCst);
                    drop(tx);
                    (device, r)
                })
                .expect("spawn USB thread")
        };
        Ok(Capture {
            stop,
            progress,
            usb,
            writer,
            transfer_bytes,
        })
    }

    pub fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Ask the capture to stop; [`Capture::wait`] then returns.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.usb.is_finished() && self.writer.is_finished()
    }

    /// Wait for the end. The device is handed back for the next capture.
    pub fn wait(self) -> (Option<Fx2lafw>, Result<CaptureReport, Error>) {
        let (device, stream) = match self.usb.join() {
            Ok((d, s)) => (Some(d), s),
            Err(_) => (
                None,
                Err(lv_fx2lafw::Error::Protocol("USB thread panicked".into())),
            ),
        };
        let written = match self.writer.join() {
            Ok(w) => w,
            Err(_) => return (device, Err(Error::ThreadPanicked)),
        };
        let stream = match stream {
            Ok(s) => s,
            Err(e) => return (device, Err(e.into())),
        };
        let (info, write, finish_time) = match written {
            Ok(w) => w,
            Err(e) => return (device, Err(e.into())),
        };
        let outcome = match stream.end {
            StreamEnd::LimitReached => Outcome::Complete,
            StreamEnd::Stopped => Outcome::Stopped,
            StreamEnd::SinkFull => Outcome::Overflow,
            other => Outcome::DeviceError(other),
        };
        let snap = self.progress.snapshot();
        let report = CaptureReport {
            outcome,
            stream,
            info,
            write,
            queue_peak_bytes: snap.queue_peak * self.transfer_bytes,
            queue_capacity_bytes: snap.queue_capacity * self.transfer_bytes,
            finish_time,
        };
        (device, Ok(report))
    }
}

fn writer_loop(
    mut writer: CaptureWriter,
    rx: Receiver<Vec<u8>>,
    recycle: Sender<Vec<u8>>,
    progress: &Progress,
    stop: &AtomicBool,
    ended_ok: &AtomicBool,
) -> Result<(CaptureInfo, WriteStats, Duration), lv_store::Error> {
    let mut failed = None;
    for block in rx.iter() {
        if failed.is_none() {
            if let Err(e) = writer.push(&block) {
                log::error!("writing {}: {e}", writer.path().display());
                failed = Some(e);
                stop.store(true, Ordering::Relaxed);
            }
            progress.written.store(writer.samples(), Ordering::Relaxed);
        }
        progress.queued.store(rx.len(), Ordering::Relaxed);
        let _ = recycle.try_send(block);
    }
    // The USB thread is done (sender dropped).
    if let Some(e) = failed {
        return Err(e);
    }
    let t = Instant::now();
    let status = if ended_ok.load(Ordering::SeqCst) {
        Status::Complete
    } else {
        Status::Aborted
    };
    let (info, stats) = writer.finish(status)?;
    Ok((info, stats, t.elapsed()))
}
