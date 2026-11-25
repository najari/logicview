//! The settings window (like PulseView's in the WeAct U3LogicAnalyzer):
//! general, view, decoders, about and logging.

use eframe::egui::{self, Color32};

use super::App;
use crate::applog;
use crate::settings::{Theme, ViewSettings};

const TABS: [&str; 5] = ["일반", "보기", "디코더", "정보", "로그"];

pub fn apply_theme(ctx: &egui::Context, t: Theme) {
    ctx.set_theme(match t {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    });
}

/// Hand the logging settings to the logger.
pub fn apply_log(v: &ViewSettings) {
    if let Some(l) = applog::parse_level(&v.log_level) {
        applog::set_level(l);
    }
    applog::set_capacity(v.log_lines);
}

impl App {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        egui::Window::new("설정")
            .open(&mut open)
            .default_width(560.0)
            .default_height(460.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (i, t) in TABS.iter().enumerate() {
                        ui.selectable_value(&mut self.settings_tab, i, *t);
                    }
                });
                ui.separator();
                match self.settings_tab {
                    0 => self.general_tab(ui),
                    1 => self.view_tab(ui),
                    2 => self.decoder_tab(ui),
                    3 => self.about_tab(ui),
                    _ => self.log_tab(ui),
                }
            });
        self.show_settings = open;
        if self.view_opts != self.saved_view {
            if self.view_opts.theme != self.saved_view.theme {
                apply_theme(ctx, self.view_opts.theme);
            }
            apply_log(&self.view_opts);
            if let Err(e) = self.view_opts.save(&self.db) {
                log::warn!("saving view settings: {e}");
            }
            self.saved_view = self.view_opts.clone();
        }
    }

    fn general_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("화면 테마");
            for (t, name) in Theme::ALL {
                ui.radio_value(&mut self.view_opts.theme, t, name);
            }
        });
        ui.label("언어: 한국어");
        ui.add_space(6.0);
        ui.label(format!(
            "설정은 {}에 저장됩니다.",
            crate::settings::default_path().display()
        ));
    }

    fn view_tab(&mut self, ui: &mut egui::Ui) {
        let v = &mut self.view_opts;
        ui.checkbox(&mut v.follow_live, "수집 중에 계속 전체 보기");
        ui.checkbox(&mut v.fit_on_stop, "수집이 끝나면 전체 보기");
        ui.checkbox(
            &mut v.zero_at_trigger,
            "시간 0을 트리거 위치에 (트리거가 있는 캡처)",
        );
        ui.checkbox(
            &mut v.sample_dots,
            "샘플 점 보이기 (샘플 사이가 넓게 보일 만큼 확대했을 때)",
        );
        ui.horizontal(|ui| {
            ui.checkbox(&mut v.fill_high, "High 구간 채우기");
            let mut c = Color32::from_rgba_unmultiplied(
                v.fill_color[0],
                v.fill_color[1],
                v.fill_color[2],
                v.fill_color[3],
            );
            if ui.color_edit_button_srgba(&mut c).changed() {
                v.fill_color = c.to_srgba_unmultiplied();
            }
        });
        ui.checkbox(&mut v.cursor_line, "마우스 커서에 세로선");
        ui.horizontal(|ui| {
            ui.label("마커가 에지에 붙는 거리");
            ui.add(
                egui::DragValue::new(&mut v.snap_px)
                    .range(0.0..=50.0)
                    .suffix(" 픽셀"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("채널 줄 높이");
            let mut auto = v.row_height == 0.0;
            if ui.checkbox(&mut auto, "창에 맞춤").changed() {
                v.row_height = if auto { 0.0 } else { 40.0 };
            }
            // Its own value: the range would clamp the 0 that means "fit".
            let mut h = if auto { 40.0 } else { v.row_height };
            if ui
                .add_enabled(
                    !auto,
                    egui::DragValue::new(&mut h)
                        .range(16.0..=300.0)
                        .suffix(" 픽셀"),
                )
                .changed()
                && !auto
            {
                v.row_height = h;
            }
        });
        if ui.button("기본값으로").clicked() {
            let keep = (v.theme, v.log_level.clone(), v.log_lines);
            *v = ViewSettings::default();
            (v.theme, v.log_level, v.log_lines) = keep;
        }
    }

    fn decoder_tab(&mut self, ui: &mut egui::Ui) {
        self.decode.paths_ui(ui, &self.db);
    }

    fn about_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("about").num_columns(2).show(ui, |ui| {
                ui.label("logicview");
                ui.label(env!("CARGO_PKG_VERSION"));
                ui.end_row();
                ui.label("디코더 런타임");
                ui.label(match lv_decode::running_dir() {
                    Some(d) => d.display().to_string(),
                    None => "아직 시작 안 함".into(),
                });
                ui.end_row();
                ui.label("지원 장치");
                ui.label(
                    "Saleae Logic 호환 FX2 (0925:3881, fx2lafw, 8채널 24 MHz)\n\
                     WeAct Logic Analyzer V2 (1a86:5537, 16채널 200 MHz)",
                );
                ui.end_row();
                ui.label("열 수 있는 파일");
                ui.label(".lvcap, .sr (sigrok 세션)");
                ui.end_row();
                ui.label("저장할 수 있는 형식");
                ui.label(".lvcap, .sr, CSV (MCP·lvcli: Saleae 바이너리, raw)");
                ui.end_row();
            });
            ui.separator();
            ui.label("펌웨어 찾는 곳:");
            for d in lv_device::default_firmware_dirs() {
                ui.label(format!("  {}", d.display()));
            }
            ui.label("디코더 런타임 찾는 곳:");
            for d in lv_decode::runtime_candidates() {
                ui.label(format!("  {}", d.display()));
            }
            if let Some(p) = std::env::var_os("SIGROKDECODE_PATH") {
                ui.label("추가 디코더 폴더 (SIGROKDECODE_PATH):");
                for d in std::env::split_paths(&p) {
                    ui.label(format!("  {}", d.display()));
                }
            }
            ui.separator();
            match self.decode.decoder_list() {
                Some(list) => {
                    ui.label(format!("프로토콜 디코더 {}개:", list.len()));
                    egui::Grid::new("about-dec")
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| {
                            for d in list {
                                ui.monospace(&d.id);
                                ui.label(&d.longname);
                                ui.end_row();
                            }
                        });
                }
                None => {
                    if ui.button("프로토콜 디코더 목록 불러오기").clicked() {
                        self.decode.load_catalog(ui.ctx());
                    }
                }
            }
        });
    }

    fn log_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("로그 수준");
            let cur = applog::parse_level(&self.view_opts.log_level);
            let name = applog::LEVELS
                .iter()
                .find(|(l, _)| Some(*l) == cur)
                .map(|(_, n)| *n)
                .unwrap_or("경고");
            egui::ComboBox::from_id_salt("log-level")
                .selected_text(name)
                .show_ui(ui, |ui| {
                    for (l, n) in applog::LEVELS {
                        if ui.selectable_label(Some(l) == cur, n).clicked() {
                            self.view_opts.log_level = l.to_string().to_lowercase();
                        }
                    }
                });
            ui.label("보관할 줄");
            ui.add(
                egui::DragValue::new(&mut self.view_opts.log_lines)
                    .range(100..=1_000_000)
                    .speed(50),
            );
        });
        let lines = applog::lines();
        ui.horizontal(|ui| {
            if ui.button("파일로 저장…").clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .set_file_name("logicview.log")
                    .add_filter("로그", &["log", "txt"])
                    .save_file()
            {
                self.log_msg = Some(match std::fs::write(&p, lines.join("\n") + "\n") {
                    Ok(()) => format!("{}에 저장했습니다", p.display()),
                    Err(e) => format!("저장하지 못했습니다: {e}"),
                });
            }
            if ui.button("지우기").clicked() {
                applog::clear();
            }
            ui.label(format!("{}줄", lines.len()));
            if let Some(m) = &self.log_msg {
                ui.label(m);
            }
        });
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for l in &lines {
                    ui.monospace(l);
                }
            });
    }
}
