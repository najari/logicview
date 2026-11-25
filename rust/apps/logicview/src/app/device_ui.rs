//! The device options window (like the WeAct U3LogicAnalyzer's): port,
//! model and serial number, input level and threshold, pre-trigger share,
//! board key and indicator brightness, channel mode and shown channels. The
//! board key is watched on a background thread while it is turned on.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, Color32};
use lv_device::{DeviceSlot, Found};

use super::{App, COLORS, fmt_rate};

/// Indicator brightness: label and percent.
pub const LED_CHOICES: [(&str, u8); 5] = [
    ("어둡게", 10),
    ("기본", 20),
    ("밝게", 40),
    ("더 밝게", 70),
    ("최대", 100),
];

/// How often the board key is asked about.
const KEY_POLL: Duration = Duration::from_millis(250);

pub enum DevEvent {
    /// The board key was pressed while no capture was running.
    KeyPressed,
    Message(String),
}

/// Asks the board for its key count while the option is on.
pub struct KeyWatch {
    location: String,
    stop: Arc<AtomicBool>,
    /// Set while a capture runs: the capture owns the device then.
    pub paused: Arc<AtomicBool>,
}

impl Drop for KeyWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Run `f` on the device for `found`: the kept one, or a freshly opened
/// one, which is kept afterwards. `lock` keeps a capture start from taking
/// the device at the same moment.
pub fn with_device<R>(
    found: &Found,
    slot: &DeviceSlot,
    lock: &Mutex<()>,
    f: impl FnOnce(&mut lv_device::Device) -> R,
) -> Result<R, String> {
    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut dev = match lv_device::reuse(slot.take(), found) {
        Some(d) => d,
        None => lv_device::open(found).map_err(|e| e.to_string())?,
    };
    let r = f(&mut dev);
    slot.put(dev);
    Ok(r)
}

fn spawn_key_watch(
    found: Found,
    slot: DeviceSlot,
    lock: Arc<Mutex<()>>,
    tx: Sender<DevEvent>,
    ctx: egui::Context,
) -> KeyWatch {
    let stop = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    let w = KeyWatch {
        location: found.location().to_string(),
        stop: stop.clone(),
        paused: paused.clone(),
    };
    thread::Builder::new()
        .name("lv-key-watch".into())
        .spawn(move || {
            // The count when watching (re)started; a change is a press.
            let mut base: Option<u8> = None;
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(KEY_POLL);
                if paused.load(Ordering::Relaxed) {
                    // The press that stopped a capture is not a new start.
                    base = None;
                    continue;
                }
                let count = with_device(&found, &slot, &lock, |d| d.key_count());
                match count {
                    Ok(Ok(Some(c))) => match base {
                        Some(b) if b != c => {
                            base = None;
                            if !paused.load(Ordering::Relaxed) {
                                let _ = tx.send(DevEvent::KeyPressed);
                                ctx.request_repaint();
                            }
                        }
                        _ => base = Some(c),
                    },
                    Ok(Ok(None)) => {
                        let _ = tx.send(DevEvent::Message(
                            "이 펌웨어는 보드 버튼을 알리지 않습니다 (0.2 이상 필요)".into(),
                        ));
                        ctx.request_repaint();
                        break;
                    }
                    Ok(Err(e)) => log::debug!("key count: {e}"),
                    Err(e) => {
                        log::debug!("key watch: {e}");
                        thread::sleep(Duration::from_secs(1));
                    }
                }
            }
        })
        .expect("spawn key watch");
    w
}

impl App {
    pub(super) fn selected_device(&self) -> Option<&Found> {
        self.devices.get(self.dev_sel)
    }

    /// The toolbar button and what it says about the settings.
    pub(super) fn device_options_button(&mut self, ui: &mut egui::Ui) {
        let Some(found) = self.selected_device() else {
            return;
        };
        let n = self.channels_for(found);
        let mut text = format!("{n}채널");
        if found.is_u3() {
            text.push_str(&format!(" · {:.1} V", self.level));
            if self.key_capture {
                text.push_str(" · 버튼");
            }
        }
        if ui
            .button(format!("장치 옵션 ({text})…"))
            .on_hover_text("포트, 채널 모드, 입력 레벨, 앞 구간, 보드 버튼, 표시등")
            .clicked()
        {
            self.show_device = !self.show_device;
        }
    }

    /// Keep the key watcher in line with the option and the selection, and
    /// handle what it and the LED jobs report.
    pub(super) fn poll_device(&mut self, ctx: &egui::Context) {
        let want = self
            .selected_device()
            .filter(|d| d.is_u3() && self.key_capture && !self.fw.running())
            .cloned();
        match (&self.key_watch, &want) {
            (Some(w), Some(f)) if w.location == f.location() => {}
            (None, None) => {}
            _ => {
                self.key_watch = want.map(|f| {
                    spawn_key_watch(
                        f,
                        self.slot.clone(),
                        self.usb_lock.clone(),
                        self.dev_tx.clone(),
                        ctx.clone(),
                    )
                });
            }
        }
        if let Some(w) = &self.key_watch {
            w.paused.store(self.session.is_some(), Ordering::Relaxed);
        }
        while let Ok(ev) = self.dev_rx.try_recv() {
            match ev {
                DevEvent::KeyPressed => {
                    if self.session.is_none() {
                        self.start_capture(ctx);
                    }
                }
                DevEvent::Message(m) => self.device_msg = Some(m),
            }
        }
    }

