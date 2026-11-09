//! Decoders in the app (M3): the "add decoder" window, background decoding
//! with progress (DEC-4), annotation rows under the waveform and the data
//! table (UI-9).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::Instant;

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Painter, Rect, Stroke, pos2};
use lv_decode::{Control, DecoderInfo, RowDesc, Settings, StackEntry};
use lv_store::CaptureFile;
use lv_store::analysis::{AnalyzerInfo, Frame};

use crate::wave::{self, View};

pub const ANN_ROW_H: f32 = 22.0;
/// Frames read for one view; more than this and the row is drawn as dense.
const VIEW_FRAMES: usize = 5000;
const ROW_COLORS: [Color32; 6] = [
    Color32::from_rgb(0x4c, 0x9f, 0xd8),
    Color32::from_rgb(0x8b, 0xc3, 0x4a),
    Color32::from_rgb(0xff, 0xa7, 0x26),
    Color32::from_rgb(0xba, 0x68, 0xc8),
    Color32::from_rgb(0x26, 0xc6, 0xda),
    Color32::from_rgb(0xef, 0x53, 0x50),
];

// ---- stored analyzers ---------------------------------------------------------

struct FrameCache {
    a: u64,
    b: u64,
    /// Frame and its texts, longest first.
    frames: Vec<(Frame, Vec<String>)>,
    full: bool,
}

/// A stored analyzer and how to draw it.
pub struct Analyzer {
    pub info: AnalyzerInfo,
    pub rows: Vec<RowDesc>,
    /// Rows that have frames, in order.
    pub used: Vec<usize>,
    pub shown: bool,
    cache: Option<FrameCache>,
}

pub fn load_analyzers(file: &CaptureFile) -> Vec<Analyzer> {
    let list = match file.analyzers() {
        Ok(l) => l,
        Err(e) => {
            log::warn!("reading analyzers: {e}");
            return Vec::new();
        }
    };
    list.into_iter()
        .map(|info| {
            let rows = Settings::parse(&info.settings_json)
                .map(|s| s.rows)
                .unwrap_or_default();
            let used = file
                .frame_rows(info.id)
                .unwrap_or_default()
                .into_iter()
                .map(|r| r.max(0) as usize)
                .collect();
            Analyzer {
                info,
                rows,
                used,
                shown: true,
                cache: None,
            }
        })
        .collect()
}

impl Analyzer {
    pub fn row_name(&self, row: usize) -> String {
        self.rows
            .get(row)
            .map(|r| r.desc.clone())
            .unwrap_or_else(|| format!("행 {row}"))
    }

    /// Frames overlapping `[a, b)`, from a cache that also covers some of
    /// the neighbourhood so panning does not query every frame.
    fn frames(&mut self, file: &CaptureFile, a: u64, b: u64) -> lv_store::Result<&FrameCache> {
        let hit = self
            .cache
            .as_ref()
            .is_some_and(|c| c.a <= a && b <= c.b && (!c.full || (c.a == a && c.b == b)));
        if !hit {
            let w = b.saturating_sub(a).max(1);
            let (qa, qb) = (a.saturating_sub(w / 2), b + w / 2);
            let mut frames =
                file.frames_in(self.info.id, qa, qb, self.info.max_span, VIEW_FRAMES)?;
            let full = frames.len() == VIEW_FRAMES;
            let (qa, qb) = if full {
                // Too many: keep exactly the view so the next look re-reads.
                frames.retain(|f| f.end >= a && f.start < b);
                (a, b)
            } else {
                (qa, qb)
            };
            let frames = frames
                .into_iter()
                .map(|f| {
                    let mut texts = vec![f.text.clone()];
                    if let Some(d) = &f.data
                        && let Ok(more) = serde_json::from_str::<Vec<String>>(d)
                    {
                        texts.extend(more);
                    }
                    (f, texts)
                })
                .collect();
            self.cache = Some(FrameCache {
                a: qa,
                b: qb,
                frames,
                full,
            });
        }
        Ok(self.cache.as_ref().expect("cache filled"))
    }

