//! The main window (M2): capture from the device, open and save files, zoom
//! and pan the waveform, two markers, channel names and visibility.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Key, Rect, Sense, Shape, Stroke, pos2, vec2,
};
use lv_capture::{CaptureRequest, Outcome, Trigger};
use lv_fx2lafw::{FoundDevice, SAMPLERATES};

use crate::acquire::{self, DeviceSlot, Msg, Session};
use crate::bench::Bench;
use crate::capture_view::CaptureView;
use crate::decode_ui::{self, Analyzer, DecodeUi, TableAction, TableUi};
use crate::settings::{AppDb, Remembered};
use crate::wave::{self, Row, View, WaveStats};

/// Channel colours in resistor colour-code order, as sigrok tools use.
const COLORS: [Color32; 8] = [
    Color32::from_rgb(0xbf, 0xbf, 0xbf),
    Color32::from_rgb(0xb2, 0x7d, 0x52),
    Color32::from_rgb(0xef, 0x4f, 0x4f),
    Color32::from_rgb(0xf5, 0x9c, 0x3a),
    Color32::from_rgb(0xe8, 0xd4, 0x4d),
    Color32::from_rgb(0x6c, 0xc2, 0x4a),
    Color32::from_rgb(0x4f, 0x9d, 0xef),
    Color32::from_rgb(0xb0, 0x7c, 0xe8),
];
const MARKER_COLORS: [Color32; 2] = [
    Color32::from_rgb(0xff, 0xd0, 0x40),
    Color32::from_rgb(0x50, 0xe0, 0xf0),
];
const TRIGGER_COLOR: Color32 = Color32::from_rgb(0xff, 0x60, 0x60);

const LABEL_W: f32 = 140.0;
const RULER_H: f32 = 26.0;
/// Most zoomed in: 64 points per sample.
const MIN_SPP: f64 = 1.0 / 64.0;
/// Capture lengths in seconds offered in the toolbar; `None` = until stopped.
const DURATION_CHOICES: [Option<f64>; 9] = [
    Some(0.1),
    Some(0.5),
    Some(1.0),
    Some(2.0),
    Some(5.0),
    Some(10.0),
    Some(30.0),
    Some(60.0),
    None,
];
/// How often the live view re-reads the file (UI-8).
const LIVE_REFRESH: Duration = Duration::from_millis(100);
/// Redraw interval while capturing (at most 30 times a second).
const LIVE_REPAINT: Duration = Duration::from_millis(33);
/// A marker snaps to an edge closer than this.
const SNAP_PX: f64 = 8.0;
const DEVICE_SCAN: Duration = Duration::from_secs(2);

pub struct App {
    capture: Option<CaptureView>,
    view: View,
    error: Option<String>,
    /// Result of the last action (capture, save), shown in the toolbar.
    message: Option<String>,
    /// (wave build ms, egui cpu ms) of recent frames.
    timings: VecDeque<(f64, f64)>,
    last_stats: WaveStats,
    plot_width: f32,
    fit_pending: bool,
    /// Status readout from the previous frame (the status bar is laid out
    /// before the waveform).
    cursor_text: Option<String>,
    bench: Option<Bench>,

    // Capture settings and state (UI-1).
    devices: Vec<FoundDevice>,
    dev_sel: usize,
    last_scan: Option<Instant>,
    /// A device scan running on another thread (USB enumeration can take
    /// tens of milliseconds, too long for the UI thread).
    scan_rx: Option<std::sync::mpsc::Receiver<Vec<FoundDevice>>>,
    slot: DeviceSlot,
    rate: u64,
    /// Capture length in seconds; `None` = until stopped.
    duration: Option<f64>,
    trigger_on: bool,
    trigger_text: String,
    pre_percent: u8,
    session: Option<Session>,
    last_refresh: Instant,
    /// Keep the whole capture in view while recording, until the user zooms.
    follow: bool,

    /// Marker A and B as sample positions (UI-5).
    markers: [Option<f64>; 2],

    // Channels (UI-6): names and visibility, also used for the next capture.
    show_channels: bool,
    names: Vec<String>,
    shown: Vec<bool>,

    // Decoders (M3).
    analyzers: Vec<Analyzer>,
    decode: DecodeUi,
    table: TableUi,
    /// A frame picked in the data table.
    highlight: Option<(u64, u64)>,

    // MCP server inside the app (AUT-3).
    mcp_cfg: lv_mcp::Config,
    mcp_addr: (String, u16),
    mcp: Option<lv_mcp::HttpHandle>,

    /// `app.db`: last settings (UI-10) and decoder presets.
    db: AppDb,
    /// The settings as last written to `db`.
    saved: Remembered,
}

pub fn setup_fonts(ctx: &egui::Context) {
    // UI-7: Malgun Gothic for Hangul. eframe also falls back to system
    // fonts, but a fixed font keeps the look the same on every PC.
    let mut fonts = egui::FontDefinitions::default();
    let windir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    if let Ok(bytes) = std::fs::read(windir.join(r"Fonts\malgun.ttf")) {
        fonts
            .font_data
            .insert("malgun".into(), Arc::new(egui::FontData::from_owned(bytes)));
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push("malgun".into());
        }
    } else {
        log::warn!("malgun.ttf not found; using the system font fallback for Hangul");
    }
    ctx.set_fonts(fonts);
}

/// Capture settings given on the command line.
#[derive(Default)]
pub struct Preset {
    pub rate: Option<u64>,
    /// Capture length in seconds; `Some(None)` = manual stop.
    pub duration: Option<Option<f64>>,
    /// Sample count (`--samples`), turned into a length at the final rate.
    pub samples: Option<Option<u64>>,
    pub trigger: Option<String>,
    /// Marker A and B (samples).
    pub markers: Vec<f64>,
}

