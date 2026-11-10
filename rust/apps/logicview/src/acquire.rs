//! Capturing from the app (UI-1, UI-8): a background thread opens the
//! device (uploading firmware if needed, which takes ~2 s), runs the capture
//! and reports back over a channel. The opened device is kept for the next
//! capture.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use lv_capture::{Capture, CaptureReport, CaptureRequest, Outcome, Progress};
use lv_fx2lafw::{FoundDevice, Fx2lafw, StreamEnd};

pub enum Msg {
    /// Something to show while the device is being opened.
    Status(String),
    /// Samples are flowing into `path`.
    Started { progress: Arc<Progress> },
    /// The capture ended (the file is closed).
    Finished(Result<Box<CaptureReport>, String>),
}

/// An opened device kept between captures.
pub type DeviceSlot = Arc<Mutex<Option<Fx2lafw>>>;

pub struct Session {
    pub rx: Receiver<Msg>,
    pub stop: Arc<AtomicBool>,
    pub progress: Option<Arc<Progress>>,
    pub path: PathBuf,
    pub limit: Option<u64>,
    pub samplerate: u64,
    pub started: Instant,
    pub status: String,
}

impl Session {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Folder for new captures: `%LOCALAPPDATA%\logicview\captures`.
pub fn captures_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("logicview")
        .join("captures")
}

/// `capture-2026-11-10T04-30-00Z.lvcap` in [`captures_dir`].
pub fn new_capture_path() -> std::io::Result<PathBuf> {
    let dir = captures_dir();
    std::fs::create_dir_all(&dir)?;
    let stamp = lv_store::now_rfc3339().replace(':', "-");
    let mut path = dir.join(format!("capture-{stamp}.lvcap"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("capture-{stamp}-{n}.lvcap"));
        n += 1;
    }
    Ok(path)
}

pub fn start(
    found: FoundDevice,
    req: CaptureRequest,
    slot: DeviceSlot,
    ctx: egui::Context,
) -> Session {
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let session = Session {
        rx,
        stop: stop.clone(),
        progress: None,
        path: req.output.clone(),
        limit: req.limit_samples,
        samplerate: req.samplerate,
        started: Instant::now(),
        status: "장치를 여는 중…".into(),
    };
    thread::Builder::new()
        .name("lv-session".into())
        .spawn(move || run(found, req, slot, stop, tx, ctx))
        .expect("spawn session thread");
    session
}

fn run(
    found: FoundDevice,
    req: CaptureRequest,
    slot: DeviceSlot,
    stop: Arc<AtomicBool>,
    tx: Sender<Msg>,
    ctx: egui::Context,
) {
    let send = |m: Msg| {
        let _ = tx.send(m);
        ctx.request_repaint();
    };
    let cached = slot.lock().ok().and_then(|mut s| s.take());
    let dev = match lv_fx2lafw::reuse(cached, &found) {
        Some(d) => d,
        None => {
            if !found.has_firmware {
                send(Msg::Status("펌웨어를 올리는 중… (약 2초)".into()));
            }
            match lv_fx2lafw::open(&found, &lv_fx2lafw::default_firmware_dirs()) {
                Ok(d) => d,
                Err(e) => {
                    send(Msg::Finished(Err(format!("장치를 열 수 없습니다: {e}"))));
                    return;
                }
            }
        }
    };
    if stop.load(Ordering::Relaxed) {
        put_back(&slot, dev);
        send(Msg::Finished(Err("시작하기 전에 중지했습니다".into())));
        return;
    }
    let cap = match Capture::start(dev, req) {
        Ok(c) => c,
        Err(e) => {
            send(Msg::Finished(Err(format!(
                "수집을 시작할 수 없습니다: {e}"
            ))));
            return;
        }
    };
    send(Msg::Started {
        progress: cap.progress_handle(),
    });
    while !cap.is_finished() {
        if stop.load(Ordering::Relaxed) {
            cap.stop();
        }
        thread::sleep(Duration::from_millis(20));
    }
    let (dev, report) = cap.wait();
    // Keep the device unless it went away.
    let gone = matches!(
        &report,
        Ok(r) if r.outcome == Outcome::DeviceError(StreamEnd::Disconnected)
    ) || report.is_err();
    if let Some(d) = dev
        && !gone
    {
        put_back(&slot, d);
    }
    send(Msg::Finished(
        report.map(Box::new).map_err(|e| format!("수집 오류: {e}")),
    ));
}

fn put_back(slot: &DeviceSlot, dev: Fx2lafw) {
    if let Ok(mut s) = slot.lock() {
        *s = Some(dev);
    }
}
