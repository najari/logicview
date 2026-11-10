//! Server state and the tools (AUT-3, AUT-4). Behaviour, error codes and
//! messages follow `mcp/logicview_mcp.py`, with captures, decoding and
//! storage done by the logicview crates instead of sigrok-cli.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lv_capture::{CaptureRequest, Outcome, Trigger};
use lv_decode::{Control, StackEntry};
use lv_fx2lafw::{FoundDevice, Fx2lafw, StreamEnd};
use lv_store::{CaptureFile, CaptureWriter, NewCapture, Status};
use serde_json::{Value, json};

/// An opened device, shared with the app's own capturing.
pub type DeviceSlot = Arc<Mutex<Option<Fx2lafw>>>;

#[derive(Debug, Clone)]
pub struct McpError {
    pub code: &'static str,
    pub message: String,
}

impl McpError {
    fn new(code: &'static str, message: impl Into<String>) -> McpError {
        McpError {
            code,
            message: message.into(),
        }
    }
}

type R<T> = Result<T, McpError>;

fn need(ok: bool, code: &'static str, msg: impl FnOnce() -> String) -> R<()> {
    if ok {
        Ok(())
    } else {
        Err(McpError::new(code, msg()))
    }
}

fn invalid(msg: impl Into<String>) -> McpError {
    McpError::new("INVALID_REQUEST", msg)
}

fn internal(e: impl std::fmt::Display) -> McpError {
    McpError::new("INTERNAL_ERROR", e.to_string())
}

#[derive(Clone)]
pub struct Config {
    /// Captures and installed HLAs live here.
    pub data_dir: PathBuf,
    /// `extensions/hla.ps1` (installs HLAs as sigrok decoders).
    pub hla_script: Option<PathBuf>,
    pub device_slot: DeviceSlot,
}

impl Config {
    pub fn new(data_dir: PathBuf) -> Config {
        Config {
            data_dir,
            hla_script: find_hla_script(),
            device_slot: Arc::new(Mutex::new(None)),
        }
    }

    pub fn hla_root(&self) -> PathBuf {
        self.data_dir.join("hla")
    }
}

/// `extensions\hla.ps1` next to the executable or in one of its parents
/// (the repository when running from `rust\target\release`).
pub fn find_hla_script() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .map(|d| d.join("extensions").join("hla.ps1"))
        .find(|p| p.is_file())
}

/// Default data folder: `%LOCALAPPDATA%\logicview\mcp`.
pub fn default_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("logicview")
        .join("mcp")
}

/// Call before the decoder runtime starts, so HLAs installed later are found.
pub fn prepare(cfg: &Config) {
    lv_decode::add_decoder_dir(&cfg.hla_root().join("decoders"));
}

// ---- Logic 2 analyzers --------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum SVal {
    Int(i64),
    Float(f64),
    Str(&'static str),
}

impl SVal {
    /// Python `str()`.
    fn py_str(self) -> String {
        match self {
            SVal::Int(i) => i.to_string(),
            SVal::Float(f) if f.fract() == 0.0 => format!("{f:.1}"),
            SVal::Float(f) => f.to_string(),
            SVal::Str(s) => s.to_string(),
        }
    }

    fn spec(self) -> String {
        match self {
            SVal::Int(i) => i.to_string(),
            SVal::Float(f) => g(f),
            SVal::Str(s) => s.to_string(),
        }
    }

    fn eq_num(self, x: f64) -> bool {
        match self {
            SVal::Int(i) => i as f64 == x,
            SVal::Float(f) => f == x,
            SVal::Str(_) => false,
        }
    }
}

/// Python `'{:g}'` for the values that occur here.
fn g(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        f.to_string()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Channel,
    Integer,
    Choices,
}

struct SettingDef {
    title: &'static str,
    kind: Kind,
    target: &'static str,
    default: SVal,
    choices: Vec<(String, SVal)>,
    required: bool,
}

fn chan(title: &'static str, target: &'static str, required: bool) -> SettingDef {
    SettingDef {
        title,
        kind: Kind::Channel,
        target,
        default: SVal::Int(0),
        choices: Vec::new(),
        required,
    }
}

fn integer(title: &'static str, target: &'static str, default: i64) -> SettingDef {
    SettingDef {
        title,
        kind: Kind::Integer,
        target,
        default: SVal::Int(default),
        choices: Vec::new(),
        required: false,
    }
}

fn choices(
    title: &'static str,
    target: &'static str,
    default: SVal,
    list: &[(&str, SVal)],
) -> SettingDef {
    SettingDef {
        title,
        kind: Kind::Choices,
        target,
        default,
        choices: list.iter().map(|(t, v)| (t.to_string(), *v)).collect(),
        required: false,
    }
}

fn n_bits(lo: i64, hi: i64) -> Vec<(String, SVal)> {
    (lo..=hi)
        .map(|n| {
            (
                format!(
                    "{n} Bits per Transfer{}",
                    if n == 8 { " (Standard)" } else { "" }
                ),
                SVal::Int(n),
            )
        })
        .collect()
}

struct Logic2 {
    decoder: &'static str,
    classes: &'static [&'static str],
    settings: Vec<SettingDef>,
}

const LOGIC2_NAMES: [&str; 4] = ["Async Serial", "I2C", "SPI", "CAN"];

fn logic2(name: &str) -> Option<Logic2> {
    use SVal::*;
    Some(match name {
        "Async Serial" => Logic2 {
            decoder: "uart",
            classes: &[
                "rx-data",
                "tx-data",
                "rx-warning",
                "tx-warning",
                "rx-break",
                "tx-break",
            ],
            settings: vec![
                chan("Input Channel", "rx", true),
                integer("Bit Rate (Bits/s)", "baudrate", 9600),
                SettingDef {
                    choices: n_bits(5, 9),
                    ..choices("Bits per Frame", "data_bits", Int(8), &[])
                },
                choices(
                    "Stop Bits",
                    "stop_bits",
                    Float(1.0),
                    &[
                        ("1 Stop Bit (Standard)", Float(1.0)),
                        ("1.5 Stop Bits", Float(1.5)),
                        ("2 Stop Bits", Float(2.0)),
                    ],
                ),
                choices(
                    "Parity Bit",
                    "parity",
                    Str("none"),
                    &[
                        ("No Parity Bit (Standard)", Str("none")),
                        ("Even Parity Bit", Str("even")),
                        ("Odd Parity Bit", Str("odd")),
                    ],
                ),
                choices(
                    "Significant Bit",
                    "bit_order",
                    Str("lsb-first"),
                    &[
                        (
                            "Least Significant Bit Sent First (Standard)",
                            Str("lsb-first"),
                        ),
                        ("Most Significant Bit Sent First", Str("msb-first")),
                    ],
                ),
                choices(
                    "Signal inversion",
                    "invert_rx",
                    Str("no"),
                    &[
                        ("Non Inverted (Standard)", Str("no")),
                        ("Inverted", Str("yes")),
                    ],
                ),
            ],
        },
        "I2C" => Logic2 {
            decoder: "i2c",
            classes: &[
                "start",
                "repeat-start",
                "stop",
                "address-read",
                "address-write",
                "data-read",
                "data-write",
                "warning",
            ],
            settings: vec![chan("SDA", "sda", true), chan("SCL", "scl", true)],
        },
        "SPI" => Logic2 {
            decoder: "spi",
            classes: &["mosi-data", "miso-data", "warning"],
            settings: vec![
                chan("MOSI", "mosi", false),
                chan("MISO", "miso", false),
                chan("Clock", "clk", true),
                chan("Enable", "cs", false),
                choices(
                    "Significant Bit",
                    "bitorder",
                    Str("msb-first"),
                    &[
                        ("Most Significant Bit First (Standard)", Str("msb-first")),
                        ("Least Significant Bit First", Str("lsb-first")),
                    ],
                ),
                SettingDef {
                    choices: n_bits(4, 32),
                    ..choices("Bits per Transfer", "wordsize", Int(8), &[])
                },
                choices(
                    "Clock State",
                    "cpol",
                    Int(0),
                    &[
                        ("Clock is Low when inactive (CPOL = 0)", Int(0)),
                        ("Clock is High when inactive (CPOL = 1)", Int(1)),
                    ],
                ),
                choices(
                    "Clock Phase",
                    "cpha",
                    Int(0),
                    &[
                        ("Data is Valid on Clock Leading Edge (CPHA = 0)", Int(0)),
                        ("Data is Valid on Clock Trailing Edge (CPHA = 1)", Int(1)),
                    ],
                ),
                choices(
                    "Enable Line",
                    "cs_polarity",
                    Str("active-low"),
                    &[
                        ("Enable line is Active Low (Standard)", Str("active-low")),
                        ("Enable line is Active High", Str("active-high")),
                    ],
                ),
            ],
        },
        "CAN" => Logic2 {
            decoder: "can",
            classes: &[
                "sof",
                "id",
                "ext-id",
                "full-id",
                "rtr",
                "dlc",
                "data",
                "crc-sequence",
                "ack-slot",
                "eof",
                "warning",
            ],
            settings: vec![
                chan("CAN", "can_rx", true),
                integer("Bit Rate (Bits/s)", "nominal_bitrate", 1_000_000),
            ],
        },
        _ => return None,
    })
}