fn default_names() -> Vec<String> {
    (0..8).map(|i| format!("D{i}")).collect()
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        path: Option<PathBuf>,
        bench: bool,
        initial_view: Option<View>,
        preset: Preset,
        mcp: (lv_mcp::Config, String, u16),
    ) -> Self {
        let (mcp_cfg, mcp_host, mcp_port) = mcp;
        setup_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut app = Self {
            capture: None,
            view: View {
                start: 0.0,
                spp: 1.0,
            },
            error: None,
            message: None,
            timings: VecDeque::new(),
            last_stats: WaveStats::default(),
            plot_width: 1000.0,
            fit_pending: true,
            cursor_text: None,
            bench: bench.then(Bench::default),
            devices: Vec::new(),
            dev_sel: 0,
            last_scan: None,
            scan_rx: None,
            slot: Arc::new(Mutex::new(None)),
            rate: 16_000_000,
            duration: Some(1.0),
            trigger_on: false,
            trigger_text: "0=f".into(),
            pre_percent: 10,
            session: None,
            last_refresh: Instant::now(),
            follow: false,
            markers: [None; 2],
            show_channels: false,
            names: default_names(),
            shown: vec![true; 8],
            analyzers: Vec::new(),
            decode: DecodeUi::default(),
            table: TableUi::default(),
            highlight: None,
            mcp_cfg,
            mcp_addr: (mcp_host, mcp_port),
            mcp: None,
            db: AppDb::open_default(),
            saved: Remembered {
                rate: 0,
                duration: None,
                names: Vec::new(),
                shown: Vec::new(),
                trigger_text: String::new(),
                pre_percent: 0,
            },
        };
        // MCP captures and the toolbar share one opened device.
        app.mcp_cfg.device_slot = app.slot.clone();
        // UI-10: the last settings, then the command line on top.
        let last = Remembered::load(&app.db, app.remembered());
        app.apply(&last);
        if let Some(r) = preset.rate.filter(|r| SAMPLERATES.contains(r)) {
            app.rate = r;
        }
        if let Some(d) = preset.duration {
            app.duration = d;
        } else if let Some(n) = preset.samples {
            app.duration = n.map(|n| n as f64 / app.rate as f64);
        }
        if let Some(t) = preset.trigger {
            app.trigger_on = true;
            app.trigger_text = t;
        }
        if let Some(p) = path {
            app.open(&p);
        }
        if let Some(v) = initial_view {
            app.view = v;
            app.fit_pending = false;
        }
        for (slot, &m) in app.markers.iter_mut().zip(&preset.markers) {
            *slot = Some(m);
        }
        app.saved = app.remembered();
        app
    }

    // ---- remembered settings (UI-10) ----------------------------------------

    fn remembered(&self) -> Remembered {
        Remembered {
            rate: self.rate,
            duration: self.duration,
            names: self.names.clone(),
            shown: self.shown.clone(),
            trigger_text: self.trigger_text.clone(),
            pre_percent: self.pre_percent,
        }
    }

    fn apply(&mut self, r: &Remembered) {
        self.rate = r.rate;
        self.duration = r.duration;
        self.names = r.names.clone();
        self.shown = r.shown.clone();
        self.trigger_text = r.trigger_text.clone();
        self.pre_percent = r.pre_percent;
    }

    /// Write changed settings, once no text field is being edited (and on
    /// exit, see `Drop`).
    fn remember(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() {
            return;
        }
        self.save_settings();
    }

    fn save_settings(&mut self) {
        let now = self.remembered();
        if now == self.saved {
            return;
        }
        if let Err(e) = now.save(&self.db) {
            log::warn!("saving settings: {e}");
        }
        self.saved = now;
    }

    // ---- files -------------------------------------------------------------

    fn open(&mut self, path: &Path) {
        if self.session.is_some() {
            self.message = Some("수집 중에는 다른 파일을 열 수 없습니다".into());
            return;
        }
        let is_sr = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("sr"));
        if is_sr {
            // sigrok session (STO-5): convert to a .lvcap in the temp folder.
            let dir = std::env::temp_dir().join("logicview");
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let out = dir.join(format!("{stem}.lvcap"));
            let converted = std::fs::create_dir_all(&dir)
                .map_err(|e| lv_store::Error::Io(dir.clone(), e))
                .and_then(|_| lv_store::sigrok::import(path, &out, true));
            match converted {
                Ok(_) => return self.open(&out),
                Err(e) => {
                    self.error = Some(format!("{}을(를) 불러올 수 없습니다: {e}", path.display()));
                    return;
                }
            }
        }
        match CaptureView::open(path) {
            Ok(cv) => {
                self.set_capture(cv);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{}을(를) 열 수 없습니다: {e}", path.display())),
        }
    }

    /// Show `cv`, taking its channel names and visibility.
    fn set_capture(&mut self, cv: CaptureView) {
        let info = cv.info();
        self.names = default_names();
        self.shown = vec![true; 8];
        for (i, n) in info.channel_names.iter().take(8).enumerate() {
            self.names[i] = n.clone();
        }
        for (i, &e) in info.channel_enabled.iter().take(8).enumerate() {
            self.shown[i] = e;
        }
        self.analyzers = decode_ui::load_analyzers(cv.file());
        self.capture = Some(cv);
        self.markers = [None; 2];
        self.highlight = None;
        self.fit_pending = true;
    }

    fn reload_analyzers(&mut self) {
        if let Some(cv) = &self.capture {
            self.analyzers = decode_ui::load_analyzers(cv.file());
        }
    }

    /// Show samples `[s, e]` (a frame picked in the table).
    fn jump_to(&mut self, s: u64, e: u64) {
        let len = (e.saturating_sub(s)).max(1) as f64;
        let w = self.plot_width.max(1.0) as f64;
        let px = len / self.view.spp;
        if px > w * 0.8 || px < 4.0 {
            self.view.spp = (len * 3.0 / w).max(MIN_SPP);
        }
        let mid = (s + e) as f64 / 2.0;
        self.view.start = mid - w / 2.0 * self.view.spp;
        self.follow = false;
        self.clamp_view();
        self.highlight = Some((s, e));
    }

    fn save_dialog(&mut self) {
        let Some(cv) = &self.capture else {
            return;
        };
        let stem = cv
            .path()
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "capture".into());
        let Some(dest) = rfd::FileDialog::new()
            .set_file_name(format!("{stem}.lvcap"))
            .add_filter("logicview 캡처", &["lvcap"])
            .add_filter("sigrok 세션 (PulseView)", &["sr"])
            .add_filter("CSV (보이는 채널, 변화 지점)", &["csv"])
            .save_file()
        else {
            return;
        };
        let ext = dest
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let dest = if ext.is_empty() {
            dest.with_extension("lvcap")
        } else {
            dest
        };
        if dest == cv.path() {
            self.message = Some("이미 이 파일에 저장되어 있습니다".into());
            return;
        }
        // The dialog already asked about overwriting.
        let result = (|| -> lv_store::Result<()> {
            match ext.as_str() {
                "sr" => {
                    lv_store::sigrok::export(cv.file(), &dest)?;
                }
                "csv" => {
                    let chans: Vec<usize> = (0..cv.info().num_channels.min(8))
                        .filter(|&c| self.shown[c])
                        .collect();
                    lv_store::export::csv(cv.file(), &dest, &chans)?;
                }
                _ => {
                    if dest.exists() {
                        std::fs::remove_file(&dest)
                            .map_err(|e| lv_store::Error::Io(dest.clone(), e))?;
                    }
                    cv.file().save_copy(&dest)?;
                }
            }
            Ok(())
        })();
        self.message = Some(match result {
            Ok(()) => format!("저장했습니다: {}", dest.display()),
            Err(e) => format!("저장하지 못했습니다: {e}"),
        });
    }

    /// Write names and visibility into the open file (not while recording).
    fn persist_channels(&mut self) {
        let Some(cv) = &self.capture else {
            return;
        };
        if cv.is_recording() {
            return;
        }
        if let Err(e) = lv_store::update_channels(cv.path(), &self.names, &self.shown) {
            self.message = Some(format!("채널 설정을 파일에 쓰지 못했습니다: {e}"));
        }
    }

    // ---- capture -----------------------------------------------------------

    /// Start a device scan in the background (DEV-1).
    fn scan_devices(&mut self, ctx: &egui::Context) {
        if self.scan_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            match lv_fx2lafw::list_devices() {
                Ok(list) => {
                    let _ = tx.send(list);
                }
                Err(e) => log::warn!("listing devices: {e}"),
            }
            ctx.request_repaint();
        });
        self.scan_rx = Some(rx);
        self.last_scan = Some(Instant::now());
    }

    fn poll_scan(&mut self) {
        let Some(rx) = &self.scan_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(list) => {
                let keep = self.devices.get(self.dev_sel).map(|d| d.location.clone());
                self.dev_sel = keep
                    .and_then(|loc| list.iter().position(|d| d.location == loc))
                    .unwrap_or(0);
                self.devices = list;
                self.scan_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.scan_rx = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    fn start_capture(&mut self, ctx: &egui::Context) {
        let Some(found) = self.devices.get(self.dev_sel).cloned() else {
            self.message = Some("장치가 없습니다. 애널라이저를 꽂고 '다시 찾기'를 누르세요".into());
            return;
        };
        let trigger = if self.trigger_on {
            match Trigger::parse(&self.trigger_text, self.pre_percent) {
                Ok(t) => Some(t),
                Err(e) => {
                    self.message = Some(format!("트리거: {e}"));
                    return;
                }
            }
        } else {
            None
        };
        let path = match acquire::new_capture_path() {
            Ok(p) => p,
            Err(e) => {
                self.message = Some(format!(
                    "{}에 캡처 파일을 만들 수 없습니다: {e}",
                    acquire::captures_dir().display()
                ));
                return;
            }
        };
        let limit = self.duration.map(|s| samples_for(self.rate, s));
        let mut req = CaptureRequest::new(self.rate, limit, path);
        req.channel_names = self.names.clone();
        req.trigger = trigger;
        self.message = None;
        self.error = None;
        self.session = Some(acquire::start(found, req, self.slot.clone(), ctx.clone()));
    }

    /// Handle messages from the capture thread and keep the live view fresh.
    fn poll_session(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.session else {
            return;
        };
        let mut started = false;
        let mut finished = None;
        while let Ok(m) = s.rx.try_recv() {
            match m {
                Msg::Status(t) => s.status = t,
                Msg::Started { progress } => {
                    s.progress = Some(progress);
                    started = true;
                }
                Msg::Finished(r) => finished = Some(r),
            }
        }
        let path = s.path.clone();
        let limit = s.limit;
        if started {
            match CaptureView::open(&path) {
                Ok(cv) => {
                    self.set_capture(cv);
                    self.follow = true;
                }
                Err(e) => self.error = Some(format!("수집 중인 파일을 열 수 없습니다: {e}")),
            }
            self.last_refresh = Instant::now();
        }
        if let Some(r) = finished {
            self.session = None;
            self.finish_capture(&path, r);
            return;
        }
        // Live view (UI-8).
        if let Some(cv) = &mut self.capture
            && cv.path() == path
            && self.last_refresh.elapsed() >= LIVE_REFRESH
        {
            if let Err(e) = cv.refresh() {
                log::warn!("live refresh: {e}");
            }
            self.last_refresh = Instant::now();
            if self.follow {
                let n = cv.num_samples().max(1) as f64;
                let total = limit.map(|l| l as f64).unwrap_or(n);
                self.view = View {
                    start: 0.0,
                    spp: (total / self.plot_width.max(1.0) as f64).max(MIN_SPP),
                };
            }
        }
        ctx.request_repaint_after(LIVE_REPAINT);
    }

    fn finish_capture(
        &mut self,
        path: &Path,
        result: Result<Box<lv_capture::CaptureReport>, String>,
    ) {
        self.follow = false;
        let is_this = self.capture.as_ref().is_some_and(|c| c.path() == path);
        let r = match result {
            Ok(r) => r,
            Err(e) => {
                self.message = Some(e);
                if is_this && self.capture.as_ref().is_some_and(|c| c.num_samples() == 0) {
                    self.capture = None;
                    let _ = std::fs::remove_file(path);
                }
                return;
            }
        };
        if r.outcome == Outcome::NotTriggered {
            if is_this {
                self.capture = None;
            }
            let _ = std::fs::remove_file(path);
            self.message = Some(r.outcome.describe().to_string());
            return;
        }
        // Close our read connection so the WAL can be folded in, then reopen:
        // the file now has its final sample count and summaries.
        if is_this {
            self.capture = None;
        }
        if let Err(e) = lv_store::fold_wal(path) {
            log::info!("{}: keeping WAL ({e})", path.display());
        }
        let names = self.names.clone();
        let shown = self.shown.clone();
        if shown.iter().any(|s| !s)
            && let Err(e) = lv_store::update_channels(path, &names, &shown)
        {
            log::warn!("saving channel visibility: {e}");
        }
        match CaptureView::open(path) {
            Ok(cv) => self.set_capture(cv),
            Err(e) => self.error = Some(format!("{}을(를) 열 수 없습니다: {e}", path.display())),
        }
        let secs = r.stream.elapsed.as_secs_f64();
        let mut msg = format!(
            "{}  ·  {} 샘플  ·  {:.3} s",
            r.outcome.describe(),
            fmt_count(r.info.num_samples),
            secs
        );
        if let Some(t) = r.trigger_sample {
            msg.push_str(&format!("  ·  트리거 샘플 {}", fmt_count(t)));
        }
        self.message = Some(msg);
    }

    fn session_text(&self) -> Option<String> {
        let s = self.session.as_ref()?;
        let el = s.started.elapsed().as_secs_f64();
        let Some(p) = &s.progress else {
            return Some(s.status.clone());
        };
        let p = p.snapshot();
        let rate = s.samplerate.max(1) as f64;
        Some(if !p.triggered {
            format!(
                "트리거 대기  ·  받음 {}  ·  {:.1} s",
                fmt_count(p.received),
                el
            )
        } else {
            let pct = s
                .limit
                .map(|l| format!(" ({:.0}%)", 100.0 * p.written as f64 / l.max(1) as f64))
                .unwrap_or_default();
            format!(
                "수집 중  ·  {} 샘플{pct}  ·  {:.2} s 분량  ·  대기열 {:.0}%",
                fmt_count(p.written),
                p.written as f64 / rate,
                100.0 * p.queued as f64 / p.queue_capacity.max(1) as f64
            )
        })
    }

    // ---- view --------------------------------------------------------------

    fn fit(&mut self) {
        if let Some(cv) = &self.capture {
            let n = cv.num_samples().max(1) as f64;
            self.view = View {
                start: 0.0,
                spp: (n / self.plot_width.max(1.0) as f64).max(MIN_SPP),
            };
        }
    }

    fn max_spp(&self) -> f64 {
        let n = self
            .capture
            .as_ref()
            .map(|c| c.num_samples())
            .unwrap_or(1)
            .max(1) as f64;
        (n / self.plot_width.max(1.0) as f64 * 1.1).max(MIN_SPP)
    }

    fn clamp_view(&mut self) {
        let n = self.capture.as_ref().map(|c| c.num_samples()).unwrap_or(0) as f64;
        self.view.spp = self.view.spp.clamp(MIN_SPP, self.max_spp());
        let visible = self.plot_width as f64 * self.view.spp;
        let margin = visible * 0.05;
        let max_start = (n - visible + margin).max(-margin);
        self.view.start = self.view.start.clamp(-margin, max_start);
    }

    /// Zoom by `factor` (>1 = zoom in) keeping the sample under `x` still.
    fn zoom(&mut self, factor: f64, x: f32) {
        self.follow = false;
        let anchor = self.view.sample_at(x);
        self.view.spp /= factor;
        self.view.spp = self.view.spp.clamp(MIN_SPP, self.max_spp());
        self.view.start = anchor - x as f64 * self.view.spp;
        self.clamp_view();
    }

    /// Start or stop the MCP HTTP server (AUT-3).
    fn mcp_toggle(&mut self, ui: &mut egui::Ui) {
        let mut on = self.mcp.is_some();
        let (host, port) = self.mcp_addr.clone();
        let url = format!("http://{host}:{port}");
        let r = ui.toggle_value(&mut on, "MCP").on_hover_text(format!(
            "MCP 서버 (Saleae Logic 2 도구). Claude Code 등록:\nclaude mcp add --transport http logicview {url}"
        ));
        if !r.changed() {
            return;
        }
        if on {
            let started = lv_mcp::Server::new(self.mcp_cfg.clone())
                .map_err(|e| e.to_string())
                .and_then(|s| {
                    lv_mcp::start_http(Arc::new(s), &host, port).map_err(|e| e.to_string())
                });
            match started {
                Ok(h) => {
                    self.message = Some(format!("MCP 서버: http://{}", h.addr));
                    self.mcp = Some(h);
                }
                Err(e) => self.message = Some(format!("MCP 서버를 열 수 없습니다 ({url}): {e}")),
            }
        } else if let Some(h) = self.mcp.take() {
            h.stop();
            self.message = Some("MCP 서버를 껐습니다".into());
        }
    }

    // ---- toolbars ----------------------------------------------------------

    fn capture_bar(&mut self, ui: &mut egui::Ui) {
        let busy = self.session.is_some();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                ui.label("장치");
                let selected = self
                    .devices
                    .get(self.dev_sel)
                    .map(|d| format!("{}  {}", d.location, d.name()))
                    .unwrap_or_else(|| "장치 없음".into());
                egui::ComboBox::from_id_salt("device")
                    .selected_text(selected)
                    .width(250.0)
                    .show_ui(ui, |ui| {
                        for (i, d) in self.devices.iter().enumerate() {
                            let fw = if d.has_firmware {
                                ""
                            } else {
                                "  (펌웨어 올림)"
                            };
                            ui.selectable_value(
                                &mut self.dev_sel,
                                i,
                                format!("{}  {}{fw}", d.location, d.name()),
                            );
                        }
                    });
                if ui.button("다시 찾기").clicked() {
                    self.scan_devices(ui.ctx());
                }
                ui.separator();
                ui.label("샘플레이트");
                egui::ComboBox::from_id_salt("rate")
                    .selected_text(fmt_rate(self.rate))
                    .width(80.0)
                    .show_ui(ui, |ui| {
                        for &r in SAMPLERATES.iter().rev() {
                            ui.selectable_value(&mut self.rate, r, fmt_rate(r));
                        }
                    });
                ui.label("시간");
                egui::ComboBox::from_id_salt("duration")
                    .selected_text(fmt_duration(self.duration))
                    .width(80.0)
                    .show_ui(ui, |ui| {
                        for &d in &DURATION_CHOICES {
                            ui.selectable_value(&mut self.duration, d, fmt_duration(d));
                        }
                    })
                    .response
                    .on_hover_text(match self.duration {
                        Some(s) => format!(
                            "{} 샘플 ({} × {}초)",
                            fmt_count(samples_for(self.rate, s)),
                            fmt_rate(self.rate),
                            fmt_secs(s)
                        ),
                        None => "중지를 누를 때까지 수집".into(),
                    });
                ui.separator();
                ui.checkbox(&mut self.trigger_on, "트리거");
                ui.add_enabled(
                    self.trigger_on,
                    egui::TextEdit::singleline(&mut self.trigger_text)
                        .desired_width(80.0)
                        .hint_text("0=r,1=1"),
                )
                .on_hover_text("채널=조건, 쉼표로 AND. r 상승, f 하강, e 에지, 1 High, 0 Low");
                ui.add_enabled(
                    self.trigger_on && self.duration.is_some(),
                    egui::DragValue::new(&mut self.pre_percent)
                        .range(0..=100)
                        .suffix("% 앞"),
                )
                .on_hover_text("트리거 앞 구간 비율 (시간을 정했을 때)");
            });
            ui.separator();
            if busy {
                if ui.button("중지").clicked()
                    && let Some(s) = &self.session
                {
                    s.stop();
                }
            } else if ui
                .add_enabled(!self.devices.is_empty(), egui::Button::new("시작"))
                .clicked()
            {
                self.start_capture(ui.ctx());
            }
            if let Some(t) = self.session_text() {
                ui.label(t);
            } else if let Some(m) = &self.message {
                ui.label(m);
            }
        });
    }

    fn file_bar(&mut self, ui: &mut egui::Ui) {
        let busy = self.session.is_some();
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("열기…")).clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("캡처 파일", &["lvcap", "sr"])
                    .add_filter("logicview 캡처", &["lvcap"])
                    .add_filter("sigrok 세션", &["sr"])
                    .pick_file()
            {
                self.open(&p);
            }
            if ui
                .add_enabled(!busy && self.capture.is_some(), egui::Button::new("저장…"))
                .clicked()
            {
                self.save_dialog();
            }
            ui.separator();
            if ui.button("전체 보기").clicked() {
                self.fit();
                self.follow = busy;
            }
            if ui.button("확대").clicked() {
                self.zoom(2.0, self.plot_width / 2.0);
            }
            if ui.button("축소").clicked() {
                self.zoom(0.5, self.plot_width / 2.0);
            }
            ui.separator();
            ui.toggle_value(&mut self.show_channels, "채널…");
            ui.toggle_value(&mut self.decode.open, "디코더…");
            ui.add_enabled_ui(self.capture.is_some(), |ui| {
                ui.toggle_value(&mut self.table.show, "표");
            });
            self.mcp_toggle(ui);
            ui.separator();
            if let Some(cv) = &self.capture {
                let i = cv.info();
                ui.label(format!(
                    "{}  ·  {}  ·  {} 샘플  ·  {:.6} s  ·  {}",
                    cv.path()
                        .file_name()
                        .map(|f| f.to_string_lossy())
                        .unwrap_or_default(),
                    fmt_rate(i.samplerate_hz),
                    fmt_count(i.num_samples),
                    i.duration_secs(),
                    i.device
                ));
            } else {
                ui.label(
                    "시작을 눌러 수집하거나, 캡처 파일(.lvcap, .sr)을 열거나 창에 끌어다 놓으세요",
                );
            }
        });
    }

    fn status(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (n, sum_b, max_b, sum_c, max_c) = self.timings.iter().fold(
                (0usize, 0.0, 0.0f64, 0.0, 0.0f64),
                |(n, sb, mb, sc, mc), &(b, c)| (n + 1, sb + b, mb.max(b), sc + c, mc.max(c)),
            );
            if n > 0 {
                ui.label(format!(
                    "프레임 CPU {:.2} ms (최대 {:.2})  ·  파형 {:.2} ms (최대 {:.2})",
                    sum_c / n as f64,
                    max_c,
                    sum_b / n as f64,
                    max_b,
                ));
            }
            ui.separator();
            let s = self.last_stats;
            ui.label(format!(
                "{} / 점  ·  {}",
                if self.view.spp >= 1.0 {
                    format!("{:.1} 샘플", self.view.spp)
                } else {
                    format!("1/{:.0} 샘플", 1.0 / self.view.spp)
                },
                if s.sample_mode {
                    "원시 샘플".to_string()
                } else if s.level == 0 {
                    "원시 데이터 요약".to_string()
                } else {
                    format!("요약 {}단계", s.level)
                },
            ));
            if let Some(t) = self.decode.progress_text() {
                ui.separator();
                ui.label(t);
            }
            if let Some(c) = &self.cursor_text {
                ui.separator();
                ui.label(c);
            }
        });
    }

    fn channels_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_channels;
        let mut changed = false;
        egui::Window::new("채널")
            .open(&mut open)
            .resizable(false)
            .default_width(260.0)
            .show(ctx, |ui| {
                egui::Grid::new("channels").num_columns(3).show(ui, |ui| {
                    for (c, &color) in COLORS.iter().enumerate() {
                        let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                        ui.painter().rect_filled(r, CornerRadius::same(2), color);
                        changed |= ui.checkbox(&mut self.shown[c], format!("D{c}")).changed();
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.names[c]).desired_width(170.0),
                        );
                        changed |= resp.lost_focus();
                        ui.end_row();
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("모두 보기").clicked() {
                        self.shown = vec![true; 8];
                        changed = true;
                    }
                    if ui.button("이름 초기화").clicked() {
                        self.names = default_names();
                        changed = true;
                    }
                });
                ui.label("이름은 열린 파일에 저장되고 다음 수집에도 쓰입니다.");
            });
        self.show_channels = open;
        if changed {
            self.persist_channels();
        }
    }

    // ---- waveform ----------------------------------------------------------

    fn waveform(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let full = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(full, Sense::click_and_drag());
        let painter = ui.painter_at(full);
        let bg = ui.visuals().extreme_bg_color;
        painter.rect_filled(full, CornerRadius::ZERO, bg);

        let plot = Rect::from_min_max(pos2(full.left() + LABEL_W, full.top() + RULER_H), full.max);
        self.plot_width = plot.width().max(1.0);
        if self.fit_pending && self.capture.is_some() {
            self.fit();
            self.fit_pending = false;
        }

        // Input: wheel / pinch zoom around the cursor, drag to pan.
        let mut cursor = None;
        if let Some(pos) = resp.hover_pos() {
            let x = (pos.x - plot.left()).clamp(0.0, plot.width());
            let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = zoom as f64 * (scroll as f64 * 0.003).exp();
            if (factor - 1.0).abs() > 1e-6 {
                self.zoom(factor, x);
            }
            if plot.contains(pos) {
                cursor = Some(pos);
            }
        }
        if resp.dragged() && resp.drag_delta().x != 0.0 {
            self.follow = false;
            self.view.start -= resp.drag_delta().x as f64 * self.view.spp;
            self.clamp_view();
        }
        ui.input(|i| {
            if i.key_pressed(Key::Home) || i.key_pressed(Key::F) {
                self.fit_pending = true;
            }
            if i.key_pressed(Key::Escape) {
                self.markers = [None; 2];
            }
        });
        if let (Some(b), Some(cv)) = (&mut self.bench, &self.capture) {
            b.step(&mut self.view, self.plot_width, cv.num_samples());
            self.clamp_view();
        }

        let Some(cv) = &mut self.capture else {
            if let Some(e) = &self.error {
                painter.text(
                    full.center(),
                    Align2::CENTER_CENTER,
                    e,
                    FontId::proportional(15.0),
                    Color32::LIGHT_RED,
                );
            }
            return None;
        };
        let info = cv.info().clone();
        let rate = info.samplerate_hz.max(1) as f64;
        let nch = info.num_channels.clamp(1, 8);
        let chans: Vec<usize> = (0..nch).filter(|&c| self.shown[c]).collect();
        let ann_h: f32 = self.analyzers.iter().map(Analyzer::height).sum();
        let row_h = ((plot.height() - ann_h) / chans.len().max(1) as f32).clamp(24.0, 96.0);
        let view = self.view;
        let x_of = |s: f64| plot.left() + ((s - view.start) / view.spp) as f32;

        // Time ruler and grid.
        let step = wave::nice_step(120.0 * view.spp / rate);
        let t0 = view.start / rate;
        let t1 = (view.start + plot.width() as f64 * view.spp) / rate;
        let grid = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let text = ui.visuals().text_color();
        let mut t = (t0 / step).ceil() * step;
        while t <= t1 {
            let x = x_of(t * rate);
            painter.line_segment(
                [pos2(x, full.top() + RULER_H - 6.0), pos2(x, plot.bottom())],
                Stroke::new(1.0, grid),
            );
            painter.text(
                pos2(x + 3.0, full.top() + 4.0),
                Align2::LEFT_TOP,
                wave::fmt_time(t, step),
                FontId::proportional(12.0),
                text,
            );
            t += step;
        }

        // Channel labels and rows.
        let mut rows = Vec::with_capacity(chans.len());
        for (i, &ch) in chans.iter().enumerate() {
            let top = plot.top() + i as f32 * row_h;
            let color = COLORS[ch % COLORS.len()];
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(full.left() + 6.0, top + row_h / 2.0 - 5.0),
                    vec2(10.0, 10.0),
                ),
                CornerRadius::same(2),
                color,
            );
            painter.text(
                pos2(full.left() + 22.0, top + row_h / 2.0),
                Align2::LEFT_CENTER,
                if self.names[ch] == format!("D{ch}") {
                    format!("D{ch}")
                } else {
                    format!("D{ch}  {}", self.names[ch])
                },
                FontId::proportional(13.0),
                text,
            );
            painter.line_segment(
                [
                    pos2(full.left(), top + row_h),
                    pos2(full.right(), top + row_h),
                ],
                Stroke::new(1.0, grid.gamma_multiply(0.5)),
            );
            rows.push(Row {
                channel: ch as u8,
                hi: top + row_h * 0.22,
                lo: top + row_h * 0.78,
                color,
            });
        }

        let t_build = Instant::now();
        match wave::build(cv, plot, &rows, view) {
            Ok((mesh, stats)) => {
                self.last_stats = stats;
                painter.with_clip_rect(plot).add(Shape::mesh(mesh));
            }
            Err(e) => self.error = Some(format!("데이터를 읽지 못했습니다: {e}")),
        }
        let build_ms = t_build.elapsed().as_secs_f64() * 1e3;

        // Annotation rows (UI-9) under the channels.
        let mut y = plot.top() + chans.len() as f32 * row_h;
        for an in self.analyzers.iter_mut() {
            match an.paint(
                &painter,
                cv.file(),
                plot,
                full.left() + 22.0,
                y,
                view,
                text,
                grid,
            ) {
                Ok(h) => y += h,
                Err(e) => log::warn!("drawing {}: {e}", an.info.label),
            }
        }
        if let Some((s, e)) = self.highlight {
            let (x0, x1) = (
                x_of(s as f64).max(plot.left()),
                x_of(e as f64).min(plot.right()),
            );
            if x1 >= x0 {
                painter.rect_filled(
                    Rect::from_min_max(pos2(x0 - 1.0, plot.top()), pos2(x1 + 1.0, plot.bottom())),
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0x4c, 0x9f, 0xd8, 28),
                );
            }
        }

        // Markers (UI-5): click sets A, then B, then moves the nearer one;
        // right click clears. A click near an edge snaps to it.
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && plot.contains(pos)
        {
            let s = view.sample_at(pos.x - plot.left());
            let row = ((pos.y - plot.top()) / row_h) as usize;
            let s = chans
                .get(row)
                .and_then(|&ch| snap_to_edge(cv, ch, s, SNAP_PX * view.spp))
                .unwrap_or_else(|| s.round().max(0.0));
            let slot = match self.markers {
                [None, _] => 0,
                [Some(_), None] => 1,
                [Some(a), Some(b)] => usize::from((s - b).abs() < (s - a).abs()),
            };
            self.markers[slot] = Some(s);
        }
        if resp.secondary_clicked() {
            self.markers = [None; 2];
        }
        let trigger_at = info.trigger.as_deref().and_then(trigger_sample);
        if let Some(ts) = trigger_at {
            let x = x_of(ts as f64);
            if (plot.left()..=plot.right()).contains(&x) {
                painter.line_segment(
                    [pos2(x, full.top() + RULER_H - 8.0), pos2(x, plot.bottom())],
                    Stroke::new(1.0, TRIGGER_COLOR),
                );
                painter.text(
                    pos2(x + 3.0, full.top() + RULER_H - 10.0),
                    Align2::LEFT_BOTTOM,
                    "T",
                    FontId::proportional(11.0),
                    TRIGGER_COLOR,
                );
            }
        }
        for (i, m) in self.markers.iter().enumerate() {
            let Some(m) = *m else { continue };
            let x = x_of(m);
            if !(plot.left()..=plot.right()).contains(&x) {
                continue;
            }
            let c = MARKER_COLORS[i];
            painter.line_segment(
                [pos2(x, full.top()), pos2(x, plot.bottom())],
                Stroke::new(1.5, c),
            );
            painter.text(
                pos2(x + 3.0, full.top() + RULER_H - 2.0),
                Align2::LEFT_BOTTOM,
                ["A", "B"][i],
                FontId::proportional(12.0),
                c,
            );
        }
        if let [Some(a), Some(b)] = self.markers {
            let (xa, xb) = (x_of(a), x_of(b));
            let (l, r) = (xa.min(xb).max(plot.left()), xa.max(xb).min(plot.right()));
            if r > l {
                painter.rect_filled(
                    Rect::from_min_max(pos2(l, plot.top()), pos2(r, plot.bottom())),
                    CornerRadius::ZERO,
                    Color32::from_white_alpha(10),
                );
            }
        }

        // Readout: cursor, then markers.
        let mut parts = Vec::new();
        if let Some(pos) = cursor {
            painter.line_segment(
                [pos2(pos.x, plot.top()), pos2(pos.x, plot.bottom())],
                Stroke::new(1.0, Color32::from_white_alpha(90)),
            );
            let s = view.sample_at(pos.x - plot.left());
            let k = s.floor().max(0.0) as u64;
            let vals = cv
                .raw(k, k + 1)
                .ok()
                .and_then(|v| v.first().copied())
                .map(|b| {
                    (0..nch)
                        .map(|c| ((b >> c) & 1).to_string())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            parts.push(format!(
                "커서 {}  ·  샘플 {}  ·  D0..D{} = {}",
                wave::fmt_time(s / rate, step / 100.0),
                fmt_count(k),
                nch - 1,
                vals
            ));
        }
        let tick = (1.0 / rate).max(step / 1000.0);
        for (i, m) in self.markers.iter().enumerate() {
            if let Some(m) = m {
                parts.push(format!(
                    "{} {}",
                    ["A", "B"][i],
                    trim_time(&wave::fmt_time(m / rate, tick))
                ));
            }
        }
        if let [Some(a), Some(b)] = self.markers {
            let dt = (b - a).abs() / rate;
            if dt > 0.0 {
                parts.push(format!(
                    "Δ {}  ({})",
                    trim_time(&wave::fmt_time(dt, 1.0 / rate)),
                    fmt_freq(1.0 / dt)
                ));
            }
        }
        if parts.is_empty() {
            parts
                .push("클릭: 마커 A·B  ·  오른쪽 클릭/Esc: 마커 지우기  ·  Home: 전체 보기".into());
        }
        // The frame CPU time is filled in by `ui()` from eframe's frame info.
        self.push_timing(build_ms, 0.0);
        Some(parts.join("  ·  "))
    }

    fn push_timing(&mut self, build_ms: f64, cpu_ms: f64) {
        self.timings.push_back((build_ms, cpu_ms));
        while self.timings.len() > 120 {
            self.timings.pop_front();
        }
    }
}