    /// Draw this analyzer's rows from `y`; returns the height used.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        painter: &Painter,
        file: &CaptureFile,
        plot: Rect,
        label_x: f32,
        y: f32,
        view: View,
        text: Color32,
        grid: Color32,
    ) -> lv_store::Result<f32> {
        if !self.shown || self.used.is_empty() {
            return Ok(0.0);
        }
        let a = view.start.max(0.0) as u64;
        let b = (view.start + plot.width() as f64 * view.spp)
            .max(0.0)
            .ceil() as u64
            + 1;
        let used = self.used.clone();
        let label = self.info.label.clone();
        let names: Vec<String> = used.iter().map(|&r| self.row_name(r)).collect();
        let cache = self.frames(file, a, b)?;
        let x_of = |s: u64| plot.left() + ((s as f64 - view.start) / view.spp) as f32;
        for (i, &row) in used.iter().enumerate() {
            let top = y + i as f32 * ANN_ROW_H;
            let (y0, y1) = (top + 3.0, top + ANN_ROW_H - 3.0);
            let color = ROW_COLORS[row % ROW_COLORS.len()];
            painter.text(
                pos2(label_x, top + ANN_ROW_H / 2.0),
                Align2::LEFT_CENTER,
                format!("{label}: {}", names[i]),
                FontId::proportional(11.0),
                text,
            );
            painter.line_segment(
                [
                    pos2(label_x - 16.0, top + ANN_ROW_H),
                    pos2(plot.right(), top + ANN_ROW_H),
                ],
                Stroke::new(1.0, grid.gamma_multiply(0.4)),
            );
            let clip = painter.with_clip_rect(Rect::from_min_max(
                pos2(plot.left(), top),
                pos2(plot.right(), top + ANN_ROW_H),
            ));
            let mut last_x = f32::NEG_INFINITY;
            for (f, texts) in cache.frames.iter().filter(|(f, _)| f.row as usize == row) {
                let x0 = x_of(f.start).max(plot.left() - 2.0);
                let x1 = x_of(f.end).min(plot.right() + 2.0);
                if x1 < plot.left() || x0 > plot.right() {
                    continue;
                }
                if x1 - x0 < 2.0 {
                    // Too narrow for a box: a tick, at most one per pixel.
                    if x0 - last_x >= 1.0 {
                        clip.line_segment([pos2(x0, y0), pos2(x0, y1)], Stroke::new(1.0, color));
                        last_x = x0;
                    }
                    continue;
                }
                let r = Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
                clip.rect_filled(r, CornerRadius::same(3), color.gamma_multiply(0.35));
                clip.rect_stroke(
                    r,
                    CornerRadius::same(3),
                    Stroke::new(1.0, color),
                    egui::StrokeKind::Inside,
                );
                let room = x1 - x0 - 6.0;
                if let Some(t) = texts
                    .iter()
                    .find(|t| t.chars().count() as f32 * 6.5 <= room)
                {
                    clip.text(
                        pos2((x0 + x1) / 2.0, (y0 + y1) / 2.0),
                        Align2::CENTER_CENTER,
                        t,
                        FontId::proportional(11.0),
                        Color32::WHITE,
                    );
                }
            }
            if cache.full {
                clip.text(
                    pos2(plot.right() - 4.0, top + ANN_ROW_H / 2.0),
                    Align2::RIGHT_CENTER,
                    "프레임이 많아 일부만 표시 (확대하세요)",
                    FontId::proportional(10.0),
                    text.gamma_multiply(0.7),
                );
            }
        }
        Ok(used.len() as f32 * ANN_ROW_H)
    }

    pub fn height(&self) -> f32 {
        if self.shown {
            self.used.len() as f32 * ANN_ROW_H
        } else {
            0.0
        }
    }
}

// ---- add-decoder window ---------------------------------------------------------

enum Catalog {
    NotLoaded,
    Loading(mpsc::Receiver<Result<Vec<DecoderInfo>, String>>),
    Ready(Vec<DecoderInfo>),
    Failed(String),
}

struct OptForm {
    id: String,
    desc: String,
    default: String,
    value: String,
    choices: Vec<String>,
}

struct EntryForm {
    id: String,
    /// (decoder channel id, label, required, chosen capture channel)
    channels: Vec<(String, String, bool, Option<usize>)>,
    options: Vec<OptForm>,
}

