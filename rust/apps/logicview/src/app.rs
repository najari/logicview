//! The viewer window (M0: open a capture, zoom and pan the waveform).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Key, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2,
};

use crate::bench::Bench;
use crate::capture_view::CaptureView;
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

const LABEL_W: f32 = 130.0;
const RULER_H: f32 = 26.0;
/// Most zoomed in: 64 points per sample.
const MIN_SPP: f64 = 1.0 / 64.0;

pub struct App {
    capture: Option<CaptureView>,
    view: View,
    error: Option<String>,
    /// (wave build ms, egui cpu ms) of recent frames.
    timings: VecDeque<(f64, f64)>,
    last_stats: WaveStats,
    plot_width: f32,
    fit_pending: bool,
    /// Cursor readout from the previous frame (the status bar is laid out
    /// before the waveform).
    cursor_text: Option<String>,
    bench: Option<Bench>,
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

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        path: Option<PathBuf>,
        bench: bool,
        initial_view: Option<View>,
    ) -> Self {
        setup_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut app = Self {
            capture: None,
            view: View {
                start: 0.0,
                spp: 1.0,
            },
            error: None,
            timings: VecDeque::new(),
            last_stats: WaveStats::default(),
            plot_width: 1000.0,
            fit_pending: true,
            cursor_text: None,
            bench: bench.then(Bench::default),
        };
        if let Some(p) = path {
            app.open(&p);
        }
        if let Some(v) = initial_view {
            app.view = v;
            app.fit_pending = false;
        }
        app
    }

    fn open(&mut self, path: &Path) {
        match CaptureView::open(path) {
            Ok(cv) => {
                self.capture = Some(cv);
                self.error = None;
                self.fit_pending = true;
            }
            Err(e) => self.error = Some(format!("{}을(를) 열 수 없습니다: {e}", path.display())),
        }
    }

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
        let anchor = self.view.sample_at(x);
        self.view.spp /= factor;
        self.view.spp = self.view.spp.clamp(MIN_SPP, self.max_spp());
        self.view.start = anchor - x as f64 * self.view.spp;
        self.clamp_view();
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("열기…").clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("logicview 캡처", &["lvcap"])
                    .pick_file()
            {
                self.open(&p);
            }
            ui.separator();
            if ui.button("전체 보기").clicked() {
                self.fit();
            }
            if ui.button("확대").clicked() {
                self.zoom(2.0, self.plot_width / 2.0);
            }
            if ui.button("축소").clicked() {
                self.zoom(0.5, self.plot_width / 2.0);
            }
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
                ui.label("캡처 파일(.lvcap)을 열거나 창에 끌어다 놓으세요");
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
                    "파형 {:.2} ms (최대 {:.2})  ·  프레임 CPU {:.2} ms (최대 {:.2})",
                    sum_b / n as f64,
                    max_b,
                    sum_c / n as f64,
                    max_c
                ));
            }
            ui.separator();
            let s = self.last_stats;
            ui.label(format!(
                "{} / 점  ·  {}  ·  사각형 {}",
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
                s.quads
            ));
            if let Some(c) = &self.cursor_text {
                ui.separator();
                ui.label(c);
            }
        });
    }

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
        if resp.dragged() {
            self.view.start -= resp.drag_delta().x as f64 * self.view.spp;
            self.clamp_view();
        }
        ui.input(|i| {
            if i.key_pressed(Key::Home) || i.key_pressed(Key::F) {
                self.fit_pending = true;
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
        let row_h = (plot.height() / nch as f32).clamp(24.0, 96.0);

        // Time ruler and grid.
        let step = wave::nice_step(120.0 * self.view.spp / rate);
        let t0 = self.view.start / rate;
        let t1 = (self.view.start + plot.width() as f64 * self.view.spp) / rate;
        let grid = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let text = ui.visuals().text_color();
        let mut t = (t0 / step).ceil() * step;
        while t <= t1 {
            let x = plot.left() + ((t * rate - self.view.start) / self.view.spp) as f32;
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
        let mut rows = Vec::with_capacity(nch);
        for ch in 0..nch {
            let top = plot.top() + ch as f32 * row_h;
            let color = COLORS[ch % COLORS.len()];
            let name = info
                .channel_names
                .get(ch)
                .cloned()
                .unwrap_or_else(|| format!("D{ch}"));
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
                format!("D{ch}  {name}"),
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
        match wave::build(cv, plot, &rows, self.view) {
            Ok((mesh, stats)) => {
                self.last_stats = stats;
                painter.with_clip_rect(plot).add(Shape::mesh(mesh));
            }
            Err(e) => self.error = Some(format!("데이터를 읽지 못했습니다: {e}")),
        }
        let build_ms = t_build.elapsed().as_secs_f64() * 1e3;

        // Cursor readout.
        let readout = cursor.map(|pos: Pos2| {
            painter.line_segment(
                [pos2(pos.x, plot.top()), pos2(pos.x, plot.bottom())],
                Stroke::new(1.0, Color32::from_white_alpha(90)),
            );
            let s = self.view.sample_at(pos.x - plot.left());
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
            format!(
                "커서 {}  ·  샘플 {}  ·  D0..D{} = {}",
                wave::fmt_time(s / rate, step / 100.0),
                fmt_count(k),
                nch - 1,
                vals
            )
        });
        // The frame CPU time is filled in by `ui()` from eframe's frame info.
        self.push_timing(build_ms, 0.0);
        readout
    }

    fn push_timing(&mut self, build_ms: f64, cpu_ms: f64) {
        self.timings.push_back((build_ms, cpu_ms));
        while self.timings.len() > 120 {
            self.timings.pop_front();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Files dropped on the window.
        let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(p) = dropped.first() {
            self.open(p);
        }
        let cpu_prev_ms = frame
            .info()
            .cpu_usage
            .map(|s| s as f64 * 1e3)
            .unwrap_or(0.0);

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            self.cursor_text = self.waveform(ui);
        });
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
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ui.ctx().request_repaint();
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
