//! Firmware update window (the WeAct U3LogicAnalyzer's "FW Update"): pick a
//! `.bin` or `.hex`, switch the board into its bootloader and write it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui::{self, Color32};
use lv_device::iap;

use super::App;

enum FwMsg {
    Progress(iap::Progress),
    Done(Result<(), String>),
}

#[derive(Default)]
pub struct FwUpdate {
    pub open: bool,
    path: String,
    slot: usize,
    /// The board already sits in its bootloader (`1a86:fe17`).
    in_bootloader: bool,
    rx: Option<Receiver<FwMsg>>,
    cancel: Arc<AtomicBool>,
    percent: u8,
    log: Vec<String>,
    result: Option<Result<(), String>>,
}

impl FwUpdate {
    pub fn running(&self) -> bool {
        self.rx.is_some()
    }
}

impl App {
    pub(super) fn fw_window(&mut self, ctx: &egui::Context) {
        // Progress from the worker.
        let mut finished = false;
        if let Some(rx) = &self.fw.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    FwMsg::Progress(p) => {
                        self.fw.percent = p.percent;
                        self.fw.log.push(p.text);
                    }
                    FwMsg::Done(r) => {
                        if let Err(e) = &r {
                            self.fw.log.push(format!("실패: {e}"));
                        }
                        self.fw.result = Some(r);
                        finished = true;
                    }
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if finished {
            self.fw.rx = None;
            // The board comes back with its new firmware: look again.
            self.scan_devices(ctx);
        }
        if !self.fw.open {
            return;
        }
        let running = self.fw.running();
        let found = self.selected_device().filter(|d| d.is_u3()).cloned();
        let mut open = true;
        let mut start = false;
        egui::Window::new("펌웨어 업데이트")
            .open(&mut open)
            .resizable(false)
            .default_width(480.0)
            .show(ctx, |ui| {
                match &found {
                    Some(f) => ui.label(format!(
                        "보드: {} {} (일련번호 {})",
                        f.location(),
                        f.name(),
                        f.serial().unwrap_or("-")
                    )),
                    None => ui.label("WeAct Logic Analyzer V2를 장치 목록에서 고르거나, 업데이트 모드로 켠 보드를 쓰세요."),
                };
                ui.add_enabled_ui(!running, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("파일");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.fw.path)
                                .desired_width(300.0)
                                .hint_text(".bin 또는 .hex"),
                        );
                        if ui.button("찾아보기…").clicked()
                            && let Some(p) = rfd::FileDialog::new()
                                .add_filter("펌웨어", &["bin", "hex"])
                                .pick_file()
                        {
                            self.fw.path = p.display().to_string();
                        }
                    });
                    let is_hex = self.fw.path.to_ascii_lowercase().ends_with(".hex");
                    ui.add_enabled_ui(!is_hex, |ui| {
                        ui.label(".bin을 쓸 곳 (.hex는 파일의 주소)");
                        for (i, (addr, name)) in iap::SLOTS.iter().enumerate() {
                            ui.radio_value(&mut self.fw.slot, i, format!("{name} (0x{addr:08x})"));
                        }
                    });
                    ui.checkbox(
                        &mut self.fw.in_bootloader,
                        "보드가 이미 업데이트 모드에 있음 (KEY를 누른 채 켠 보드, 1a86:fe17)",
                    );
                });
                ui.colored_label(
                    Color32::LIGHT_YELLOW,
                    "업데이트 중에는 보드를 뽑지 마세요. 실패하면 KEY를 누른 채 전원을 넣어 \
                     업데이트 모드로 켠 뒤 다시 쓸 수 있습니다.",
                );
                ui.horizontal(|ui| {
                    if running {
                        if ui.button("취소").clicked() {
                            self.fw.cancel.store(true, Ordering::Relaxed);
                        }
                    } else {
                        let can = !self.fw.path.trim().is_empty()
                            && (found.is_some() || self.fw.in_bootloader)
                            && self.session.is_none();
                        start = ui
                            .add_enabled(can, egui::Button::new("업데이트 시작"))
                            .clicked();
                    }
                    ui.add(
                        egui::ProgressBar::new(f32::from(self.fw.percent) / 100.0)
                            .show_percentage(),
                    );
                });
                match &self.fw.result {
                    Some(Ok(())) => {
                        ui.colored_label(Color32::LIGHT_GREEN, "업데이트를 마쳤습니다");
                    }
                    Some(Err(_)) => {
                        ui.colored_label(Color32::LIGHT_RED, "업데이트하지 못했습니다");
                    }
                    None => {}
                }
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for l in &self.fw.log {
                            ui.monospace(l);
                        }
                    });
            });
        if start {
            self.start_fw_update(found);
        }
        if !running {
            self.fw.open = open;
        }
    }

    fn start_fw_update(&mut self, found: Option<lv_device::Found>) {
        let path = std::path::PathBuf::from(self.fw.path.trim());
        let image = match iap::load_image(&path, self.fw.slot) {
            Ok(i) => i,
            Err(e) => {
                self.fw.log.push(e.to_string());
                self.fw.result = Some(Err(e.to_string()));
                return;
            }
        };
        self.fw.log = vec![format!(
            "{}: {} 바이트, 0x{:08x} ({})",
            path.display(),
            image.data.len(),
            image.addr,
            image.slot().map(|s| iap::SLOTS[s].1).unwrap_or("슬롯 아님")
        )];
        self.fw.result = None;
        self.fw.percent = 0;
        self.fw.cancel = Arc::new(AtomicBool::new(false));
        // The key watcher must leave the board alone now.
        self.key_watch = None;
        let (tx, rx) = mpsc::channel();
        self.fw.rx = Some(rx);
        let (slot, lock, cancel) = (
            self.slot.clone(),
            self.usb_lock.clone(),
            self.fw.cancel.clone(),
        );
        let in_bootloader = self.fw.in_bootloader;
        thread::spawn(move || {
            let serial = found.as_ref().and_then(|f| f.serial().map(String::from));
            let r = (|| -> Result<(), String> {
                if !in_bootloader {
                    let f = found.ok_or("보드를 고르세요")?;
                    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
                    let mut dev = match lv_device::reuse(slot.take(), &f) {
                        Some(d) => d,
                        None => lv_device::open(&f).map_err(|e| e.to_string())?,
                    };
                    let _ = tx.send(FwMsg::Progress(iap::Progress {
                        percent: 5,
                        text: "업데이트 모드로 바꾸는 중…".into(),
                    }));
                    dev.enter_update_mode().map_err(|e| e.to_string())?;
                    // Closing lets the board go; it comes back as a HID device.
                    drop(dev);
                }
                iap::upgrade(&image, serial.as_deref(), &cancel, |p| {
                    let _ = tx.send(FwMsg::Progress(p));
                })
                .map_err(|e| e.to_string())
            })();
            let _ = tx.send(FwMsg::Done(r));
        });
    }
}