impl EntryForm {
    fn new(d: &DecoderInfo, names: &[String]) -> EntryForm {
        let guess = |id: &str, name: &str| {
            names
                .iter()
                .position(|n| n.eq_ignore_ascii_case(id) || n.eq_ignore_ascii_case(name))
        };
        let mut channels = Vec::new();
        for (list, required) in [(&d.channels, true), (&d.opt_channels, false)] {
            for c in list {
                channels.push((
                    c.id.clone(),
                    c.name.clone(),
                    required,
                    guess(&c.id, &c.name),
                ));
            }
        }
        EntryForm {
            id: d.id.clone(),
            channels,
            options: d
                .options
                .iter()
                .map(|o| {
                    let def = o
                        .default
                        .as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_default();
                    OptForm {
                        id: o.id.clone(),
                        desc: o.desc.clone(),
                        value: def.clone(),
                        default: def,
                        choices: o.values.iter().map(|v| v.to_string()).collect(),
                    }
                })
                .collect(),
        }
    }

    fn stack_entry(&self, first: bool) -> Result<StackEntry, String> {
        let mut args = Vec::new();
        if first {
            for (id, name, required, ch) in &self.channels {
                match ch {
                    Some(i) => args.push((id.clone(), i.to_string())),
                    None if *required => {
                        return Err(format!("{}: 필수 채널 '{name}'을(를) 고르세요", self.id));
                    }
                    None => {}
                }
            }
        }
        for o in &self.options {
            let v = o.value.trim();
            if !v.is_empty() && v != o.default {
                if v.contains([':', ',', '=']) {
                    return Err(format!(
                        "{}: 옵션 '{}' 값에 : , = 는 쓸 수 없습니다",
                        self.id, o.id
                    ));
                }
                args.push((o.id.clone(), v.to_string()));
            }
        }
        Ok(StackEntry {
            id: self.id.clone(),
            args,
        })
    }
}

/// A decode running in the background (DEC-4).
pub struct Job {
    pub ctl: Arc<Control>,
    rx: mpsc::Receiver<Result<(i64, usize), String>>,
    pub label: String,
    started: Instant,
    path: PathBuf,
}

/// What the window asks the app to do.
pub enum Event {
    /// An analyzer was stored in this file; reload the list.
    Added {
        path: PathBuf,
    },
    Failed(String),
}

pub struct DecodeUi {
    pub open: bool,
    catalog: Catalog,
    entries: Vec<EntryForm>,
    label: String,
    filter: String,
    pub job: Option<Job>,
    pub message: Option<String>,
}

impl Default for DecodeUi {
    fn default() -> Self {
        DecodeUi {
            open: false,
            catalog: Catalog::NotLoaded,
            entries: Vec::new(),
            label: String::new(),
            filter: String::new(),
            job: None,
            message: None,
        }
    }
}

fn start_catalog(ctx: &egui::Context) -> Catalog {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = lv_decode::runtime()
            .map(|rt| rt.decoders())
            .map_err(|e| e.to_string());
        let _ = tx.send(r);
        ctx.request_repaint();
    });
    Catalog::Loading(rx)
}