    fn set_led(&mut self, level: u8) {
        let Some(found) = self.selected_device().cloned() else {
            return;
        };
        let (slot, lock, tx) = (
            self.slot.clone(),
            self.usb_lock.clone(),
            self.dev_tx.clone(),
        );
        self.device_msg = Some("표시등 밝기를 바꾸는 중…".into());
        thread::spawn(move || {
            let msg = match with_device(&found, &slot, &lock, |d| d.set_led_brightness(level)) {
                Ok(Ok(())) => format!("표시등 밝기 {level}%"),
                Ok(Err(e)) => format!("표시등 밝기를 바꾸지 못했습니다: {e}"),
                Err(e) => format!("장치를 열 수 없습니다: {e}"),
            };
            let _ = tx.send(DevEvent::Message(msg));
        });
    }

    pub(super) fn device_window(&mut self, ctx: &egui::Context) {
        if !self.show_device {
            return;
        }
        let Some(found) = self.selected_device().cloned() else {
            self.show_device = false;
            return;
        };
        let busy = self.session.is_some();
        let mut open = true;
        let mut led = None;
        let mut channels_changed = false;
        egui::Window::new("장치 옵션")
            .open(&mut open)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                egui::Grid::new("dev-info").num_columns(2).show(ui, |ui| {
                    ui.label("포트");
                    ui.label(format!("{} ({})", found.location(), found.transport()));
                    ui.end_row();
                    ui.label("모델");
                    ui.label(found.name());
                    ui.end_row();
                    if let Some(sn) = found.serial() {
                        ui.label("일련번호");
                        ui.label(sn);
                        ui.end_row();
                    }
                });
                ui.add_enabled_ui(!busy, |ui| {
                    ui.group(|ui| {
                        ui.label("옵션");
                        if found.is_u3() {
                            let t = lv_device::threshold_for_level(self.level);
                            ui.label(format!(
                                "입력 문턱: {t:.2} V, {:.2} V (버퍼 보드)",
                                self.level / 2.0 * 1.394
                            ));
                            ui.horizontal(|ui| {
                                ui.label("입력 레벨");
                                let r = ui.add(
                                    egui::DragValue::new(&mut self.level)
                                        .range(1.0..=3.3)
                                        .speed(0.05)
                                        .fixed_decimals(1)
                                        .suffix(" V"),
                                );
                                if r.changed() {
                                    self.level = (self.level * 10.0).round() / 10.0;
                                }
                                r.on_hover_text(
                                    "측정하는 신호의 High 전압에 맞출 것. 다르면 샘플을 잘못 읽는다",
                                );
                            });
                        }
                        ui.horizontal(|ui| {
                            ui.label("앞 구간");
                            ui.add(
                                egui::DragValue::new(&mut self.pre_percent)
                                    .range(0..=100)
                                    .suffix(" %"),
                            )
                            .on_hover_text("트리거 앞에 둘 샘플의 비율 (트리거와 시간을 정했을 때)");
                        });
                        if found.is_u3() {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut self.key_capture, "보드 버튼으로 수집")
                                    .on_hover_text(
                                        "보드의 버튼을 짧게 누르면 수집을 시작하고, 수집 중에 누르면 멈춘다 \
                                         (펌웨어 0.2 이상). 2초 누르면 여전히 슬롯을 바꾼다",
                                    );
                                ui.label("표시등 밝기");
                                let cur = LED_CHOICES
                                    .iter()
                                    .find(|(_, v)| *v == self.led)
                                    .map(|(n, _)| *n)
                                    .unwrap_or("기본");
                                egui::ComboBox::from_id_salt("dev-led")
                                    .selected_text(cur)
                                    .width(80.0)
                                    .show_ui(ui, |ui| {
                                        for (name, v) in LED_CHOICES {
                                            if ui.selectable_label(self.led == v, name).clicked()
                                                && self.led != v
                                            {
                                                self.led = v;
                                                led = Some(v);
                                            }
                                        }
                                    });
                            });
                        }
                        for &n in found.channel_choices() {
                            let max = found
                                .samplerates(n)
                                .last()
                                .map(|&r| fmt_rate(r))
                                .unwrap_or_default();
                            if ui
                                .radio(
                                    self.channels_for(&found) == n,
                                    format!("{n}채널 (최대 {max})"),
                                )
                                .clicked()
                            {
                                self.channels = n;
                                channels_changed = true;
                            }
                        }
                    });
                });
                ui.group(|ui| {
                    ui.label("채널 (보이기)");
                    let n = self.channels_for(&found);
                    let mut changed = false;
                    egui::Grid::new("dev-channels").num_columns(8).show(ui, |ui| {
                        for c in 0..n {
                            let color = COLORS[c % COLORS.len()];
                            changed |= ui
                                .checkbox(
                                    &mut self.shown[c],
                                    egui::RichText::new(format!("D{c}")).color(color),
                                )
                                .changed();
                            if c % 8 == 7 {
                                ui.end_row();
                            }
                        }
                    });
                    if changed {
                        self.persist_channels();
                    }
                });
                if busy {
                    ui.colored_label(Color32::LIGHT_YELLOW, "수집 중에는 바꿀 수 없습니다");
                }
                if found.is_u3() && ui.button("펌웨어 업데이트…").clicked() {
                    self.fw.open = true;
                }
                if let Some(m) = &self.device_msg {
                    ui.label(m);
                }
            });
        if channels_changed {
            self.fit_rate();
        }
        if let Some(v) = led {
            self.set_led(v);
        }
        self.show_device = open;
    }
}

/// The receiving end for [`DevEvent`]s, made with the app.
pub fn channel() -> (Sender<DevEvent>, Receiver<DevEvent>) {
    mpsc::channel()
}