fn describe(defs: &[SettingDef], enabled: &[usize]) -> String {
    let mut s = String::from("Settings:\n");
    let lines: Vec<String> = defs
        .iter()
        .map(|d| match d.kind {
            Kind::Channel => format!(
                "  \"{}\" (channel, {}, enabled channels: {})",
                d.title,
                if d.required { "required" } else { "optional" },
                enabled
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Kind::Choices => format!(
                "  \"{}\" (choices: {})",
                d.title,
                d.choices
                    .iter()
                    .map(|(t, v)| format!(
                        "\"{t}\"{}",
                        if *v == d.default { " [default]" } else { "" }
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Kind::Integer => format!("  \"{}\" (integer, default: {})", d.title, d.default.spec()),
        })
        .collect();
    s.push_str(&lines.join("\n"));
    s
}

// ---- state ----------------------------------------------------------------------------

const HLA_INPUTS: [&str; 4] = ["uart", "i2c", "spi", "can"];

fn radix_format(r: i64) -> Option<&'static str> {
    match r {
        1 => Some("bin"),
        2 => Some("dec"),
        3 => Some("hex"),
        4 => Some("ascii"),
        _ => None,
    }
}

/// (start, end, analyzer label, type, value) of a data table row.
type TableRow = (u64, u64, String, String, String);

#[derive(Clone)]
struct FrameRow {
    ss: u64,
    es: u64,
    class: String,
    text: String,
}

#[derive(Clone)]
struct Analyzer {
    kind: &'static str, // "lla" | "hla"
    name: String,
    label: String,
    stack: Vec<StackEntry>,
    decoder: String,
    classes: Option<Vec<String>>,
    frames: Vec<FrameRow>,
    input_id: Option<u64>,
    db_id: Option<i64>,
}

#[derive(Default)]
struct Done {
    done: bool,
    error: Option<McpError>,
}

struct Capture {
    id: u64,
    /// The .lvcap this server works on (always its own copy).
    path: PathBuf,
    /// A loaded .sr file; saving to .sr copies it byte for byte.
    source_sr: Option<PathBuf>,
    /// Unix seconds of the first sample (ISO timestamps).
    start_wall: f64,
    state: Mutex<Done>,
    cv: Condvar,
    stop: AtomicBool,
    analyzers: Mutex<BTreeMap<u64, Analyzer>>,
}

impl Capture {
    fn finish(&self, error: Option<McpError>) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.done = true;
        s.error = error;
        self.cv.notify_all();
    }

    fn is_done(&self) -> bool {
        self.state.lock().map(|s| s.done).unwrap_or(true)
    }

    /// Wait for the end; `Err` if the capture failed.
    fn wait(&self, timeout: Option<Duration>) -> R<bool> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = timeout.map(|t| std::time::Instant::now() + t);
        while !s.done {
            match deadline {
                None => s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner()),
                Some(d) => {
                    let now = std::time::Instant::now();
                    if now >= d {
                        return Ok(false);
                    }
                    s = self
                        .cv
                        .wait_timeout(s, d - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
        match &s.error {
            Some(e) => Err(e.clone()),
            None => Ok(true),
        }
    }

    fn file(&self) -> R<CaptureFile> {
        CaptureFile::open(&self.path).map_err(internal)
    }
}

pub struct Server {
    cfg: Config,
    captures: Mutex<BTreeMap<u64, Arc<Capture>>>,
    next_capture: AtomicU64,
    next_analyzer: AtomicU64,
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Python `datetime.isoformat()` in UTC, `+00:00` written as `Z`.
fn iso(unix: f64) -> String {
    let micros_total = (unix * 1e6).round() as i64;
    let secs = micros_total.div_euclid(1_000_000);
    let micros = micros_total.rem_euclid(1_000_000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let frac = if micros > 0 {
        format!(".{micros:06}")
    } else {
        String::new()
    };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}{frac}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn csv_line(fields: &[String]) -> String {
    let mut l = fields
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",");
    l.push_str("\r\n");
    l
}

fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn arg_u64(args: &Value, key: &str) -> u64 {
    args.get(key)
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .max(0.0) as u64
}

fn arg_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

impl Server {
    pub fn new(cfg: Config) -> std::io::Result<Server> {
        std::fs::create_dir_all(cfg.data_dir.join("captures"))?;
        prepare(&cfg);
        Ok(Server {
            cfg,
            captures: Mutex::new(BTreeMap::new()),
            next_capture: AtomicU64::new(1),
            next_analyzer: AtomicU64::new(1),
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Run a tool with validated arguments.
    pub fn call(&self, name: &str, args: &Value) -> R<Value> {
        match name {
            "get_devices" => self.get_devices(args),
            "start_capture" => self.start_capture(args),
            "stop_capture" => self.stop_capture(args),
            "wait_capture" => self.wait_capture(args),
            "load_capture" => self.load_capture(args),
            "save_capture" => self.save_capture(args),
            "close_capture" => self.close_capture(args),
            "add_analyzer" => self.add_analyzer(args),
            "remove_analyzer" => self.remove_analyzer(args),
            "add_high_level_analyzer" => self.add_high_level_analyzer(args),
            "remove_high_level_analyzer" => self.remove_high_level_analyzer(args),
            "export_raw_data_csv" => self.export_raw_data_csv(args),
            "export_raw_data_binary" => self.export_raw_data_binary(args),
            "export_data_table_csv" => self.export_data_table_csv(args),
            "legacy_export_analyzer" => self.legacy_export_analyzer(args),
            "read_analyzer_frames" => self.read_analyzer_frames(args),
            "query_frames" => self.query_frames(args),
            _ => Err(internal(format!("no tool {name}"))),
        }
    }

    fn capture(&self, cid: u64) -> R<Arc<Capture>> {
        self.captures
            .lock()
            .map_err(internal)?
            .get(&cid)
            .cloned()
            .ok_or_else(|| invalid(format!("Capture Id \"{cid}\" does not exist.")))
    }

    fn finished_capture(&self, args: &Value) -> R<Arc<Capture>> {
        let cid = arg_u64(args, "captureId");
        let cap = self.capture(cid)?;
        need(cap.is_done(), "INVALID_REQUEST", || {
            format!("Capture {cid} is still running. Call stop_capture or wait_capture first.")
        })?;
        cap.wait(None)?;
        Ok(cap)
    }

    fn new_capture_id(&self) -> u64 {
        self.next_capture.fetch_add(1, Ordering::SeqCst)
    }

    fn register(&self, cap: Capture) -> R<Arc<Capture>> {
        let cap = Arc::new(cap);
        self.captures
            .lock()
            .map_err(internal)?
            .insert(cap.id, cap.clone());
        Ok(cap)
    }

    // ---- devices and captures ---------------------------------------------------------

    fn devices(&self) -> R<Vec<FoundDevice>> {
        lv_fx2lafw::list_devices().map_err(|e| McpError::new("DEVICE_ERROR", e.to_string()))
    }

    fn device_id(d: &FoundDevice) -> String {
        format!("fx2lafw:{}", d.location)
    }

    fn get_devices(&self, _args: &Value) -> R<Value> {
        let list: Vec<Value> = self
            .devices()?
            .iter()
            .map(|d| {
                json!({"deviceId": Self::device_id(d), "deviceType": d.name(), "isSimulation": false})
            })
            .collect();
        Ok(json!({"devices": list}))
    }

    fn start_capture(&self, args: &Value) -> R<Value> {
        let dev_cfg = args
            .get("logicDeviceConfiguration")
            .filter(|v| !v.is_null());
        let dev_cfg =
            dev_cfg.ok_or_else(|| invalid("logicDeviceConfiguration must be provided."))?;
        let devices = self.devices()?;
        let device_id = arg_str(args, "deviceId").unwrap_or("");
        let found = if !device_id.is_empty() {
            devices
                .iter()
                .find(|d| Self::device_id(d) == device_id)
                .cloned()
                .ok_or_else(|| {
                    McpError::new(
                        "MISSING_DEVICE",
                        format!(
                            "No device with the given id is connected. Connected devices: {}.",
                            if devices.is_empty() {
                                "none".to_string()
                            } else {
                                devices
                                    .iter()
                                    .map(Self::device_id)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            }
                        ),
                    )
                })?
        } else {
            devices.first().cloned().ok_or_else(|| {
                McpError::new(
                    "MISSING_DEVICE",
                    "No physical devices found. Is the analyzer plugged in, and not opened by another program?",
                )
            })?
        };

        let channels = dev_cfg.get("logicChannels").cloned().unwrap_or(json!({}));
        let digital: Vec<i64> = channels
            .get("digitalChannels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_f64)
                    .map(|x| x as i64)
                    .collect()
            })
            .unwrap_or_default();
        need(
            !truthy(channels.get("analogChannels")),
            "INVALID_REQUEST",
            || {
                "Requested analog channels are unavailable: this device has digital channels only."
                    .into()
            },
        )?;
        need(!digital.is_empty(), "INVALID_REQUEST", || {
            "At least 1 digital channel must be enabled".into()
        })?;
        let bad: Vec<String> = digital
            .iter()
            .filter(|&&c| !(0..8).contains(&c))
            .map(|c| c.to_string())
            .collect();
        need(bad.is_empty(), "INVALID_REQUEST", || {
            format!(
                "Requested digital channels are unavailable: {}. Device channels: 0-7.",
                bad.join(", ")
            )
        })?;
        need(
            !truthy(dev_cfg.get("digitalThresholdVolts")),
            "INVALID_REQUEST",
            || {
                format!(
                    "Digital threshold \"{}\" requested, but the requested device does not support a configurable threshold voltage.",
                    dev_cfg["digitalThresholdVolts"]
                )
            },
        )?;
        need(
            !truthy(dev_cfg.get("glitchFilters")),
            "INVALID_REQUEST",
            || "Glitch filters are not supported by this device.".into(),
        )?;
        let rate = arg_f64(dev_cfg, "digitalSampleRate").unwrap_or(0.0);
        need(rate > 0.0, "INVALID_REQUEST", || {
            "digitalSampleRate must be provided (samples/sec).".into()
        })?;
        let rate_u = rate.round() as u64;
        need(
            lv_fx2lafw::SAMPLERATES.contains(&rate_u) && rate.fract() == 0.0,
            "INVALID_REQUEST",
            || {
                format!(
                    "Invalid supplied sample rate. Allowed sample rates: {}",
                    lv_fx2lafw::SAMPLERATES
                        .iter()
                        .map(|r| r.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        )?;

        let cfg = args
            .get("captureConfiguration")
            .cloned()
            .unwrap_or(json!({}));
        let mut limit = None;
        let mut trim = 0.0;
        let mut trigger = None;
        if let Some(timed) = cfg.get("timedCaptureMode").filter(|v| truthy(Some(v))) {
            let seconds = arg_f64(timed, "durationSeconds")
                .filter(|s| *s != 0.0)
                .unwrap_or(1.0);
            need(seconds > 0.0, "INVALID_REQUEST", || {
                "durationSeconds must be greater than 0".into()
            })?;
            limit = Some(((seconds * rate).round() as u64).max(1));
            trim = arg_f64(timed, "trimDataSeconds").unwrap_or(0.0);
        } else if let Some(trig) = cfg.get("digitalCaptureMode").filter(|v| truthy(Some(v))) {
            let ch = arg_f64(trig, "triggerChannelIndex").unwrap_or(0.0) as i64;
            need(digital.contains(&ch), "INVALID_REQUEST", || {
                format!("Digital channel \"{ch}\" must be enabled to be used as a digital trigger")
            })?;
            let kind = arg_f64(trig, "triggerType").unwrap_or(0.0) as i64;
            need((1..=4).contains(&kind), "INVALID_REQUEST", || {
                "Unknown digital trigger type".into()
            })?;
            need(kind <= 2, "INVALID_REQUEST", || {
                "Pulse triggers (3, 4) are not supported by this device; use 1=rising or 2=falling."
                    .into()
            })?;
            let mut spec = vec![format!("{ch}={}", if kind == 1 { "r" } else { "f" })];
            for linked in trig
                .get("linkedChannels")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let lc = arg_f64(&linked, "channelIndex").unwrap_or(-1.0) as i64;
                let state = arg_f64(&linked, "state").unwrap_or(0.0) as i64;
                need(digital.contains(&lc), "INVALID_REQUEST", || {
                    format!(
                        "Digital channel \"{lc}\" must be enabled to be used as a linked channel in the digital trigger"
                    )
                })?;
                need(state == 1 || state == 2, "INVALID_REQUEST", || {
                    "Unknown digital trigger linked channel state".into()
                })?;
                spec.push(format!("{lc}={}", if state == 1 { "0" } else { "1" }));
            }
            let after = arg_f64(trig, "afterTriggerSeconds")
                .filter(|s| *s != 0.0)
                .unwrap_or(0.1);
            // 10 % of the samples come from before the trigger.
            limit = Some(((after * rate / 0.9).ceil() as u64).max(10));
            trigger = Some(Trigger::parse(&spec.join(","), 10).map_err(invalid)?);
            trim = arg_f64(trig, "trimDataSeconds").unwrap_or(0.0);
        } else if let Some(m) = cfg.get("manualCaptureMode") {
            trim = arg_f64(m, "trimDataSeconds").unwrap_or(0.0);
        }

        let cid = self.new_capture_id();
        let path = self
            .cfg
            .data_dir
            .join("captures")
            .join(format!("capture-{cid}.lvcap"));
        remove_db(&path);
        let mut req = CaptureRequest::new(rate_u, limit, path.clone());
        req.overwrite = true;
        req.trigger = trigger;
        let cap = self.register(Capture {
            id: cid,
            path,
            source_sr: None,
            start_wall: unix_now(),
            state: Mutex::new(Done::default()),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            analyzers: Mutex::new(BTreeMap::new()),
        })?;
        let slot = self.cfg.device_slot.clone();
        let enabled: Vec<usize> = digital.iter().map(|&c| c as usize).collect();
        log::info!("mcp capture {cid}: {} Hz, limit {limit:?}", rate_u);
        std::thread::Builder::new()
            .name(format!("mcp-capture-{cid}"))
            .spawn(move || {
                let r = run_capture(&cap, found, req, &slot, &enabled, trim);
                cap.finish(r.err());
            })
            .map_err(internal)?;
        Ok(json!({"captureId": cid}))
    }

    fn stop_capture(&self, args: &Value) -> R<Value> {
        let cap = self.capture(arg_u64(args, "captureId"))?;
        if !cap.is_done() {
            cap.stop.store(true, Ordering::Relaxed);
        }
        cap.wait(None)?;
        Ok(json!({}))
    }

    fn wait_capture(&self, args: &Value) -> R<Value> {
        let cap = self.capture(arg_u64(args, "captureId"))?;
        cap.wait(None)?;
        Ok(json!({}))
    }

    fn load_capture(&self, args: &Value) -> R<Value> {
        let path = arg_str(args, "filepath").unwrap_or("");
        need(!path.is_empty(), "INVALID_REQUEST", || {
            "Filepath must not be empty".into()
        })?;
        let lower = path.to_lowercase();
        need(!lower.ends_with(".sal"), "LOAD_CAPTURE_FAILED", || {
            "Saleae .sal files cannot be read; logicview uses sigrok session files (.sr) and .lvcap files.".into()
        })?;
        let src = Path::new(path);
        need(src.is_file(), "LOAD_CAPTURE_FAILED", || {
            format!("File not found: {path}")
        })?;
        let cid = self.new_capture_id();
        let work = self
            .cfg
            .data_dir
            .join("captures")
            .join(format!("loaded-{cid}.lvcap"));
        remove_db(&work);
        let is_lvcap = lower.ends_with(".lvcap");
        if is_lvcap {
            CaptureFile::open(src)
                .and_then(|f| f.save_copy(&work))
                .map_err(|e| {
                    McpError::new(
                        "LOAD_CAPTURE_FAILED",
                        format!("Not a logicview capture ({e}): {path}"),
                    )
                })?;
        } else {
            lv_store::sigrok::import(src, &work, true).map_err(|e| {
                McpError::new(
                    "LOAD_CAPTURE_FAILED",
                    format!("Not a sigrok session file ({e}): {path}"),
                )
            })?;
        }
        let mtime = std::fs::metadata(src)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or_else(unix_now);
        let cap = Capture {
            id: cid,
            path: work,
            source_sr: (!is_lvcap).then(|| std::path::absolute(src).unwrap_or(src.to_path_buf())),
            start_wall: mtime,
            state: Mutex::new(Done::default()),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            analyzers: Mutex::new(BTreeMap::new()),
        };
        cap.finish(None);
        self.register(cap)?;
        Ok(json!({"captureId": cid}))
    }

    fn save_capture(&self, args: &Value) -> R<Value> {
        let cap = self.finished_capture(args)?;
        let path = arg_str(args, "filepath").unwrap_or("");
        need(!path.is_empty(), "INVALID_REQUEST", || {
            "Filepath must not be empty".into()
        })?;
        let lower = path.to_lowercase();
        need(!lower.ends_with(".sal"), "INVALID_REQUEST", || {
            "Captures are saved as sigrok session files; use a .sr file name (or .lvcap).".into()
        })?;
        let dest = std::path::absolute(path).unwrap_or(PathBuf::from(path));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(internal)?;
        }
        if lower.ends_with(".lvcap") {
            remove_db(&dest);
            cap.file()?.save_copy(&dest).map_err(internal)?;
        } else if let Some(src) = &cap.source_sr {
            if *src != dest {
                std::fs::copy(src, &dest).map_err(internal)?;
            }
        } else {
            lv_store::sigrok::export(&cap.file()?, &dest).map_err(internal)?;
        }
        Ok(json!({}))
    }

    fn close_capture(&self, args: &Value) -> R<Value> {
        let cap = self.capture(arg_u64(args, "captureId"))?;
        if !cap.is_done() {
            cap.stop.store(true, Ordering::Relaxed);
            let _ = cap.wait(Some(Duration::from_secs(10)));
        }
        self.captures.lock().map_err(internal)?.remove(&cap.id);
        remove_db(&cap.path);
        Ok(json!({}))
    }

    // ---- analyzers ------------------------------------------------------------------------

    /// Decode `stack` on the capture: top-decoder frames (sorted), and the
    /// frames of every decoder for the file.
    fn decode(
        &self,
        cap: &Capture,
        stack: &[StackEntry],
    ) -> R<(Vec<FrameRow>, lv_decode::Decoded)> {
        let rt = lv_decode::runtime().map_err(internal)?;
        let file = cap.file()?;
        let d = rt
            .decode(&file, stack, &Control::default())
            .map_err(|e| invalid(format!("Decoding failed: {e}")))?;
        let top = d.stack.len() - 1;
        let mut frames: Vec<FrameRow> = d
            .annotations
            .iter()
            .filter(|a| a.level == top)
            .map(|a| FrameRow {
                ss: a.start,
                es: a.end,
                class: d.stack[top].class_id(a.class).to_string(),
                text: a.texts.first().cloned().unwrap_or_default(),
            })
            .collect();
        frames.sort_by_key(|f| (f.ss, f.es));
        Ok((frames, d))
    }

    fn store(
        &self,
        cap: &Capture,
        an: &Analyzer,
        d: &lv_decode::Decoded,
        input_db: Option<i64>,
    ) -> Option<i64> {
        let (settings, frames) = d.to_frames(&an.stack);
        let r = lv_store::analysis::add_analyzer(
            &cap.path,
            &lv_store::analysis::NewAnalyzer {
                kind: if an.kind == "hla" { "hla" } else { "sigrok" }.into(),
                decoder_id: settings.stack.clone(),
                label: an.label.clone(),
                settings_json: settings.to_json(),
                input_id: input_db,
            },
            &frames,
        );
        match r {
            Ok(id) => Some(id),
            Err(e) => {
                log::warn!("storing analyzer frames: {e}");
                None
            }
        }
    }

    fn spec_value(key: &str, v: &Value) -> R<String> {
        let text = match v {
            Value::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
            Value::Number(n) => match n.as_i64() {
                Some(i) => i.to_string(),
                None => g(n.as_f64().unwrap_or(0.0)),
            },
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        need(!text.contains([':', ',', '=']), "INVALID_REQUEST", || {
            format!("Setting \"{key}\": values cannot contain \":\", \",\" or \"=\" ('{text}').")
        })?;
        Ok(text)
    }

    fn add_analyzer(&self, args: &Value) -> R<Value> {
        let cap = self.finished_capture(args)?;
        let file = cap.file()?;
        let enabled: Vec<usize> = (0..file.info.num_channels)
            .filter(|&c| file.info.channel_enabled.get(c).copied().unwrap_or(true))
            .collect();
        drop(file);
        let name = arg_str(args, "analyzerName").unwrap_or("").to_string();
        let settings = args
            .get("settings")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let channel = |key: &str, v: Option<&Value>| -> R<String> {
            let ch = v.and_then(Value::as_f64).ok_or_else(|| {
                invalid(format!(
                    "Invalid value type for analyzer setting \"{key}\", expected number"
                ))
            })? as i64;
            need(
                ch >= 0 && enabled.contains(&(ch as usize)),
                "INVALID_REQUEST",
                || {
                    format!(
                        "Invalid value type for analyzer setting \"{key}\", channel \"{ch}\" is not enabled"
                    )
                },
            )?;
            Ok(ch.to_string())
        };

        let (decoder, args_list, classes): (String, Vec<(String, String)>, Option<Vec<String>>) =
            if let Some(spec) = logic2(&name) {
                let defs = &spec.settings;
                let mut values: Vec<(&str, SVal)> = defs
                    .iter()
                    .filter(|d| d.kind != Kind::Channel)
                    .map(|d| (d.target, d.default))
                    .collect();
                let mut chans: Vec<(&str, String)> = Vec::new();
                let desc = describe(defs, &enabled);
                for (key, entry) in &settings {
                    let d = defs.iter().find(|d| d.title == key).ok_or_else(|| {
                        invalid(format!(
                            "Invalid setting name: {key}, available settings: {}\n{desc}",
                            defs.iter()
                                .map(|x| format!("\"{}\"", x.title))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })?;
                    match d.kind {
                        Kind::Channel => {
                            let v =
                                channel(key, entry.get("numberValue").filter(|v| !v.is_null()))?;
                            chans.retain(|(t, _)| *t != d.target);
                            chans.push((d.target, v));
                        }
                        Kind::Integer => {
                            let n = entry.get("numberValue").and_then(Value::as_f64).ok_or_else(|| {
                                invalid(format!(
                                    "Invalid value type for analyzer setting \"{key}\", expected number\n{desc}"
                                ))
                            })?;
                            if let Some(slot) = values.iter_mut().find(|(t, _)| *t == d.target) {
                                slot.1 = SVal::Int(n as i64);
                            }
                        }
                        Kind::Choices => {
                            let opt = if let Some(s) =
                                entry.get("stringValue").and_then(Value::as_str)
                            {
                                d.choices
                                    .iter()
                                    .find(|(t, v)| t == s || v.py_str() == s)
                                    .map(|c| c.1)
                            } else if let Some(x) = entry.get("numberValue").and_then(Value::as_f64)
                            {
                                d.choices.iter().find(|(_, v)| v.eq_num(x)).map(|c| c.1)
                            } else {
                                None
                            };
                            let opt = opt.ok_or_else(|| {
                                invalid(format!(
                                    "Invalid value type for analyzer setting \"{key}\", expected one of {}\n{desc}",
                                    d.choices.iter().map(|(t, _)| format!("\"{t}\"")).collect::<Vec<_>>().join(", ")
                                ))
                            })?;
                            if let Some(slot) = values.iter_mut().find(|(t, _)| *t == d.target) {
                                slot.1 = opt;
                            }
                        }
                    }
                }
                let missing: Vec<&str> = defs
                    .iter()
                    .filter(|d| {
                        d.kind == Kind::Channel
                            && d.required
                            && !chans.iter().any(|c| c.0 == d.target)
                    })
                    .map(|d| d.title)
                    .collect();
                need(missing.is_empty(), "INVALID_REQUEST", || {
                    format!(
                        "Analyzer settings errors: \"Please select a channel for {}\"\n{desc}",
                        missing.join(", ")
                    )
                })?;
                let mut list: Vec<(String, String)> =
                    chans.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
                for (k, v) in values {
                    list.push((k.to_string(), v.spec()));
                }
                (
                    spec.decoder.to_string(),
                    list,
                    Some(spec.classes.iter().map(|c| c.to_string()).collect()),
                )
            } else {
                let rt = lv_decode::runtime().map_err(internal)?;
                let info = rt
                    .decoder(&name)
                    .map_err(|_| invalid(format!("Unknown decoder \"{name}\"")))?;
                need(
                    info.inputs.iter().any(|i| i == "logic"),
                    "INVALID_REQUEST",
                    || {
                        format!(
                            "Analyzer \"{name}\" is not a first-level decoder (inputs: {}). Available Logic 2 analyzers: {}. Any sigrok decoder id with logic input also works.",
                            info.inputs.join(", "),
                            LOGIC2_NAMES.join(", ")
                        )
                    },
                )?;
                let chan_ids: Vec<&str> = info
                    .channels
                    .iter()
                    .chain(&info.opt_channels)
                    .map(|c| c.id.as_str())
                    .collect();
                let opt_ids: Vec<&str> = info.options.iter().map(|o| o.id.as_str()).collect();
                let mut list = Vec::new();
                for (key, entry) in &settings {
                    if chan_ids.contains(&key.as_str()) {
                        list.push((
                            key.clone(),
                            channel(key, entry.get("numberValue").filter(|v| !v.is_null()))?,
                        ));
                    } else {
                        need(opt_ids.contains(&key.as_str()), "INVALID_REQUEST", || {
                            format!(
                                "Invalid setting name: {key}. Channels: {}. Options: {}",
                                chan_ids.join(", "),
                                opt_ids.join(", ")
                            )
                        })?;
                        let v = ["stringValue", "numberValue", "boolValue"]
                            .iter()
                            .find_map(|k| entry.get(*k).filter(|v| !v.is_null()))
                            .ok_or_else(|| invalid(format!("Setting \"{key}\" has no value")))?;
                        list.push((key.clone(), Self::spec_value(key, v)?));
                    }
                }
                let missing: Vec<&str> = info
                    .channels
                    .iter()
                    .map(|c| c.id.as_str())
                    .filter(|c| !list.iter().any(|(k, _)| k == c))
                    .collect();
                need(missing.is_empty(), "INVALID_REQUEST", || {
                    format!("Missing channel setting(s): {}", missing.join(", "))
                })?;
                (name.clone(), list, None)
            };

        let stack = vec![StackEntry {
            id: decoder.clone(),
            args: args_list,
        }];
        let (frames, d) = self.decode(&cap, &stack)?;
        let aid = self.next_analyzer.fetch_add(1, Ordering::SeqCst);
        let label = arg_str(args, "analyzerLabel")
            .filter(|s| !s.is_empty())
            .unwrap_or(&name)
            .to_string();
        let mut an = Analyzer {
            kind: "lla",
            name,
            label,
            stack,
            decoder,
            classes,
            frames,
            input_id: None,
            db_id: None,
        };
        an.db_id = self.store(&cap, &an, &d, None);
        cap.analyzers.lock().map_err(internal)?.insert(aid, an);
        Ok(json!({"analyzerId": aid}))
    }

    fn remove_analyzer(&self, args: &Value) -> R<Value> {
        let cap = self.capture(arg_u64(args, "captureId"))?;
        let aid = arg_u64(args, "analyzerId");
        let mut ans = cap.analyzers.lock().map_err(internal)?;
        let db = match ans.get(&aid) {
            Some(a) if a.kind == "lla" => a.db_id,
            _ => return Err(invalid(format!("Analyzer \"{aid}\" does not exist."))),
        };
        ans.retain(|id, a| *id != aid && a.input_id != Some(aid));
        drop(ans);
        if let Some(db) = db
            && let Err(e) = lv_store::analysis::delete_analyzer(&cap.path, db)
        {
            log::warn!("deleting analyzer frames: {e}");
        }
        Ok(json!({}))
    }

    fn install_hla(&self, ext_dir: &Path, hla_name: &str, protocol: &str) -> R<String> {
        let script = self
            .cfg
            .hla_script
            .clone()
            .filter(|p| p.is_file())
            .ok_or_else(|| {
                internal("extensions/hla.ps1 not found (needed to install HLA extensions)")
            })?;
        let root = self.cfg.hla_root();
        let mut last_err = String::new();
        let mut ok = false;
        for shell in ["pwsh", "powershell"] {
            let mut cmd = std::process::Command::new(shell);
            cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(&script)
                .arg("install")
                .arg(ext_dir)
                .args(["-Protocol", protocol, "-Root"])
                .arg(&root);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            match cmd.output() {
                Ok(out) => {
                    let text = format!(
                        "{}{}",
                        String::from_utf8_lossy(&out.stdout),
                        String::from_utf8_lossy(&out.stderr)
                    );
                    if out.status.success() {
                        ok = true;
                        break;
                    }
                    let t = text.trim();
                    last_err = t[t.len().saturating_sub(800)..].to_string();
                    return Err(invalid(format!(
                        "Installing the extension failed: {last_err}"
                    )));
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        need(ok, "INTERNAL_ERROR", || {
            format!("PowerShell is needed to install HLA extensions ({last_err}).")
        })?;
        let decoders = root.join("decoders");
        let ext_norm = ext_dir.to_string_lossy().to_lowercase();
        for entry in std::fs::read_dir(&decoders).map_err(internal)?.flatten() {
            let manifest = entry.path().join("hla.json");
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                continue;
            };
            let Ok(m) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
                continue;
            };
            let src = m["source"].as_str().unwrap_or("").to_lowercase();
            if src.trim_end_matches(['\\', '/']) == ext_norm.trim_end_matches(['\\', '/'])
                && m["hla_name"].as_str() == Some(hla_name)
                && m["input"].as_str() == Some(protocol)
                && let Some(id) = m["id"].as_str()
            {
                return Ok(id.to_string());
            }
        }
        Err(internal(format!(
            "Installed extension not found under {}",
            decoders.display()
        )))
    }

    fn add_high_level_analyzer(&self, args: &Value) -> R<Value> {
        let cap = self.finished_capture(args)?;
        let ext_arg = arg_str(args, "extensionDirectory").unwrap_or("");
        let ext_dir = std::path::absolute(ext_arg).unwrap_or(PathBuf::from(ext_arg));
        let json_path = ext_dir.join("extension.json");
        let text = std::fs::read_to_string(&json_path).map_err(|_| {
            invalid(format!(
                "Failed to load extension at {}",
                json_path.display()
            ))
        })?;
        let ext: Value =
            serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| {
                invalid(format!(
                    "Failed to load extension at {}: {e}",
                    json_path.display()
                ))
            })?;
        let hlas: Vec<String> = ext["extensions"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(_, v)| v["type"].as_str() == Some("HighLevelAnalyzer"))
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        let hla_name = arg_str(args, "hlaName").unwrap_or("").to_string();
        need(hlas.contains(&hla_name), "INVALID_REQUEST", || {
            format!(
                "HLA \"{hla_name}\" not found in {}. High level analyzers: {}",
                json_path.display(),
                if hlas.is_empty() {
                    "none".to_string()
                } else {
                    hlas.iter()
                        .map(|h| format!("\"{h}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )
        })?;
        let input_id = arg_u64(args, "inputAnalyzerId");
        let source = cap
            .analyzers
            .lock()
            .map_err(internal)?
            .get(&input_id)
            .filter(|a| a.kind == "lla")
            .cloned()
            .ok_or_else(|| invalid(format!("Analyzer Id \"{input_id}\" does not exist.")))?;
        need(
            HLA_INPUTS.contains(&source.decoder.as_str()),
            "INVALID_REQUEST",
            || {
                format!(
                    "High level analyzers take Async Serial, I2C, SPI or CAN input, not \"{}\".",
                    source.name
                )
            },
        )?;
        let decoder = self.install_hla(&ext_dir, &hla_name, &source.decoder)?;

        let rt = lv_decode::runtime().map_err(internal)?;
        let info = rt
            .decoder(&decoder)
            .map_err(|e| invalid(format!("Loading the installed HLA failed: {e}")))?;
        let options: Vec<&lv_decode::OptionInfo> = info
            .options
            .iter()
            .filter(|o| !o.id.starts_with("hla_"))
            .collect();
        let mut hla_args = Vec::new();
        if let Some(settings) = args.get("settings").and_then(Value::as_object) {
            for (key, entry) in settings {
                let opt = options
                    .iter()
                    .find(|o| o.id == *key)
                    .or_else(|| options.iter().find(|o| o.desc == *key))
                    .ok_or_else(|| {
                        invalid(format!(
                            "Unknown High Level Analyzer setting(s): \"{key}\".\nSettings: {}",
                            if options.is_empty() {
                                "none".to_string()
                            } else {
                                options
                                    .iter()
                                    .map(|o| format!("\"{}\"", o.desc))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            }
                        ))
                    })?;
                let value = if let Some(x) = entry.get("numberValue").and_then(Value::as_f64) {
                    g(x)
                } else if let Some(s) = entry.get("stringValue").filter(|v| !v.is_null()) {
                    Self::spec_value(key, s)?
                } else {
                    return Err(invalid(format!(
                        "Encountered unknown High Level Analyzer setting type for key \"{key}\""
                    )));
                };
                need(!value.contains([':', ',', '=']), "INVALID_REQUEST", || {
                    format!(
                        "Setting \"{key}\": values cannot contain \":\", \",\" or \"=\" ('{value}')."
                    )
                })?;
                hla_args.push((opt.id.clone(), value));
            }
        }
        if source.decoder == "spi"
            && source.stack[0]
                .args
                .iter()
                .any(|(k, v)| k == "cs_polarity" && v == "active-high")
        {
            hla_args.push(("hla_spi_cs_polarity".into(), "active-high".into()));
        }
        let mut stack = source.stack.clone();
        stack.push(StackEntry {
            id: decoder.clone(),
            args: hla_args,
        });
        let (frames, d) = self.decode(&cap, &stack)?;
        if let Some(f) = frames.iter().find(|f| {
            f.class == "error"
                && (f.text.starts_with("Setting:") || f.text.starts_with("HLA init failed"))
        }) {
            return Err(invalid(format!(
                "High Level Analyzer settings errors:\n  {}",
                f.text
            )));
        }
        let aid = self.next_analyzer.fetch_add(1, Ordering::SeqCst);
        let label = arg_str(args, "hlaLabel")
            .filter(|s| !s.is_empty())
            .unwrap_or(&hla_name)
            .to_string();
        let mut an = Analyzer {
            kind: "hla",
            name: hla_name,
            label,
            stack,
            decoder,
            classes: None,
            frames,
            input_id: Some(input_id),
            db_id: None,
        };
        an.db_id = self.store(&cap, &an, &d, source.db_id);
        cap.analyzers.lock().map_err(internal)?.insert(aid, an);
        Ok(json!({"analyzerId": aid}))
    }

    fn remove_high_level_analyzer(&self, args: &Value) -> R<Value> {
        let cap = self.capture(arg_u64(args, "captureId"))?;
        let aid = arg_u64(args, "analyzerId");
        let mut ans = cap.analyzers.lock().map_err(internal)?;
        let db = match ans.get(&aid) {
            Some(a) if a.kind == "hla" => a.db_id,
            _ => {
                return Err(invalid(format!(
                    "High Level Analyzer \"{aid}\" does not exist."
                )));
            }
        };
        ans.remove(&aid);
        drop(ans);
        if let Some(db) = db {
            let _ = lv_store::analysis::delete_analyzer(&cap.path, db);
        }
        Ok(json!({}))
    }

    // ---- exports -----------------------------------------------------------------------------

    fn time_fmt(cap: &Capture, rate: f64, iso_ts: bool) -> impl Fn(u64) -> String + '_ {
        move |s| {
            if iso_ts {
                iso(cap.start_wall + s as f64 / rate)
            } else {
                format!("{:.9}", s as f64 / rate)
            }
        }
    }

    /// Channels to export, checked like Logic 2.
    fn export_channels(file: &CaptureFile, args: &Value) -> R<Vec<usize>> {
        let enabled: Vec<usize> = (0..file.info.num_channels)
            .filter(|&c| file.info.channel_enabled.get(c).copied().unwrap_or(true))
            .collect();
        let sel = args.get("logicChannels").cloned().unwrap_or(json!({}));
        if let Some(a) = sel.get("analogChannels").and_then(Value::as_array)
            && !a.is_empty()
        {
            return Err(invalid(format!(
                "Analog channel(s) {} are not enabled in the capture",
                a.iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let wanted: Vec<i64> = sel
            .get("digitalChannels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_f64)
                    .map(|x| x as i64)
                    .collect()
            })
            .unwrap_or_default();
        let chans: Vec<usize> = if wanted.is_empty() {
            enabled.clone()
        } else {
            let missing: Vec<String> = wanted
                .iter()
                .filter(|&&c| c < 0 || !enabled.contains(&(c as usize)))
                .map(|c| c.to_string())
                .collect();
            need(missing.is_empty(), "INVALID_REQUEST", || {
                format!(
                    "Digital channel(s) {} are not enabled in the capture",
                    missing.join(", ")
                )
            })?;
            wanted.iter().map(|&c| c as usize).collect()
        };
        if let Some(r) = arg_f64(args, "analogDownsampleRatio") {
            need((1.0..=1_000_000.0).contains(&r), "INVALID_REQUEST", || {
                "Analog Downsample Rate must be between 1 and 1000000".into()
            })?;
        }
        let dir = arg_str(args, "directory").unwrap_or("");
        need(!dir.is_empty(), "INVALID_REQUEST", || {
            "Export directory is required".into()
        })?;
        std::fs::create_dir_all(dir).map_err(internal)?;
        Ok(chans)
    }

    fn export_raw_data_csv(&self, args: &Value) -> R<Value> {
        let cap = self.finished_capture(args)?;
        let file = cap.file()?;
        let chans = Self::export_channels(&file, args)?;
        let rate = file.info.samplerate_hz.max(1) as f64;
        let when = Self::time_fmt(&cap, rate, truthy(args.get("iso8601Timestamp")));
        let path = Path::new(arg_str(args, "directory").unwrap_or(".")).join("digital.csv");
        let mut w = std::io::BufWriter::new(std::fs::File::create(&path).map_err(internal)?);
        let mut head = vec!["Time [s]".to_string()];
        head.extend(chans.iter().map(|c| format!("Channel {c}")));
        w.write_all(csv_line(&head).as_bytes()).map_err(internal)?;
        let mask = chans.iter().fold(0u8, |m, &c| m | 1 << c);
        let mut prev: Option<u8> = None;
        let mut io_err = None;
        file.for_each_chunk(|first, data| {
            for (i, &b) in data.iter().enumerate() {
                let v = b & mask;
                if prev == Some(v) {
                    continue;
                }
                prev = Some(v);
                let mut row = vec![when(first + i as u64)];
                row.extend(chans.iter().map(|&c| (v >> c & 1).to_string()));
                if let Err(e) = w.write_all(csv_line(&row).as_bytes()) {
                    io_err = Some(e);
                    return Err(lv_store::Error::Corrupt("write failed".into()));
                }
            }
            Ok(())
        })
        .map_err(|e| {
            internal(
                io_err
                    .take()
                    .map(|x| x.to_string())
                    .unwrap_or(e.to_string()),
            )
        })?;
        w.flush().map_err(internal)?;
        Ok(json!({}))
    }

    fn export_raw_data_binary(&self, args: &Value) -> R<Value> {
        let cap = self.finished_capture(args)?;
        let file = cap.file()?;
        let chans = Self::export_channels(&file, args)?;
        let dir = PathBuf::from(arg_str(args, "directory").unwrap_or("."));
        lv_store::export::saleae_binary(&file, &dir, &chans).map_err(internal)?;
        Ok(json!({}))
    }

    /// Table rows of an analyzer, re-decoded for a UART radix.
    fn table_rows(&self, cap: &Capture, an: &Analyzer, radix: i64) -> R<Vec<TableRow>> {
        let mut frames = an.frames.clone();
        if let Some(fmt) = radix_format(radix)
            && an.kind == "lla"
            && an.decoder == "uart"
            && fmt != "hex"
        {
            let mut entry = an.stack[0].clone();
            entry.args.push(("format".into(), fmt.into()));
            frames = self.decode(cap, &[entry])?.0;
        }
        Ok(frames
            .into_iter()
            .filter(|f| an.classes.as_ref().is_none_or(|c| c.contains(&f.class)))
            .map(|f| {
                let class = if an.kind == "hla" {
                    f.class.strip_prefix("rt-").unwrap_or(&f.class).to_string()
                } else {
                    f.class
                };
                (f.ss, f.es, an.label.clone(), class, f.text)
            })
            .collect())
    }

    fn export_data_table_csv(&self, args: &Value) -> R<Value> {
        let cap = self.finished_capture(args)?;
        let ans = cap.analyzers.lock().map_err(internal)?.clone();
        let mut selected: Vec<(Analyzer, i64)> = Vec::new();
        if let Some(configs) = args
            .get("analyzers")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
        {
            for c in configs {
                let id = arg_u64(c, "analyzerId");
                let an = ans
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| invalid(format!("Analyzer Id \"{id}\" does not exist.")))?;
                let radix = arg_f64(c, "radixType").filter(|r| *r != 0.0).unwrap_or(4.0) as i64;
                selected.push((an, radix));
            }
        } else {
            selected = ans.values().cloned().map(|a| (a, 4)).collect();
            need(!selected.is_empty(), "INVALID_REQUEST", || {
                "Exporting the data table requires at least one analyzer already present on the capture.".into()
            })?;
        }
        let mut rows = Vec::new();
        for (an, radix) in &selected {
            rows.extend(self.table_rows(&cap, an, *radix)?);
        }
        rows.sort_by_key(|r| (r.0, r.1));
        let rate = cap.file()?.info.samplerate_hz.max(1) as f64;
        let when = Self::time_fmt(&cap, rate, truthy(args.get("iso8601Timestamp")));
        let columns = ["name", "type", "start_time", "duration", "value"];
        let wanted: Vec<String> = args
            .get("exportColumns")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| columns.iter().map(|c| c.to_string()).collect());
        let unknown: Vec<&str> = wanted
            .iter()
            .map(String::as_str)
            .filter(|c| !columns.contains(c))
            .collect();
        need(unknown.is_empty(), "INVALID_REQUEST", || {
            format!(
                "Unknown column(s) {}. Columns: {}",
                unknown.join(", "),
                columns.join(", ")
            )
        })?;
        let flt = args.get("filter").cloned().unwrap_or(json!({}));
        let query = arg_str(&flt, "query").unwrap_or("").to_lowercase();
        let search: Vec<String> = flt
            .get("columns")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| columns.iter().map(|c| c.to_string()).collect());
        let path = PathBuf::from(arg_str(args, "filepath").unwrap_or(""));
        if let Some(parent) = std::path::absolute(&path)
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            std::fs::create_dir_all(parent).map_err(internal)?;
        }
        let mut w = std::io::BufWriter::new(std::fs::File::create(&path).map_err(internal)?);
        w.write_all(csv_line(&wanted).as_bytes())
            .map_err(internal)?;
        for (ss, es, label, class, text) in rows {
            let field = |c: &str| -> String {
                match c {
                    "name" => label.clone(),
                    "type" => class.clone(),
                    "start_time" => when(ss),
                    "duration" => format!("{:.9}", (es - ss) as f64 / rate),
                    _ => text.clone(),
                }
            };
            if !query.is_empty()
                && !search
                    .iter()
                    .any(|c| field(c).to_lowercase().contains(&query))
            {
                continue;
            }
            let rec: Vec<String> = wanted.iter().map(|c| field(c)).collect();
            w.write_all(csv_line(&rec).as_bytes()).map_err(internal)?;
        }
        w.flush().map_err(internal)?;
        Ok(json!({}))
    }

    fn analyzer(&self, cap: &Capture, args: &Value) -> R<Analyzer> {
        let id = arg_u64(args, "analyzerId");
        cap.analyzers
            .lock()
            .map_err(internal)?
            .get(&id)
            .cloned()
            .ok_or_else(|| invalid(format!("Analyzer Id \"{id}\" does not exist.")))
    }

    fn legacy_export_analyzer(&self, args: &Value) -> R<Value> {
        let cap = self.finished_capture(args)?;
        let an = self.analyzer(&cap, args)?;
        let path = arg_str(args, "filepath").unwrap_or("");
        need(!path.is_empty(), "INVALID_REQUEST", || {
            "Filepath cannot be empty.".into()
        })?;
        let radix = arg_f64(args, "radixType").unwrap_or(0.0) as i64;
        need(radix_format(radix).is_some(), "INVALID_REQUEST", || {
            "Unknown radix type".into()
        })?;
        let rate = cap.file()?.info.samplerate_hz.max(1) as f64;
        let mut w = std::io::BufWriter::new(std::fs::File::create(path).map_err(internal)?);
        w.write_all(csv_line(&["Time [s]".into(), "Type".into(), "Value".into()]).as_bytes())
            .map_err(internal)?;
        for (ss, _, _, class, text) in self.table_rows(&cap, &an, radix)? {
            w.write_all(csv_line(&[format!("{:.9}", ss as f64 / rate), class, text]).as_bytes())
                .map_err(internal)?;
        }
        w.flush().map_err(internal)?;
        Ok(json!({}))
    }

    fn read_analyzer_frames(&self, args: &Value) -> R<Value> {
        let cap = self.finished_capture(args)?;
        let an = self.analyzer(&cap, args)?;
        let rows = self.table_rows(&cap, &an, 0)?;
        let offset = arg_u64(args, "offset") as usize;
        let limit = arg_f64(args, "limit")
            .filter(|l| *l != 0.0)
            .unwrap_or(200.0)
            .max(0.0) as usize;
        let rate = cap.file()?.info.samplerate_hz.max(1) as f64;
        let frames: Vec<Value> = rows
            .iter()
            .skip(offset)
            .take(limit)
            .map(|(ss, es, _, class, text)| {
                json!({"type": class, "start_time": *ss as f64 / rate, "end_time": *es as f64 / rate, "value": text})
            })
            .collect();
        Ok(json!({"total": rows.len(), "offset": offset, "frames": frames}))
    }

    fn query_frames(&self, args: &Value) -> R<Value> {
        let cap = self.finished_capture(args)?;
        let sql = arg_str(args, "sql").unwrap_or("");
        need(!sql.trim().is_empty(), "INVALID_REQUEST", || {
            "sql must not be empty".into()
        })?;
        let file = cap.file()?;
        let q = file
            .query(sql, 1000, Duration::from_secs(2))
            .map_err(invalid)?;
        use lv_store::analysis::Cell;
        let rows: Vec<Value> = q
            .rows
            .iter()
            .map(|r| {
                Value::Array(
                    r.iter()
                        .map(|c| match c {
                            Cell::Null => Value::Null,
                            Cell::Int(i) => json!(i),
                            Cell::Real(x) => json!(x),
                            Cell::Text(t) => json!(t),
                            Cell::Blob(n) => json!(format!("<blob {n} bytes>")),
                        })
                        .collect(),
                )
            })
            .collect();
        Ok(json!({"columns": q.columns, "rows": rows, "truncated": q.truncated}))
    }
}

/// Capture into `req.output` (a device capture of `start_capture`).
fn run_capture(
    cap: &Capture,
    found: FoundDevice,
    req: CaptureRequest,
    slot: &DeviceSlot,
    enabled: &[usize],
    trim: f64,
) -> R<()> {
    let dev_err = |e: String| McpError::new("DEVICE_ERROR", e);
    let path = req.output.clone();
    let trigger_mode = req.trigger.is_some();
    let cached = slot.lock().ok().and_then(|mut s| s.take());
    let dev = match lv_fx2lafw::reuse(cached, &found) {
        Some(d) => d,
        None => lv_fx2lafw::open(&found, &lv_fx2lafw::default_firmware_dirs())
            .map_err(|e| dev_err(format!("Opening the device failed: {e}")))?,
    };
    let c = lv_capture::Capture::start(dev, req)
        .map_err(|e| dev_err(format!("Capture failed: {e}")))?;
    while !c.is_finished() {
        if cap.stop.load(Ordering::Relaxed) {
            c.stop();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (dev, report) = c.wait();
    let gone = matches!(&report, Ok(r) if r.outcome == Outcome::DeviceError(StreamEnd::Disconnected))
        || report.is_err();
    if let Some(d) = dev
        && !gone
        && let Ok(mut s) = slot.lock()
    {
        *s = Some(d);
    }
    let r = report.map_err(|e| dev_err(format!("Capture failed: {e}")))?;
    if r.outcome == Outcome::NotTriggered || r.info.num_samples == 0 {
        return Err(dev_err(format!(
            "Capture stopped before any data was recorded{}",
            if trigger_mode {
                " (trigger not reached)."
            } else {
                "."
            }
        )));
    }
    if !r.outcome.is_ok() {
        return Err(dev_err(format!("Capture failed: {}", r.outcome.describe())));
    }
    if trim > 0.0 {
        let keep = (trim * r.info.samplerate_hz as f64).round() as u64;
        if keep < r.info.num_samples {
            trim_capture(&path, keep).map_err(internal)?;
        }
    }
    let names: Vec<String> = (0..8).map(|i| format!("D{i}")).collect();
    let flags: Vec<bool> = (0..8).map(|c| enabled.contains(&c)).collect();
    lv_store::fold_wal(&path).ok();
    lv_store::update_channels(&path, &names, &flags).map_err(internal)?;
    Ok(())
}

/// Keep only the last `keep` samples of a capture.
fn trim_capture(path: &Path, keep: u64) -> lv_store::Result<()> {
    let tmp = path.with_extension("trim.lvcap");
    {
        let f = CaptureFile::open(path)?;
        let n = f.info.num_samples;
        let mut w = CaptureWriter::create(
            &tmp,
            &NewCapture {
                device: f.info.device.clone(),
                samplerate_hz: f.info.samplerate_hz,
                channel_names: f.info.channel_names.clone(),
            },
            true,
        )?;
        let mut pos = n - keep;
        while pos < n {
            let len = (n - pos).min(1 << 20);
            w.push(&f.read_samples(pos, len)?)?;
            pos += len;
        }
        w.finish(Status::Complete)?;
    }
    remove_db(path);
    std::fs::rename(&tmp, path).map_err(|e| lv_store::Error::Io(path.to_path_buf(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aut3_number_and_time_text() {
        assert_eq!(g(1.0), "1");
        assert_eq!(g(1.5), "1.5");
        assert_eq!(SVal::Float(1.0).py_str(), "1.0");
        assert_eq!(SVal::Int(8).py_str(), "8");
        assert_eq!(iso(0.0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_762_740_000.25), "2025-11-10T02:00:00.250000Z");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn aut3_logic2_settings_text() {
        let d = logic2("Async Serial").unwrap();
        let text = describe(&d.settings, &[0, 1]);
        assert!(text.contains("\"Input Channel\" (channel, required, enabled channels: 0, 1)"));
        assert!(text.contains("\"Bit Rate (Bits/s)\" (integer, default: 9600)"));
        assert!(text.contains("\"No Parity Bit (Standard)\" [default]"));
        assert!(text.contains("\"8 Bits per Transfer (Standard)\" [default]"));
    }
}