impl DecodeUi {
    /// Poll the catalog and a running job.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Event> {
        if let Catalog::Loading(rx) = &self.catalog
            && let Ok(r) = rx.try_recv()
        {
            self.catalog = match r {
                Ok(list) => Catalog::Ready(list),
                Err(e) => Catalog::Failed(e),
            };
        }
        let job = self.job.as_ref()?;
        match job.rx.try_recv() {
            Ok(r) => {
                let job = self.job.take().expect("job");
                let secs = job.started.elapsed().as_secs_f64();
                Some(match r {
                    Ok((_, frames)) => {
                        self.message =
                            Some(format!("{}: 프레임 {}개, {:.2} s", job.label, frames, secs));
                        Event::Added { path: job.path }
                    }
                    Err(e) => {
                        self.message = Some(e.clone());
                        Event::Failed(e)
                    }
                })
            }
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.job = None;
                Some(Event::Failed("디코딩 스레드가 멈췄습니다".into()))
            }
        }
    }

    pub fn progress_text(&self) -> Option<String> {
        let j = self.job.as_ref()?;
        let done = j.ctl.done.load(Ordering::Relaxed) as f64;
        let total = j.ctl.total.load(Ordering::Relaxed).max(1) as f64;
        Some(format!(
            "디코딩 중: {} {:.0}%",
            j.label,
            100.0 * done / total
        ))
    }

    /// The window. `file` is the open capture (None or recording: no decoding).
    pub fn window(
        &mut self,
        ctx: &egui::Context,
        file: Option<(&std::path::Path, &[String])>,
        recording: bool,
    ) {
        if !self.open {
            return;
        }
        if matches!(self.catalog, Catalog::NotLoaded) {
            self.catalog = start_catalog(ctx);
        }
        let mut open = self.open;
        egui::Window::new("디코더 추가")
            .open(&mut open)
            .default_width(420.0)
            .show(ctx, |ui| self.contents(ui, file, recording));
        self.open = open;
    }

    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        file: Option<(&std::path::Path, &[String])>,
        recording: bool,
    ) {
        let list = match &self.catalog {
            Catalog::Ready(l) => l.clone(),
            Catalog::Failed(e) => {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    format!("디코더 런타임을 쓸 수 없습니다: {e}"),
                );
                return;
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("디코더를 불러오는 중… (처음 한 번, 몇 초)");
                });
                return;
            }
        };
        let Some((path, names)) = file else {
            ui.label("먼저 캡처를 열거나 수집하세요.");
            return;
        };
        let find = |id: &str| list.iter().find(|d| d.id == id);

        // First decoder: those that read logic signals, picked from a
        // searchable list (131 decoders are too many for a combo box).
        let first_id = self
            .entries
            .first()
            .map(|e| e.id.clone())
            .unwrap_or_default();
        let mut pick = None;
        ui.horizontal(|ui| {
            ui.label("디코더");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .desired_width(160.0)
                    .hint_text("찾기 (예: uart, i2c)"),
            );
            if let Some(d) = find(&first_id) {
                ui.label(format!("선택: {} - {}", d.id, d.longname));
            }
        });
        let f = self.filter.to_lowercase();
        if self.entries.is_empty() || !f.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("dec-list")
                .max_height(180.0)
                .show(ui, |ui| {
                    for d in list.iter().filter(|d| {
                        d.inputs.iter().any(|i| i == "logic")
                            && (f.is_empty()
                                || d.id.to_lowercase().contains(&f)
                                || d.longname.to_lowercase().contains(&f))
                    }) {
                        if ui
                            .selectable_label(
                                d.id == first_id,
                                format!("{} - {}", d.id, d.longname),
                            )
                            .clicked()
                        {
                            pick = Some(d.id.clone());
                        }
                    }
                });
        }
        if pick.is_some() {
            self.filter.clear();
        }
        if let Some(id) = pick
            && let Some(d) = find(&id)
        {
            self.entries = vec![EntryForm::new(d, names)];
            self.label = d.name.clone();
        }
        if self.entries.is_empty() {
            return;
        }

        let mut remove_from = None;
        for (level, e) in self.entries.iter_mut().enumerate() {
            let Some(d) = find(&e.id) else { continue };
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong(format!(
                    "{}{} - {}",
                    if level > 0 { "↑ " } else { "" },
                    d.id,
                    d.longname
                ));
                if level > 0 && ui.small_button("빼기").clicked() {
                    remove_from = Some(level);
                }
            });
            egui::Grid::new(("dec-grid", level))
                .num_columns(2)
                .show(ui, |ui| {
                    if level == 0 {
                        for (cid, name, required, ch) in e.channels.iter_mut() {
                            ui.label(format!("{name}{}", if *required { " *" } else { "" }))
                                .on_hover_text(cid.as_str());
                            let text = ch
                                .map(|i| {
                                    format!("D{i}  {}", names.get(i).cloned().unwrap_or_default())
                                })
                                .unwrap_or_else(|| "없음".into());
                            egui::ComboBox::from_id_salt(("dec-ch", level, cid.as_str()))
                                .selected_text(text)
                                .width(180.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(ch, None, "없음");
                                    for (i, n) in names.iter().enumerate() {
                                        ui.selectable_value(ch, Some(i), format!("D{i}  {n}"));
                                    }
                                });
                            ui.end_row();
                        }
                    }
                    for o in e.options.iter_mut() {
                        ui.label(&o.desc).on_hover_text(o.id.as_str());
                        if o.choices.is_empty() {
                            ui.add(egui::TextEdit::singleline(&mut o.value).desired_width(180.0));
                        } else {
                            egui::ComboBox::from_id_salt(("dec-opt", level, o.id.as_str()))
                                .selected_text(o.value.clone())
                                .width(180.0)
                                .show_ui(ui, |ui| {
                                    for c in &o.choices {
                                        ui.selectable_value(&mut o.value, c.clone(), c);
                                    }
                                });
                        }
                        ui.end_row();
                    }
                });
        }
        if let Some(l) = remove_from {
            self.entries.truncate(l);
        }

        // Stack another decoder on the top one (DEC-2).
        let top = self.entries.last().and_then(|e| find(&e.id));
        if let Some(top) = top {
            let above: Vec<&DecoderInfo> = list
                .iter()
                .filter(|d| d.inputs.iter().any(|i| top.outputs.contains(i)))
                .collect();
            if !above.is_empty() {
                let mut add = None;
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(("dec-stack", self.entries.len()))
                        .selected_text("위에 쌓을 디코더…")
                        .width(300.0)
                        .height(300.0)
                        .show_ui(ui, |ui| {
                            for d in &above {
                                if ui
                                    .selectable_label(false, format!("{} - {}", d.id, d.longname))
                                    .clicked()
                                {
                                    add = Some(d.id.clone());
                                }
                            }
                        });
                });
                if let Some(id) = add
                    && let Some(d) = find(&id)
                {
                    self.entries.push(EntryForm::new(d, names));
                    self.label = d.name.clone();
                }
            }
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("이름");
            ui.add(egui::TextEdit::singleline(&mut self.label).desired_width(200.0));
        });
        let stack: Result<Vec<StackEntry>, String> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| e.stack_entry(i == 0))
            .collect();
        ui.horizontal(|ui| {
            let busy = self.job.is_some();
            if let Some(j) = &self.job {
                let done = j.ctl.done.load(Ordering::Relaxed) as f32;
                let total = j.ctl.total.load(Ordering::Relaxed).max(1) as f32;
                ui.add(
                    egui::ProgressBar::new(done / total)
                        .show_percentage()
                        .desired_width(160.0),
                );
                if ui.button("취소").clicked() {
                    j.ctl.cancel.store(true, Ordering::Relaxed);
                }
            } else if ui
                .add_enabled(!recording && stack.is_ok(), egui::Button::new("디코딩"))
                .clicked()
                && let Ok(stack) = &stack
            {
                self.job = Some(start_job(
                    path.to_path_buf(),
                    stack.clone(),
                    self.label.clone(),
                    ui.ctx(),
                ));
                self.message = None;
            }
            if !busy {
                match &stack {
                    Err(e) => {
                        ui.colored_label(Color32::LIGHT_RED, e);
                    }
                    Ok(s) => {
                        ui.label(lv_decode::stack_text(s))
                            .on_hover_text("sigrok-cli -P 형식");
                    }
                }
            }
        });
        if recording {
            ui.label("수집이 끝난 뒤 디코딩할 수 있습니다.");
        }
        if let Some(m) = &self.message {
            ui.label(m);
        }
    }
}

