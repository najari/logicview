//! A capture session (CAP-4, CAP-5, CAP-7):
//!
//! ```text
//! USB thread --blocks--> [bounded queue] --> writer thread (chunk, zstd, summary, SQLite)
//! ```
//!
//! The USB thread only copies each completed transfer into a block and hands
//! it over. If the queue is full the capture stops rather than dropping data.
//! With a trigger (CAP-8) the writer thread looks for it, keeps the samples
//! before it in a ring buffer, and stops the capture once enough samples
//! after it are stored.

pub mod trigger;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use lv_fx2lafw::{Fx2lafw, StreamConfig, StreamEnd, StreamStats};
use lv_store::{CaptureInfo, CaptureWriter, NewCapture, Status, WriteStats};

pub use trigger::{Cond, Trigger};
use trigger::{Matcher, PreBuffer};

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
    /// Start storing at a trigger (CAP-8). `limit_samples` then counts the
    /// stored samples, before and after the trigger.
    pub trigger: Option<Trigger>,
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
            trigger: None,
        }
    }

    /// Samples to keep before the trigger.
    pub fn pre_trigger_samples(&self) -> u64 {
        match (&self.trigger, self.limit_samples) {
            (Some(t), Some(limit)) => limit * u64::from(t.pre_percent) / 100,
            _ => 0,
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
    /// True when no trigger was set, or once it fired.
    pub triggered: AtomicBool,
}

impl Progress {
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            received: self.received.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            queued: self.queued.load(Ordering::Relaxed),
            queue_peak: self.queue_peak.load(Ordering::Relaxed),
            queue_capacity: self.queue_capacity.load(Ordering::Relaxed),
            triggered: self.triggered.load(Ordering::Relaxed),
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
    pub triggered: bool,
}

/// How a capture ended, for people.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// All requested samples were stored.
    Complete,
    /// Stopped by the user; everything received was stored.
    Stopped,
    /// Stopped by the user before the trigger fired; nothing was stored.
    NotTriggered,
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
            Outcome::NotTriggered => "stopped by user before the trigger fired; nothing stored",
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
    /// Where the trigger is in the stored capture (sample index).
    pub trigger_sample: Option<u64>,
}

type UsbResult = (Fx2lafw, Result<StreamStats, lv_fx2lafw::Error>);

/// What the writer thread reports when the file is closed.
struct WriterDone {
    info: CaptureInfo,
    stats: WriteStats,
    finish_time: Duration,
    /// Trigger position in the file, once it fired.
    trigger_sample: Option<u64>,
    /// With a trigger: the sample count was reached and the writer stopped
    /// the capture.
    reached_limit: bool,
}

/// Trigger settings handed to the writer thread.
struct TriggerRun {
    trigger: Trigger,
    matcher: Matcher,
    pre: PreBuffer,
    limit: Option<u64>,
}

pub struct Capture {
    stop: Arc<AtomicBool>,
    progress: Arc<Progress>,
    usb: JoinHandle<UsbResult>,
    writer: JoinHandle<Result<WriterDone, lv_store::Error>>,
    transfer_bytes: usize,
    triggered_mode: bool,
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

