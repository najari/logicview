//! logicview: capture from an fx2lafw analyzer, open `.lvcap`/`.sr`, browse
//! the waveform, decode, and serve MCP.
//!
//! ```text
//! logicview [file.lvcap]          open a capture
//! logicview --mcp-stdio           MCP server on stdin/stdout, no window (AUT-5)
//! logicview --mcp-http [--host H] [--port 10530]
//!                                 MCP server over HTTP, no window
//!   [--data-dir D] [--sigrok-dir D | --sigrok-cli D\sigrok-cli.exe] [--hla-script P]
//! logicview --bench file.lvcap    scripted zoom/pan, print frame times, exit
//! logicview --view 1000,0.5 f     start at sample 1000, 0.5 samples per point
//! logicview --rate 1M --time 0.5 --trigger 0=f
//!                                 preset the capture toolbar (--time <seconds | manual>;
//!                                 --samples <count | manual> sets the length in samples)
//! ```

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod acquire;
mod app;
mod bench;
mod capture_view;
mod decode_ui;
mod settings;
mod wave;

use std::path::PathBuf;

fn main() -> eframe::Result {
    prefer_dx12();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut path = None;
    let mut bench = false;
    let mut view = None;
    let mut preset = app::Preset::default();
    let mut mcp: Option<&'static str> = None;
    let mut host = "127.0.0.1".to_string();
    let mut port = 10530u16;
    let mut data_dir = None;
    let mut hla_script = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().map(|v| v.to_string_lossy().to_string());
        if a == "--bench" {
            bench = true;
        } else if a == "--mcp-stdio" {
            mcp = Some("stdio");
        } else if a == "--mcp-http" {
            mcp = Some("http");
        } else if a == "--host" {
            host = value().unwrap_or(host);
        } else if a == "--port" {
            port = value().and_then(|v| v.parse().ok()).unwrap_or(port);
        } else if a == "--data-dir" {
            data_dir = value().map(PathBuf::from);
        } else if a == "--hla-script" {
            hla_script = value().map(PathBuf::from);
        } else if a == "--sigrok-dir" || a == "--sigrok-cli" {
            // The decoder runtime folder (sigrok-cli.exe sits in it).
            if let Some(v) = value() {
                let p = PathBuf::from(v);
                let dir = if p.is_file() {
                    p.parent().map(PathBuf::from).unwrap_or(p)
                } else {
                    p
                };
                // SAFETY: single-threaded here, before anything reads it.
                unsafe { std::env::set_var("LOGICVIEW_SIGROK_DIR", dir) };
            }
        } else if a == "--rate" {
            preset.rate = value().and_then(|v| lv_store::sigrok::parse_rate(&v));
        } else if a == "--time" {
            // Capture length in seconds ("0.5", "2s") or "manual".
            preset.duration = value().and_then(|v| {
                if v.eq_ignore_ascii_case("manual") {
                    return Some(None);
                }
                let s: f64 = v.trim_end_matches(['s', 'S']).trim().parse().ok()?;
                (s.is_finite() && s > 0.0).then_some(Some(s))
            });
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
    let mut mcp_cfg = lv_mcp::Config::new(data_dir.unwrap_or_else(lv_mcp::default_data_dir));
    if hla_script.is_some() {
        mcp_cfg.hla_script = hla_script;
    }
    // HLAs installed through MCP must be on the decoder search path before
    // the decoder runtime starts.
    lv_mcp::prepare(&mcp_cfg);
    if let Some(mode) = mcp {
        return run_mcp(mode, mcp_cfg, &host, port);
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
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(
                cc,
                path,
                bench,
                view,
                preset,
                (mcp_cfg, host, port),
            )))
        }),
    )
}

/// Headless MCP server.
fn run_mcp(mode: &str, cfg: lv_mcp::Config, host: &str, port: u16) -> eframe::Result {
    let server = match lv_mcp::Server::new(cfg) {
        Ok(s) => std::sync::Arc::new(s),
        Err(e) => {
            log::error!("MCP data folder: {e}");
            std::process::exit(1);
        }
    };
    if mode == "stdio" {
        lv_mcp::serve_stdio(server);
        return Ok(());
    }
    match lv_mcp::start_http(server, host, port) {
        Ok(_handle) => loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        },
        Err(e) => {
            log::error!("MCP HTTP server on {host}:{port}: {e}");
            std::process::exit(1);
        }
    }
}

/// Draw with Direct3D 12 on Windows unless `WGPU_BACKEND` says otherwise.
/// wgpu would pick Vulkan first, and on an Intel Iris Xe the Vulkan window
/// stayed white (nothing presented) while DX12 drew normally. DX12 is always
/// there on Windows 10+ (with a software renderer when there is no GPU).
#[cfg(windows)]
fn prefer_dx12() {
    if std::env::var_os("WGPU_BACKEND").is_none() {
        // SAFETY: single-threaded here, before the window and wgpu start.
        unsafe { std::env::set_var("WGPU_BACKEND", "dx12") };
    }
}

#[cfg(not(windows))]
fn prefer_dx12() {}

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