fn start_job(path: PathBuf, stack: Vec<StackEntry>, label: String, ctx: &egui::Context) -> Job {
    let ctl = Arc::new(Control::default());
    let (tx, rx) = mpsc::channel();
    let job = Job {
        ctl: ctl.clone(),
        rx,
        label: label.clone(),
        started: Instant::now(),
        path: path.clone(),
    };
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("lv-decode".into())
        .spawn(move || {
            let r = (|| -> Result<(i64, usize), String> {
                let rt = lv_decode::runtime().map_err(|e| e.to_string())?;
                let file = CaptureFile::open(&path).map_err(|e| e.to_string())?;
                let d = rt.decode(&file, &stack, &ctl).map_err(|e| e.to_string())?;
                drop(file);
                let (settings, frames) = d.to_frames(&stack);
                let id = lv_store::analysis::add_analyzer(
                    &path,
                    &lv_store::analysis::NewAnalyzer {
                        kind: lv_decode::analyzer_kind(&stack).into(),
                        decoder_id: settings.stack.clone(),
                        label,
                        settings_json: settings.to_json(),
                        input_id: None,
                    },
                    &frames,
                )
                .map_err(|e| format!("결과를 저장하지 못했습니다: {e}"))?;
                Ok((id, frames.len()))
            })();
            let _ = tx.send(r);
            ctx.request_repaint();
        })
        .expect("spawn decode thread");
    job
}

// ---- data table (UI-9) --------------------------------------------------------------