        let trigger = req.trigger.clone().map(|t| TriggerRun {
            matcher: Matcher::new(&t),
            pre: PreBuffer::new(req.pre_trigger_samples() as usize),
            trigger: t,
            limit: req.limit_samples,
        });
        let triggered_mode = trigger.is_some();
        let cfg = StreamConfig {
            samplerate: req.samplerate,
            // With a trigger the writer decides when there is enough.
            limit_samples: if triggered_mode {
                None
            } else {
                req.limit_samples
            },
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
        progress.triggered.store(!triggered_mode, Ordering::Relaxed);
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
                .spawn(move || {
                    writer_loop(writer, rx, recycle_tx, &progress, &stop, &ended_ok, trigger)
                })
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
            triggered_mode,
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
        let done = match written {
            Ok(w) => w,
            Err(e) => return (device, Err(e.into())),
        };
        let outcome = match stream.end {
            _ if done.reached_limit => Outcome::Complete,
            StreamEnd::LimitReached => Outcome::Complete,
            StreamEnd::Stopped if self.triggered_mode && done.trigger_sample.is_none() => {
                Outcome::NotTriggered
            }
            StreamEnd::Stopped => Outcome::Stopped,
            StreamEnd::SinkFull => Outcome::Overflow,
            other => Outcome::DeviceError(other),
        };
        let snap = self.progress.snapshot();
        let report = CaptureReport {
            outcome,
            stream,
            info: done.info,
            write: done.stats,
            queue_peak_bytes: snap.queue_peak * self.transfer_bytes,
            queue_capacity_bytes: snap.queue_capacity * self.transfer_bytes,
            finish_time: done.finish_time,
            trigger_sample: done.trigger_sample,
        };
        (device, Ok(report))
    }
}

/// Decides which part of the stream goes into the file.
struct Sink {
    writer: CaptureWriter,
    trigger: Option<TriggerRun>,
    trigger_sample: Option<u64>,
    /// With a trigger: the sample count is stored, ignore the rest.
    done: bool,
}

impl Sink {
    fn new(writer: CaptureWriter, trigger: Option<TriggerRun>) -> Sink {
        Sink {
            writer,
            trigger,
            trigger_sample: None,
            done: false,
        }
    }

    fn feed(&mut self, block: &[u8]) -> Result<(), lv_store::Error> {
        if self.done {
            return Ok(());
        }
        let Some(t) = &mut self.trigger else {
            return self.writer.push(block);
        };
        let mut block = block;
        if self.trigger_sample.is_none() {
            let Some(i) = t.matcher.find(block) else {
                t.pre.push(block);
                return Ok(());
            };
            t.pre.push(&block[..i]);
            let (a, b) = t.pre.as_slices();
            self.writer.push(a)?;
            self.writer.push(b)?;
            let at = t.pre.len() as u64;
            self.writer.set_trigger(&t.trigger.to_json(at))?;
            self.trigger_sample = Some(at);
            t.pre = PreBuffer::new(0);
            block = &block[i..];
        }
        let take = match t.limit {
            Some(l) => (l.saturating_sub(self.writer.samples())).min(block.len() as u64) as usize,
            None => block.len(),
        };
        self.writer.push(&block[..take])?;
        if t.limit.is_some_and(|l| self.writer.samples() >= l) {
            self.done = true;
        }
        Ok(())
    }

    fn finish(self, usb_ended_ok: bool) -> Result<WriterDone, lv_store::Error> {
        let t = Instant::now();
        let fired = self.trigger.is_none() || self.trigger_sample.is_some();
        let status = if self.done || (usb_ended_ok && fired) {
            Status::Complete
        } else {
            Status::Aborted
        };
        let (info, stats) = self.writer.finish(status)?;
        Ok(WriterDone {
            info,
            stats,
            finish_time: t.elapsed(),
            trigger_sample: self.trigger_sample,
            reached_limit: self.done,
        })
    }
}