/// The nearest edge of `ch` within `radius` samples of `s`.
fn snap_to_edge(cv: &mut CaptureView, ch: usize, s: f64, radius: f64) -> Option<f64> {
    if radius > 65_536.0 {
        return None; // too zoomed out to read that much
    }
    let a = (s - radius).floor().max(1.0) as u64 - 1;
    let b = (s + radius).ceil().max(0.0) as u64 + 1;
    let raw = cv.raw(a, b).ok()?;
    let bit = 1u8 << ch;
    raw.windows(2)
        .enumerate()
        .filter(|(_, w)| (w[0] ^ w[1]) & bit != 0)
        .map(|(i, _)| (a + i as u64 + 1) as f64)
        .filter(|&e| (e - s).abs() <= radius)
        .min_by(|x, y| (x - s).abs().total_cmp(&(y - s).abs()))
}

/// `1.50000 ms` -> `1.5 ms`.
fn trim_time(s: &str) -> String {
    match s.split_once(' ') {
        Some((num, unit)) if num.contains('.') => {
            format!("{} {unit}", num.trim_end_matches('0').trim_end_matches('.'))
        }
        _ => s.to_string(),
    }
}

/// `"sample":N` from `capture.trigger_json`.
fn trigger_sample(json: &str) -> Option<u64> {
    let rest = &json[json.find("\"sample\":")? + 9..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

impl Drop for App {
    fn drop(&mut self) {
        self.save_settings();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Files dropped on the window.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(p) = dropped.first() {
            self.open(p);
        }
        if self.bench.is_none()
            && self.session.is_none()
            && self.last_scan.is_none_or(|t| t.elapsed() >= DEVICE_SCAN)
        {
            self.scan_devices(&ctx);
            ctx.request_repaint_after(DEVICE_SCAN);
        }
        self.poll_scan();
        self.poll_session(&ctx);
        let cpu_prev_ms = frame
            .info()
            .cpu_usage
            .map(|s| s as f64 * 1e3)
            .unwrap_or(0.0);

        egui::Panel::top("toolbar").show(ui, |ui| {
            self.capture_bar(ui);
            self.file_bar(ui);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status(ui));
        if let Some(ev) = self.decode.poll(&ctx) {
            match ev {
                decode_ui::Event::Added { path, .. } => {
                    if self.capture.as_ref().is_some_and(|c| c.path() == path) {
                        self.reload_analyzers();
                        self.table.show = true;
                    }
                }
                decode_ui::Event::Failed(e) => log::warn!("decode: {e}"),
            }
        }
        if self.table.show
            && let Some(cv) = &self.capture
        {
            let rate = cv.info().samplerate_hz as f64;
            let mut act = None;
            egui::Panel::bottom("table")
                .resizable(true)
                .default_size(220.0)
                .show(ui, |ui| {
                    act = self.table.panel(ui, cv.file(), &mut self.analyzers, rate);
                });
            match act {
                Some(TableAction::Jump(s, e)) => self.jump_to(s, e),
                Some(TableAction::Delete(id)) => {
                    let path = self.capture.as_ref().map(|c| c.path().to_path_buf());
                    if let Some(path) = path {
                        match lv_store::analysis::delete_analyzer(&path, id) {
                            Ok(()) => self.reload_analyzers(),
                            Err(e) => {
                                self.message = Some(format!("분석기를 지우지 못했습니다: {e}"))
                            }
                        }
                    }
                }
                None => {}
            }
        }
        egui::CentralPanel::default().show(ui, |ui| {
            self.cursor_text = self.waveform(ui);
        });
        self.channels_window(&ctx);
        let recording = self.session.is_some();
        let file = self
            .capture
            .as_ref()
            .map(|c| (c.path().to_path_buf(), self.names.clone()));
        self.decode.window(
            &ctx,
            file.as_ref().map(|(p, n)| (p.as_path(), n.as_slice())),
            recording,
            &self.db,
        );
        self.remember(&ctx);
        // eframe reports the previous frame's total CPU time.
        if let Some(last) = self.timings.back_mut() {
            last.1 = cpu_prev_ms;
        }
        if let Some(b) = &mut self.bench {
            b.record(
                self.timings.back().copied().unwrap_or_default(),
                self.last_stats,
            );
            if b.done() {
                b.report();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint();
        }
    }
}

pub fn fmt_rate(hz: u64) -> String {
    let hz = hz as f64;
    if hz >= 1e6 {
        format!("{} MHz", hz / 1e6)
    } else if hz >= 1e3 {
        format!("{} kHz", hz / 1e3)
    } else {
        format!("{hz} Hz")
    }
}

/// Samples in `secs` seconds at `rate` (at least one).
fn samples_for(rate: u64, secs: f64) -> u64 {
    ((rate as f64 * secs).round() as u64).max(1)
}

/// Seconds without trailing zeros: 0.1, 2, 0.000625.
fn fmt_secs(secs: f64) -> String {
    let s = format!("{secs:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn fmt_duration(d: Option<f64>) -> String {
    match d {
        None => "수동 중지".into(),
        Some(s) => format!("{} 초", fmt_secs(s)),
    }
}

fn fmt_freq(hz: f64) -> String {
    let (v, unit) = if hz >= 1e6 {
        (hz / 1e6, "MHz")
    } else if hz >= 1e3 {
        (hz / 1e3, "kHz")
    } else {
        (hz, "Hz")
    };
    format!("{} {unit}", trim_float(v, 4))
}

/// `v` with `sig` significant digits, without trailing zeros.
fn trim_float(v: f64, sig: usize) -> String {
    let int_digits = if v.abs() >= 1.0 {
        v.abs().log10().floor() as usize + 1
    } else {
        1
    };
    let dec = sig.saturating_sub(int_digits);
    let s = format!("{v:.dec$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

pub fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui5_frequency_text() {
        assert_eq!(fmt_freq(1000.0), "1 kHz");
        assert_eq!(fmt_freq(3921.5686), "3.922 kHz");
        assert_eq!(fmt_freq(115_200.0), "115.2 kHz");
        assert_eq!(fmt_freq(12.5), "12.5 Hz");
        assert_eq!(fmt_freq(24e6), "24 MHz");
    }

    #[test]
    fn ui5_marker_time_text() {
        assert_eq!(trim_time("1.00000 ms"), "1 ms");
        assert_eq!(trim_time("1.50000 ms"), "1.5 ms");
        assert_eq!(trim_time("500 µs"), "500 µs");
        assert_eq!(trim_time("10.0 s"), "10 s");
    }

    #[test]
    fn cap8_trigger_position_from_json() {
        assert_eq!(
            trigger_sample(r#"{"conditions":"0=r","pre_percent":10,"sample":1234}"#),
            Some(1234)
        );
        assert_eq!(trigger_sample("{}"), None);
    }

    #[test]
    fn ui1_duration_text() {
        assert_eq!(fmt_duration(Some(0.1)), "0.1 초");
        assert_eq!(fmt_duration(Some(60.0)), "60 초");
        assert_eq!(fmt_duration(Some(10_000.0 / 16e6)), "0.000625 초");
        assert_eq!(fmt_duration(None), "수동 중지");
        assert_eq!(samples_for(16_000_000, 0.1), 1_600_000);
        assert_eq!(samples_for(24_000_000, 60.0), 1_440_000_000);
        assert_eq!(samples_for(20_000, 1e-9), 1);
        // --samples n gives back n samples at the same rate.
        assert_eq!(samples_for(16_000_000, 10_000_000.0 / 16e6), 10_000_000);
    }
}
