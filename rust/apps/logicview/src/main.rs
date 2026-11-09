//! logicview (M2): capture from an fx2lafw analyzer, open `.lvcap`/`.sr`, browse the waveform.
//!
//! ```text
//! logicview [file.lvcap]          open a capture
//! logicview --bench file.lvcap    scripted zoom/pan, print frame times, exit
//! logicview --view 1000,0.5 f     start at sample 1000, 0.5 samples per point
//! logicview --rate 1M --samples manual --trigger 0=f
//!                                 preset the capture toolbar
//! ```

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod acquire;
mod app;
mod bench;
mod capture_view;
mod decode_ui;
mod wave;

use std::path::PathBuf;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut path = None;
    let mut bench = false;
    let mut view = None;
    let mut preset = app::Preset::default();
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().map(|v| v.to_string_lossy().to_string());
        if a == "--bench" {
            bench = true;
        } else if a == "--rate" {
            preset.rate = value().and_then(|v| lv_store::sigrok::parse_rate(&v));
        } else if a == "--samples" {
            preset.samples = value().map(|v| {
                if v.eq_ignore_ascii_case("manual") {
                    None
                } else {
                    lv_store::sigrok::parse_rate(&v)
                }
            });
        } else if a == "--trigger" {
            preset.trigger = value();
        } else if a == "--markers" {
            // --markers <A>,<B>: marker positions in samples (tests, screenshots).
            preset.markers = value()
                .map(|v| v.split(',').filter_map(|m| m.trim().parse().ok()).collect())
                .unwrap_or_default();
        } else if a == "--view" {
            // --view <first sample>,<samples per point>: start zoomed (tests).
            view = args.next().and_then(|v| {
                let v = v.to_string_lossy().to_string();
                let (s, z) = v.split_once(',')?;
                Some(wave::View {
                    start: s.trim().parse().ok()?,
                    spp: z.trim().parse().ok()?,
                })
            });
        } else {
            path = Some(PathBuf::from(a));
        }
    }
    if bench {
        attach_console();
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("logicview")
            .with_inner_size([1400.0, 820.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "logicview",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, path, bench, view, preset)))),
    )
}

/// The release build has no console; `--bench` prints, so borrow the
/// console of the shell that started us.
#[cfg(windows)]
fn attach_console() {
    unsafe extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    unsafe {
        AttachConsole(u32::MAX);
    }
}

#[cfg(not(windows))]
fn attach_console() {}