fn writer_loop(
    writer: CaptureWriter,
    rx: Receiver<Vec<u8>>,
    recycle: Sender<Vec<u8>>,
    progress: &Progress,
    stop: &AtomicBool,
    ended_ok: &AtomicBool,
    trigger: Option<TriggerRun>,
) -> Result<WriterDone, lv_store::Error> {
    let mut sink = Sink::new(writer, trigger);
    let mut failed = None;
    for block in rx.iter() {
        if failed.is_none() && !sink.done {
            if let Err(e) = sink.feed(&block) {
                log::error!("writing {}: {e}", sink.writer.path().display());
                failed = Some(e);
                stop.store(true, Ordering::Relaxed);
            }
            progress
                .written
                .store(sink.writer.samples(), Ordering::Relaxed);
            if sink.trigger_sample.is_some() {
                progress.triggered.store(true, Ordering::Relaxed);
            }
            if sink.done {
                // Enough after the trigger: end the stream.
                stop.store(true, Ordering::Relaxed);
            }
        }
        progress.queued.store(rx.len(), Ordering::Relaxed);
        let _ = recycle.try_send(block);
    }
    // The USB thread is done (sender dropped).
    if let Some(e) = failed {
        return Err(e);
    }
    sink.finish(ended_ok.load(Ordering::SeqCst))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lv_store::CaptureFile;

    fn sink(name: &str, trigger: Option<(&str, u8)>, limit: Option<u64>) -> Sink {
        let dir = std::env::temp_dir().join("lv-capture-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut req = CaptureRequest::new(1_000_000, limit, path.clone());
        req.trigger = trigger.map(|(s, pre)| Trigger::parse(s, pre).unwrap());
        let writer = CaptureWriter::create(
            &path,
            &NewCapture {
                device: "test".into(),
                samplerate_hz: req.samplerate,
                channel_names: req.channel_names.clone(),
            },
            true,
        )
        .unwrap();
        let run = req.trigger.clone().map(|t| TriggerRun {
            matcher: Matcher::new(&t),
            pre: PreBuffer::new(req.pre_trigger_samples() as usize),
            trigger: t,
            limit,
        });
        Sink::new(writer, run)
    }

    /// Sample i: D0 rises at 1000, D1 is bit 0 of the block number.
    fn stream() -> Vec<Vec<u8>> {
        (0..10)
            .map(|blk| {
                (0..300)
                    .map(|j| {
                        let i = blk * 300 + j;
                        u8::from(i >= 1000) | ((blk & 1) as u8) << 1
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn cap8_pre_and_post_trigger() {
        // 400 samples, 25% (100) before the rising edge of D0 at 1000.
        let mut s = sink("trigger.lvcap", Some(("0=r", 25)), Some(400));
        for b in stream() {
            s.feed(&b).unwrap();
        }
        assert!(s.done);
        let path = s.writer.path().to_path_buf();
        let done = s.finish(true).unwrap();
        assert_eq!(done.trigger_sample, Some(100));
        assert_eq!(done.info.num_samples, 400);
        assert_eq!(done.info.status, Status::Complete);
        let f = CaptureFile::open(&path).unwrap();
        let data = f.read_samples(0, 400).unwrap();
        let all: Vec<u8> = stream().concat();
        assert_eq!(data, all[900..1300]);
        assert_eq!(
            f.info.trigger.as_deref(),
            Some(r#"{"conditions":"0=r","pre_percent":25,"sample":100}"#)
        );
    }

    #[test]
    fn cap8_short_pre_trigger_is_filled_after() {
        // Wants 1500 before the trigger but only 1000 samples came first.
        let mut s = sink("trigger-short.lvcap", Some(("0=r", 75)), Some(2000));
        for b in stream() {
            s.feed(&b).unwrap();
        }
        let done = s.finish(true).unwrap();
        assert_eq!(done.trigger_sample, Some(1000));
        assert_eq!(done.info.num_samples, 2000);
    }

    #[test]
    fn cap8_manual_stop_before_trigger() {
        let mut s = sink("trigger-none.lvcap", Some(("2=1", 10)), Some(100));
        for b in stream() {
            s.feed(&b).unwrap();
        }
        assert!(!s.done);
        let done = s.finish(true).unwrap();
        assert_eq!(done.trigger_sample, None);
        assert_eq!(done.info.num_samples, 0);
        assert_eq!(done.info.status, Status::Aborted);
    }

    #[test]
    fn cap2_without_trigger_stores_everything() {
        let mut s = sink("plain.lvcap", None, None);
        for b in stream() {
            s.feed(&b).unwrap();
        }
        let done = s.finish(true).unwrap();
        assert_eq!(done.info.num_samples, 3000);
        assert_eq!(done.info.status, Status::Complete);
        assert!(!done.reached_limit);
    }
}
