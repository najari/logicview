//! Bulk streaming of samples (CAP-2, CAP-3, CAP-6).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nusb::MaybeFuture;
use nusb::transfer::{Buffer, Bulk, ControlOut, ControlType, In, Recipient, TransferError};

use crate::Error;
use crate::device::Fx2lafw;
use crate::protocol::*;

#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub samplerate: u64,
    /// Stop after this many samples; `None` runs until `stop` is set.
    pub limit_samples: Option<u64>,
    /// Override the transfer size (bytes, multiple of 512).
    pub transfer_size: Option<usize>,
    /// Override the number of queued transfers.
    pub transfer_count: Option<usize>,
}

impl StreamConfig {
    pub fn new(samplerate: u64, limit_samples: Option<u64>) -> Self {
        Self {
            samplerate,
            limit_samples,
            transfer_size: None,
            transfer_count: None,
        }
    }
}

/// Why streaming ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    /// The requested number of samples arrived.
    LimitReached,
    /// The caller asked to stop.
    Stopped,
    /// The sink refused data (its queue was full).
    SinkFull,
    /// The device stopped sending data.
    DeviceStopped,
    /// A transfer ended with STALL; the halt was cleared afterwards.
    Stalled,
    /// The device went away.
    Disconnected,
}

impl StreamEnd {
    /// True when the capture holds every sample that was asked for.
    pub fn is_complete(self) -> bool {
        matches!(self, StreamEnd::LimitReached | StreamEnd::Stopped)
    }
}

#[derive(Debug, Clone)]
pub struct StreamStats {
    pub end: StreamEnd,
    pub samples: u64,
    pub transfers: u64,
    pub empty_transfers: u64,
    pub transfer_size: usize,
    pub transfer_count: usize,
    /// From the start command to the end.
    pub elapsed: Duration,
    /// From the start command to the first data.
    pub first_data: Option<Duration>,
}

impl Fx2lafw {
    /// Stream samples into `sink` until the limit, `stop`, or an error.
    ///
    /// `sink` gets every chunk of samples as it arrives (one byte per sample,
    /// bit n = channel n) and returns `false` to abort with
    /// [`StreamEnd::SinkFull`]. It runs on the calling thread between
    /// transfer completions, so it must be quick: copy and hand off.
    pub fn stream(
        &mut self,
        cfg: &StreamConfig,
        stop: &AtomicBool,
        mut sink: impl FnMut(&[u8]) -> bool,
    ) -> Result<StreamStats, Error> {
        let cmd = start_command(cfg.samplerate)?;
        let size = cfg
            .transfer_size
            .unwrap_or_else(|| transfer_size(cfg.samplerate));
        let count = cfg
            .transfer_count
            .unwrap_or_else(|| transfer_count(cfg.samplerate));
        let mut ep = self.intf.endpoint::<Bulk, In>(EP_DATA)?;
        let packet = ep.max_packet_size().max(1);
        let size = size.div_ceil(packet) * packet;

        // Queue the transfers first so the first data has somewhere to go (CAP-3).
        for _ in 0..count {
            ep.submit(Buffer::new(size));
        }
        self.intf
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: CMD_START,
                    value: 0,
                    index: 0,
                    data: &cmd,
                },
                Duration::from_millis(500),
            )
            .wait()
            .map_err(|e| {
                ep.cancel_all();
                Error::from(e)
            })?;

        let started = Instant::now();
        // The device has stopped if nothing arrives for this long.
        let transfer_time = Duration::from_secs_f64(size as f64 / cfg.samplerate.max(1) as f64);
        let idle_limit = (transfer_time * 4).max(Duration::from_millis(1000));
        let mut last_data = started;
        let mut stats = StreamStats {
            end: StreamEnd::Stopped,
            samples: 0,
            transfers: 0,
            empty_transfers: 0,
            transfer_size: size,
            transfer_count: count,
            elapsed: Duration::ZERO,
            first_data: None,
        };
        let mut empty_in_row = 0usize;
        let mut stalled = false;

        let end = loop {
            if stop.load(Ordering::Relaxed) {
                break StreamEnd::Stopped;
            }
            let Some(c) = ep.wait_next_complete(Duration::from_millis(50)) else {
                if last_data.elapsed() > idle_limit {
                    break StreamEnd::DeviceStopped;
                }
                continue;
            };
            stats.transfers += 1;
            let mut errored = false;
            match c.status {
                Ok(()) => {}
                Err(TransferError::Disconnected) => break StreamEnd::Disconnected,
                Err(TransferError::Stall) => {
                    stalled = true;
                    break StreamEnd::Stalled;
                }
                Err(TransferError::Cancelled) => break StreamEnd::Stopped,
                Err(e) => {
                    log::warn!("transfer error: {e}");
                    errored = true;
                }
            }
            let data = &c.buffer[..c.actual_len.min(c.buffer.len())];
            if data.is_empty() || errored {
                stats.empty_transfers += 1;
                empty_in_row += 1;
                if empty_in_row > MAX_EMPTY_TRANSFERS {
                    break StreamEnd::DeviceStopped;
                }
            } else {
                empty_in_row = 0;
                last_data = Instant::now();
                stats.first_data.get_or_insert_with(|| started.elapsed());
                let take = match cfg.limit_samples {
                    Some(limit) => (limit - stats.samples).min(data.len() as u64) as usize,
                    None => data.len(),
                };
                if !sink(&data[..take]) {
                    break StreamEnd::SinkFull;
                }
                stats.samples += take as u64;
                if cfg.limit_samples.is_some_and(|l| stats.samples >= l) {
                    break StreamEnd::LimitReached;
                }
            }
            let mut buf = c.buffer;
            buf.clear();
            ep.submit(buf);
        };
        stats.elapsed = started.elapsed();
        stats.end = end;

        ep.cancel_all();
        while ep.pending() > 0 {
            if ep.wait_next_complete(Duration::from_secs(1)).is_none() {
                log::warn!("{} transfers did not return after cancel", ep.pending());
                break;
            }
        }
        if stalled && let Err(e) = ep.clear_halt().wait() {
            log::warn!("clear halt failed: {e}");
        }
        Ok(stats)
    }
}
