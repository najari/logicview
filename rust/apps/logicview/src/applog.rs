//! The app's logger: lines go to stderr as before and into a ring buffer
//! the settings window shows and saves. The level can be changed while the
//! app runs (`RUST_LOG` sets where it starts: a plain level like `info`).

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use log::{LevelFilter, Log, Metadata, Record};

struct AppLog {
    lines: Mutex<VecDeque<String>>,
    cap: AtomicUsize,
}

static LOGGER: AppLog = AppLog {
    lines: Mutex::new(VecDeque::new()),
    cap: AtomicUsize::new(DEFAULT_LINES),
};

/// Lines kept unless set otherwise.
pub const DEFAULT_LINES: usize = 5000;

impl Log for AppLog {
    fn enabled(&self, m: &Metadata) -> bool {
        // nusb reports transfers that lv-fx2lafw expects to fail as errors;
        // they only help when debugging USB.
        m.level() <= log::max_level()
            && (!m.target().starts_with("nusb") || log::max_level() >= LevelFilter::Debug)
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}",
            lv_store::now_rfc3339(),
            r.level(),
            r.target(),
            r.args()
        );
        let _ = writeln!(std::io::stderr(), "{line}");
        if let Ok(mut l) = self.lines.lock() {
            l.push_back(line);
            let cap = self.cap.load(Ordering::Relaxed).max(1);
            while l.len() > cap {
                l.pop_front();
            }
        }
    }

    fn flush(&self) {}
}

/// Install the logger. `RUST_LOG=info` (a plain level) sets the start level;
/// otherwise warnings and errors are logged.
pub fn init() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| v.split(',').next().and_then(|l| l.parse().ok()))
        .unwrap_or(LevelFilter::Warn);
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(level);
    }
}

pub fn set_level(level: LevelFilter) {
    log::set_max_level(level);
}

pub fn set_capacity(lines: usize) {
    LOGGER.cap.store(lines.max(1), Ordering::Relaxed);
}

/// The kept lines, oldest first.
pub fn lines() -> Vec<String> {
    LOGGER
        .lines
        .lock()
        .map(|l| l.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn clear() {
    if let Ok(mut l) = LOGGER.lines.lock() {
        l.clear();
    }
}

/// Levels offered in the settings, with Korean names.
pub const LEVELS: [(LevelFilter, &str); 5] = [
    (LevelFilter::Error, "오류"),
    (LevelFilter::Warn, "경고"),
    (LevelFilter::Info, "정보"),
    (LevelFilter::Debug, "디버그"),
    (LevelFilter::Trace, "추적"),
];

/// `warn` -> the level (what `app.db` keeps).
pub fn parse_level(s: &str) -> Option<LevelFilter> {
    s.parse().ok()
}
