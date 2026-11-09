//! logicview viewer (M0): open a `.lvcap` capture and browse the waveform.
//!
//! ```text
//! logicview [file.lvcap]          open a capture
//! logicview --bench file.lvcap    scripted zoom/pan, print frame times, exit
//! logicview --view 1000,0.5 f     start at sample 1000, 0.5 samples per point
//! ```

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bench;
mod capture_view;
mod wave;

use std::path::PathBuf;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut path = None;
    let mut bench = false;
    let mut view = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        if a == "--bench" {
            bench = true;
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
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, path, bench, view)))),
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