#[derive(Default)]
pub struct TableUi {
    pub show: bool,
    sel: usize,
    search: String,
    count: Option<(i64, String, u64)>,
    page: Option<(i64, String, usize, Vec<Frame>)>,
    pub selected: Option<(u64, u64)>,
}

/// What the table asks the app to do.
pub enum TableAction {
    /// Show these samples.
    Jump(u64, u64),
    Delete(i64),
}

impl TableUi {
    pub fn panel(
        &mut self,
        ui: &mut egui::Ui,
        file: &CaptureFile,
        analyzers: &mut [Analyzer],
        rate: f64,
    ) -> Option<TableAction> {
        if analyzers.is_empty() {
            ui.label("분석기가 없습니다. '디코더…'로 추가하세요.");
            return None;
        }
        self.sel = self.sel.min(analyzers.len() - 1);
        let mut action = None;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("table-analyzer")
                .selected_text(&analyzers[self.sel].info.label)
                .show_ui(ui, |ui| {
                    for (i, a) in analyzers.iter().enumerate() {
                        ui.selectable_value(&mut self.sel, i, &a.info.label);
                    }
                });
            ui.label("검색");
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(160.0)
                    .hint_text("텍스트나 종류"),
            );
            ui.checkbox(&mut analyzers[self.sel].shown, "파형에 보이기");
            if ui
                .button("삭제")
                .on_hover_text("이 분석기와 결과를 파일에서 지움")
                .clicked()
            {
                action = Some(TableAction::Delete(analyzers[self.sel].info.id));
            }
        });
        let a = &analyzers[self.sel];
        let id = a.info.id;
        let fresh = self
            .count
            .as_ref()
            .is_none_or(|(cid, cs, _)| *cid != id || *cs != self.search);
        if fresh {
            let n = file.frame_count(id, &self.search).unwrap_or(0);
            self.count = Some((id, self.search.clone(), n));
            self.page = None;
        }
        let total = self.count.as_ref().map(|c| c.2).unwrap_or(0);
        ui.label(format!("{} 프레임  ·  {}", total, a.info.decoder_id));
        let row_h = 18.0;
        let tick = 1.0 / rate.max(1.0);
        egui::ScrollArea::vertical()
            .id_salt(("table", id))
            .auto_shrink([false, false])
            .show_rows(ui, row_h, total as usize, |ui, range| {
                let want = (id, self.search.clone(), range.start);
                let stale = self.page.as_ref().is_none_or(|(pi, ps, po, rows)| {
                    (*pi, ps.as_str(), *po) != (want.0, want.1.as_str(), want.2)
                        || rows.len() < range.len().min(total as usize - range.start)
                });
                if stale {
                    let rows = file
                        .frames_page(id, &self.search, range.start as u64, range.len())
                        .unwrap_or_default();
                    self.page = Some((want.0, want.1, want.2, rows));
                }
                let rows = &self.page.as_ref().expect("page").3;
                for (i, f) in rows.iter().enumerate() {
                    let n = range.start + i;
                    let sel = self.selected == Some((f.start, f.end));
                    let line = format!(
                        "{:>6}  {:>14}  {:>10}  {:<22} {}",
                        n + 1,
                        trim(&wave::fmt_time(f.start as f64 / rate, tick)),
                        trim(&wave::fmt_time((f.end - f.start) as f64 / rate, tick)),
                        f.kind,
                        f.text
                    );
                    let (rect, r) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::click(),
                    );
                    let vis = ui.visuals();
                    if sel {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::ZERO, vis.selection.bg_fill);
                    } else if r.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::ZERO,
                            vis.widgets.hovered.weak_bg_fill,
                        );
                    }
                    ui.painter().text(
                        rect.left_center() + egui::vec2(4.0, 0.0),
                        Align2::LEFT_CENTER,
                        line,
                        FontId::monospace(12.0),
                        vis.text_color(),
                    );
                    if r.clicked() {
                        self.selected = Some((f.start, f.end));
                        action = Some(TableAction::Jump(f.start, f.end));
                    }
                }
            });
        action
    }
}

fn trim(s: &str) -> String {
    match s.split_once(' ') {
        Some((num, unit)) if num.contains('.') => {
            format!("{} {unit}", num.trim_end_matches('0').trim_end_matches('.'))
        }
        _ => s.to_string(),
    }
}
