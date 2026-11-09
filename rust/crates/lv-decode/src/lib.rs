//! Protocol decoding with libsigrokdecode (DEC-1, DEC-2, DEC-4).
//!
//! The decoder runtime (libsigrokdecode, glib, Python and the decoder
//! scripts) is a folder of the existing app build, loaded at run time:
//!
//! ```text
//! <runtime>/libsigrokdecode-4.dll, libglib-2.0-0.dll, libpython3.*.dll, ...
//! <runtime>/lib/python3.*/                     Python standard library
//! <runtime>/share/libsigrokdecode/decoders/    decoder scripts
//! ```
//!
//! It is found through `LOGICVIEW_SIGROK_DIR`, then `sigrok\` next to the
//! executable, then the executable's own folder. libsigrokdecode embeds
//! Python, which can only be started once per process, so the runtime is
//! global and decoding sessions run one at a time.

mod ffi;

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use lv_store::CaptureFile;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Runtime(String),
    #[error("{0}")]
    Spec(String),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error(transparent)]
    Store(#[from] lv_store::Error),
    #[error("decoding was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

/// A decoder option value, typed like the decoder's default.
#[derive(Debug, Clone, PartialEq)]
pub enum OptValue {
    Str(String),
    Int(i64),
    Float(f64),
}

impl std::fmt::Display for OptValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OptValue::Str(s) => write!(f, "{s}"),
            OptValue::Int(i) => write!(f, "{i}"),
            OptValue::Float(x) => write!(f, "{x}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    pub desc: String,
}

#[derive(Debug, Clone)]
pub struct OptionInfo {
    pub id: String,
    pub desc: String,
    pub default: Option<OptValue>,
    pub values: Vec<OptValue>,
}

#[derive(Debug, Clone)]
pub struct RowInfo {
    pub id: String,
    pub desc: String,
    pub classes: Vec<usize>,
}

/// What a decoder declares about itself.
#[derive(Debug, Clone)]
pub struct DecoderInfo {
    pub id: String,
    pub name: String,
    pub longname: String,
    pub desc: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub channels: Vec<ChannelInfo>,
    pub opt_channels: Vec<ChannelInfo>,
    pub options: Vec<OptionInfo>,
    /// (id, description) by class index.
    pub annotations: Vec<(String, String)>,
    pub rows: Vec<RowInfo>,
}

impl DecoderInfo {
    /// The annotation row a class is shown in. Decoders without rows get
    /// one row per class.
    pub fn row_of(&self, class: usize) -> usize {
        if self.rows.is_empty() {
            return class;
        }
        self.rows
            .iter()
            .position(|r| r.classes.contains(&class))
            .unwrap_or(self.rows.len())
    }

    pub fn class_id(&self, class: usize) -> &str {
        self.annotations
            .get(class)
            .map(|a| a.0.as_str())
            .unwrap_or("?")
    }

    fn channel(&self, id: &str) -> Option<&ChannelInfo> {
        self.channels
            .iter()
            .chain(&self.opt_channels)
            .find(|c| c.id == id)
    }
}

/// One decoder of a stack and its `key=value` settings (channels for the
/// first decoder, options for any), as in `sigrok-cli -P`.
#[derive(Debug, Clone, PartialEq)]
pub struct StackEntry {
    pub id: String,
    pub args: Vec<(String, String)>,
}

/// Parse `uart:rx=D0:baudrate=115200,midi` (bottom decoder first).
pub fn parse_stack(spec: &str) -> Result<Vec<StackEntry>> {
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let mut fields = part.split(':');
        let id = fields.next().unwrap_or_default().trim().to_string();
        if id.is_empty() {
            return Err(Error::Spec(format!("'{part}': decoder name is missing")));
        }
        let mut args = Vec::new();
        for f in fields {
            let (k, v) = f
                .split_once('=')
                .ok_or_else(|| Error::Spec(format!("'{f}': write key=value")))?;
            args.push((k.trim().to_string(), v.trim().to_string()));
        }
        out.push(StackEntry { id, args });
    }
    if out.is_empty() {
        return Err(Error::Spec("no decoder given".into()));
    }
    Ok(out)
}

/// `analyzer.kind` for a stack: `hla` when it runs a Saleae HLA (installed
/// by `extensions/hla.ps1` as an `hla_*` decoder, DEC-3), else `sigrok`.
pub fn analyzer_kind(stack: &[StackEntry]) -> &'static str {
    if stack.iter().any(|e| e.id.starts_with("hla_")) {
        "hla"
    } else {
        "sigrok"
    }
}

/// Back to the `sigrok-cli -P` form.
pub fn stack_text(stack: &[StackEntry]) -> String {
    stack
        .iter()
        .map(|e| {
            let mut s = e.id.clone();
            for (k, v) in &e.args {
                s.push_str(&format!(":{k}={v}"));
            }
            s
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// One annotation from a decoder of the stack.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub start: u64,
    pub end: u64,
    /// Index of the decoder in the stack.
    pub level: usize,
    pub class: usize,
    /// Longest text first, then shorter forms.
    pub texts: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Decoded {
    pub stack: Vec<DecoderInfo>,
    /// What each decoder of the stack calls its output (e.g. `uart-1`, as
    /// sigrok-cli prints it); the decoder id if it produced nothing.
    pub proto_ids: Vec<String>,
    pub annotations: Vec<Annotation>,
    /// Warnings and errors the decoders logged.
    pub log: Vec<String>,
}

/// Progress and cancellation of a running decode (DEC-4).
#[derive(Debug, Default)]
pub struct Control {
    /// Samples handed to the decoders so far.
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

pub struct Runtime {
    api: ffi::Api,
    dir: PathBuf,
    session: Mutex<()>,
    all_loaded: OnceLock<()>,
}

// The raw pointers in `Api` are function pointers into DLLs that stay
// loaded; all calls into libsigrokdecode go through `session`.
unsafe impl Send for Runtime {}
unsafe impl Sync for Runtime {}

static RUNTIME: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
static VPRINTF: OnceLock<(
    unsafe extern "C" fn(*const c_char, *mut c_void) -> *mut c_char,
    unsafe extern "C" fn(*mut c_void),
)> = OnceLock::new();

unsafe extern "C" fn log_cb(
    _data: *mut c_void,
    level: c_int,
    format: *const c_char,
    args: *mut c_void,
) -> c_int {
    if level > ffi::SRD_LOG_WARN {
        return 0;
    }
    if let Some((vprintf, free)) = VPRINTF.get() {
        unsafe {
            let p = vprintf(format, args);
            let msg = ffi::string(p);
            free(p.cast());
            log::warn!("srd: {msg}");
            if let Ok(mut l) = LOG.lock() {
                l.push(msg);
            }
        }
    }
    0
}

static EXTRA_DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Search `dir` for decoders too (e.g. where HLAs get installed). Takes
/// effect when the runtime starts, so call it before [`runtime`]; decoders
/// installed there later are still found when first used.
pub fn add_decoder_dir(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut v) = EXTRA_DIRS.lock()
        && !v.iter().any(|d| d == dir)
    {
        v.push(dir.to_path_buf());
    }
}

fn has_runtime(dir: &Path) -> bool {
    dir.join("libsigrokdecode-4.dll").is_file()
        && dir.join("share/libsigrokdecode/decoders").is_dir()
}

/// Where the decoder runtime is looked for, in order.
pub fn runtime_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = std::env::var_os("LOGICVIEW_SIGROK_DIR") {
        v.push(PathBuf::from(d));
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        v.push(exe_dir.join("sigrok"));
        v.push(exe_dir);
    }
    v
}

/// The decoder runtime, started on first use.
pub fn runtime() -> Result<&'static Runtime> {
    RUNTIME
        .get_or_init(start_runtime)
        .as_ref()
        .map_err(|e| Error::Runtime(e.clone()))
}

fn start_runtime() -> std::result::Result<Runtime, String> {
    let candidates = runtime_candidates();
    let dir = candidates.iter().find(|d| has_runtime(d)).cloned().ok_or_else(|| {
        format!(
            "decoder runtime (libsigrokdecode-4.dll with share\\libsigrokdecode\\decoders) not found in: {}. \
             Set LOGICVIEW_SIGROK_DIR to the app folder that has it",
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;
    // The embedded Python finds its standard library through PYTHONHOME,
    // since our executable is not next to it.
    let has_stdlib = std::fs::read_dir(dir.join("lib"))
        .map(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("python3"))
        })
        .unwrap_or(false);
    if has_stdlib && std::env::var_os("PYTHONHOME").is_none() {
        // SAFETY: done once, before Python starts; Windows keeps the
        // environment behind a lock, so concurrent readers are safe.
        unsafe { std::env::set_var("PYTHONHOME", &dir) };
    }
    let extra = EXTRA_DIRS.lock().map(|v| v.clone()).unwrap_or_default();
    if !extra.is_empty() {
        let mut paths: Vec<PathBuf> = std::env::var_os("SIGROKDECODE_PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        paths.extend(extra);
        if let Ok(joined) = std::env::join_paths(paths) {
            // SAFETY: as for PYTHONHOME below.
            unsafe { std::env::set_var("SIGROKDECODE_PATH", joined) };
        }
    }
    // Python loads extension modules (e.g. _ctypes -> libffi) with the
    // default DLL search dirs, which hold the executable's folder, not this
    // one; add it.
    add_dll_directory(&dir);
    let api = unsafe { ffi::Api::load(&dir)? };
    let _ = VPRINTF.set((api.g_strdup_vprintf, api.g_free));
    let decoders = dir.join("share").join("libsigrokdecode").join("decoders");
    let path = CString::new(decoders.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    unsafe {
        (api.srd_log_callback_set)(log_cb, std::ptr::null_mut());
        (api.srd_log_loglevel_set)(ffi::SRD_LOG_WARN);
        let rc = (api.srd_init)(path.as_ptr());
        if rc != ffi::SRD_OK {
            return Err(format!(
                "libsigrokdecode did not start ({}): {}",
                ffi::string((api.srd_strerror)(rc)),
                take_log().join("; ")
            ));
        }
    }
    log::info!("decoder runtime: {}", dir.display());
    Ok(Runtime {
        api,
        dir,
        session: Mutex::new(()),
        all_loaded: OnceLock::new(),
    })
}

#[cfg(windows)]
fn add_dll_directory(dir: &Path) {
    use std::os::windows::ffi::OsStrExt;
    unsafe extern "system" {
        fn AddDllDirectory(dir: *const u16) -> *mut c_void;
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    if unsafe { AddDllDirectory(wide.as_ptr()) }.is_null() {
        log::warn!("AddDllDirectory({}) failed", dir.display());
    }
}

#[cfg(not(windows))]
fn add_dll_directory(_dir: &Path) {}

fn take_log() -> Vec<String> {
    LOG.lock()
        .map(|mut l| std::mem::take(&mut *l))
        .unwrap_or_default()
}

/// Collects annotations from the session callback.
struct Collector {
    insts: Vec<*mut ffi::srd_decoder_inst>,
    proto_ids: Vec<Option<String>>,
    out: Vec<Annotation>,
}

unsafe extern "C" fn ann_cb(pdata: *mut ffi::srd_proto_data, data: *mut c_void) {
    unsafe {
        let c = &mut *(data as *mut Collector);
        let pd = &*pdata;
        let di = (*pd.pdo).di;
        let Some(level) = c.insts.iter().position(|&i| i == di) else {
            return;
        };
        if c.proto_ids[level].is_none() {
            c.proto_ids[level] = Some(ffi::string((*pd.pdo).proto_id));
        }
        let ann = &*(pd.data as *const ffi::srd_proto_data_annotation);
        let mut texts = Vec::new();
        let mut p = ann.ann_text;
        while !p.is_null() && !(*p).is_null() {
            texts.push(ffi::string(*p));
            p = p.add(1);
        }
        c.out.push(Annotation {
            start: pd.start_sample,
            end: pd.end_sample,
            level,
            class: ann.ann_class.max(0) as usize,
            texts,
        });
    }
}

impl Runtime {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn gvar(&self, v: *mut c_void) -> Option<OptValue> {
        if v.is_null() {
            return None;
        }
        unsafe {
            let t = ffi::string((self.api.g_variant_get_type_string)(v));
            match t.as_str() {
                "s" => Some(OptValue::Str(ffi::string((self.api.g_variant_get_string)(
                    v,
                    std::ptr::null_mut(),
                )))),
                "x" => Some(OptValue::Int((self.api.g_variant_get_int64)(v))),
                "d" => Some(OptValue::Float((self.api.g_variant_get_double)(v))),
                _ => None,
            }
        }
    }

    /// # Safety
    /// `d` must point to a loaded decoder.
    unsafe fn info(&self, d: *const ffi::srd_decoder) -> DecoderInfo {
        unsafe {
            let d = &*d;
            let strings = |l: *mut ffi::GSList| -> Vec<String> {
                ffi::slist(l)
                    .map(|p| ffi::string(p as *const c_char))
                    .collect()
            };
            let channels = |l: *mut ffi::GSList| -> Vec<ChannelInfo> {
                ffi::slist(l)
                    .map(|p| {
                        let c = &*(p as *const ffi::srd_channel);
                        ChannelInfo {
                            id: ffi::string(c.id),
                            name: ffi::string(c.name),
                            desc: ffi::string(c.desc),
                        }
                    })
                    .collect()
            };
            DecoderInfo {
                id: ffi::string(d.id),
                name: ffi::string(d.name),
                longname: ffi::string(d.longname),
                desc: ffi::string(d.desc),
                inputs: strings(d.inputs),
                outputs: strings(d.outputs),
                channels: channels(d.channels),
                opt_channels: channels(d.opt_channels),
                options: ffi::slist(d.options)
                    .map(|p| {
                        let o = &*(p as *const ffi::srd_decoder_option);
                        OptionInfo {
                            id: ffi::string(o.id),
                            desc: ffi::string(o.desc),
                            default: self.gvar(o.def),
                            values: ffi::slist(o.values).filter_map(|v| self.gvar(v)).collect(),
                        }
                    })
                    .collect(),
                annotations: ffi::slist(d.annotations)
                    .map(|p| {
                        let a = p as *const *const c_char;
                        (ffi::string(*a), ffi::string(*a.add(1)))
                    })
                    .collect(),
                rows: ffi::slist(d.annotation_rows)
                    .map(|p| {
                        let r = &*(p as *const ffi::srd_decoder_annotation_row);
                        RowInfo {
                            id: ffi::string(r.id),
                            desc: ffi::string(r.desc),
                            classes: ffi::slist(r.ann_classes).map(|c| c as usize).collect(),
                        }
                    })
                    .collect(),
            }
        }
    }

    /// Every installed decoder, sorted by id.
    pub fn decoders(&self) -> Vec<DecoderInfo> {
        let _g = self.session.lock();
        self.all_loaded.get_or_init(|| unsafe {
            (self.api.srd_decoder_load_all)();
        });
        let mut v: Vec<DecoderInfo> = unsafe {
            ffi::slist((self.api.srd_decoder_list)())
                .map(|d| self.info(d as *const ffi::srd_decoder))
                .collect()
        };
        take_log();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    fn load(&self, id: &str) -> Result<DecoderInfo> {
        let cid = CString::new(id).map_err(|_| Error::Spec(format!("bad decoder name '{id}'")))?;
        unsafe {
            let mut d = (self.api.srd_decoder_get_by_id)(cid.as_ptr());
            if d.is_null() {
                (self.api.srd_decoder_load)(cid.as_ptr());
                d = (self.api.srd_decoder_get_by_id)(cid.as_ptr());
            }
            if d.is_null() {
                let log = take_log();
                return Err(Error::Spec(if log.is_empty() {
                    format!("unknown decoder '{id}' (see `lvcli decoders`)")
                } else {
                    format!("decoder '{id}' did not load: {}", log.join("; "))
                }));
            }
            Ok(self.info(d))
        }
    }

    /// One decoder's description, loading it if needed.
    pub fn decoder(&self, id: &str) -> Result<DecoderInfo> {
        let _g = self.session.lock();
        self.load(id)
    }

    /// Run a decoder stack over a whole capture.
    pub fn decode(
        &self,
        file: &CaptureFile,
        stack: &[StackEntry],
        ctl: &Control,
    ) -> Result<Decoded> {
        let _g = self
            .session
            .lock()
            .map_err(|_| Error::Runtime("decoder runtime is unusable".into()))?;
        take_log();
        let infos = stack
            .iter()
            .map(|e| self.load(&e.id))
            .collect::<Result<Vec<_>>>()?;
        let names = &file.info.channel_names;
        let api = &self.api;

        let mut sess: *mut c_void = std::ptr::null_mut();
        if unsafe { (api.srd_session_new)(&mut sess) } != ffi::SRD_OK {
            return Err(Error::Runtime("could not create a decoding session".into()));
        }
        // Destroy the session however this ends.
        struct Session<'a>(&'a ffi::Api, *mut c_void);
        impl Drop for Session<'_> {
            fn drop(&mut self) {
                unsafe { (self.0.srd_session_destroy)(self.1) };
            }
        }
        let session = Session(api, sess);

        let mut collector = Box::new(Collector {
            insts: Vec::new(),
            proto_ids: vec![None; stack.len()],
            out: Vec::new(),
        });
        for (level, (entry, info)) in stack.iter().zip(&infos).enumerate() {
            let (opts, chans) = self.split_args(entry, info, level, names)?;
            unsafe {
                let ohash = (api.g_hash_table_new_full)(
                    api.g_str_hash,
                    api.g_str_equal,
                    Some(api.g_free),
                    Some(api.g_variant_unref),
                );
                for (k, v) in &opts {
                    let key = CString::new(k.as_str()).unwrap_or_default();
                    let var = match v {
                        OptValue::Str(s) => {
                            let s = CString::new(s.as_str()).unwrap_or_default();
                            (api.g_variant_new_string)(s.as_ptr())
                        }
                        OptValue::Int(i) => (api.g_variant_new_int64)(*i),
                        OptValue::Float(x) => (api.g_variant_new_double)(*x),
                    };
                    (api.g_hash_table_insert)(
                        ohash,
                        (api.g_strdup)(key.as_ptr()).cast(),
                        (api.g_variant_ref_sink)(var),
                    );
                }
                let cid = CString::new(entry.id.as_str()).unwrap_or_default();
                let di = (api.srd_inst_new)(sess, cid.as_ptr(), ohash);
                (api.g_hash_table_destroy)(ohash);
                if di.is_null() {
                    return Err(Error::Spec(format!(
                        "could not set up decoder '{}': {}",
                        entry.id,
                        take_log().join("; ")
                    )));
                }
                if level == 0 {
                    let chash = (api.g_hash_table_new_full)(
                        api.g_str_hash,
                        api.g_str_equal,
                        Some(api.g_free),
                        Some(api.g_variant_unref),
                    );
                    for (k, idx) in &chans {
                        let key = CString::new(k.as_str()).unwrap_or_default();
                        (api.g_hash_table_insert)(
                            chash,
                            (api.g_strdup)(key.as_ptr()).cast(),
                            (api.g_variant_ref_sink)((api.g_variant_new_int32)(*idx as i32)),
                        );
                    }
                    let rc = (api.srd_inst_channel_set_all)(di, chash);
                    (api.g_hash_table_destroy)(chash);
                    if rc != ffi::SRD_OK {
                        return Err(Error::Spec(format!(
                            "channels for '{}': {}",
                            entry.id,
                            take_log().join("; ")
                        )));
                    }
                }
                if let Some(&prev) = collector.insts.last()
                    && (api.srd_inst_stack)(sess, prev, di) != ffi::SRD_OK
                {
                    return Err(Error::Spec(format!(
                        "'{}' cannot be stacked on '{}': {}",
                        entry.id,
                        stack[level - 1].id,
                        take_log().join("; ")
                    )));
                }
                collector.insts.push(di);
            }
        }

        let rate = file.info.samplerate_hz;
        unsafe {
            (api.srd_pd_output_callback_add)(
                sess,
                ffi::SRD_OUTPUT_ANN,
                ann_cb,
                (&mut *collector as *mut Collector).cast(),
            );
            (api.srd_session_metadata_set)(
                sess,
                ffi::SRD_CONF_SAMPLERATE,
                (api.g_variant_new_uint64)(rate),
            );
            if (api.srd_session_start)(sess) != ffi::SRD_OK {
                return Err(Error::Decode(format!(
                    "could not start: {}",
                    take_log().join("; ")
                )));
            }
        }
        ctl.total.store(file.info.num_samples, Ordering::Relaxed);
        ctl.done.store(0, Ordering::Relaxed);
        let mut failed = None;
        file.for_each_chunk(|first, data| {
            if ctl.cancel.load(Ordering::Relaxed) {
                failed = Some(Error::Cancelled);
                return Err(lv_store::Error::Corrupt("cancelled".into()));
            }
            let end = first + data.len() as u64;
            let rc = unsafe {
                (api.srd_session_send)(sess, first, end, data.as_ptr(), data.len() as u64, 1)
            };
            if rc != ffi::SRD_OK {
                failed = Some(Error::Decode(take_log().join("\n")));
                return Err(lv_store::Error::Corrupt("decoder error".into()));
            }
            ctl.done.store(end, Ordering::Relaxed);
            Ok(())
        })
        .or_else(|e| if failed.is_some() { Ok(()) } else { Err(e) })?;
        if let Some(e) = failed {
            unsafe { (api.srd_session_terminate_reset)(sess) };
            return Err(e);
        }
        unsafe { (api.srd_session_send_eof)(sess) };
        drop(session);
        // In the order the decoders emitted them, like sigrok-cli prints.
        let annotations = std::mem::take(&mut collector.out);
        let proto_ids = collector
            .proto_ids
            .iter()
            .zip(&infos)
            .map(|(p, d)| p.clone().unwrap_or_else(|| d.id.clone()))
            .collect();
        Ok(Decoded {
            stack: infos,
            proto_ids,
            annotations,
            log: take_log(),
        })
    }

    /// Sort `key=value` settings into typed options and channel indices.
    #[allow(clippy::type_complexity)]
    fn split_args(
        &self,
        entry: &StackEntry,
        info: &DecoderInfo,
        level: usize,
        names: &[String],
    ) -> Result<(Vec<(String, OptValue)>, Vec<(String, usize)>)> {
        let mut opts = Vec::new();
        let mut chans = Vec::new();
        for (k, v) in &entry.args {
            if let Some(o) = info.options.iter().find(|o| &o.id == k) {
                let val = match &o.default {
                    Some(OptValue::Int(_)) => OptValue::Int(parse_int(v).ok_or_else(|| {
                        Error::Spec(format!(
                            "{}: option '{k}' needs a whole number, not '{v}'",
                            info.id
                        ))
                    })?),
                    Some(OptValue::Float(_)) => OptValue::Float(v.parse().map_err(|_| {
                        Error::Spec(format!(
                            "{}: option '{k}' needs a number, not '{v}'",
                            info.id
                        ))
                    })?),
                    _ => OptValue::Str(v.clone()),
                };
                opts.push((k.clone(), val));
            } else if info.channel(k).is_some() {
                if level > 0 {
                    return Err(Error::Spec(format!(
                        "{}: only the first decoder of a stack takes channels ('{k}')",
                        info.id
                    )));
                }
                chans.push((k.clone(), resolve_channel(v, names).ok_or_else(|| {
                    Error::Spec(format!(
                        "{}: channel '{v}' for '{k}' does not exist (use 0..7, D0..D7 or a channel name)",
                        info.id
                    ))
                })?));
            } else {
                let known: Vec<&str> = info
                    .channels
                    .iter()
                    .chain(&info.opt_channels)
                    .map(|c| c.id.as_str())
                    .chain(info.options.iter().map(|o| o.id.as_str()))
                    .collect();
                return Err(Error::Spec(format!(
                    "{}: unknown option or channel '{k}' (known: {})",
                    info.id,
                    known.join(", ")
                )));
            }
        }
        Ok((opts, chans))
    }
}

/// One annotation row of a stored analyzer (UI-9).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RowDesc {
    pub decoder: String,
    pub id: String,
    pub desc: String,
}

/// `analyzer.settings_json` of a decoder stack.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// The stack in `sigrok-cli -P` form.
    pub stack: String,
    /// Rows of all decoders, bottom decoder first; `frame.ann_row` indexes this.
    pub rows: Vec<RowDesc>,
}

impl Settings {
    pub fn parse(json: &str) -> Option<Settings> {
        serde_json::from_str(json).ok()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The rows of every decoder and where each decoder's rows start.
pub fn stack_rows(stack: &[DecoderInfo]) -> (Vec<RowDesc>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut offsets = Vec::new();
    for d in stack {
        offsets.push(rows.len());
        if d.rows.is_empty() {
            for (id, desc) in &d.annotations {
                rows.push(RowDesc {
                    decoder: d.id.clone(),
                    id: id.clone(),
                    desc: desc.clone(),
                });
            }
        } else {
            for r in &d.rows {
                rows.push(RowDesc {
                    decoder: d.id.clone(),
                    id: r.id.clone(),
                    desc: r.desc.clone(),
                });
            }
            // Classes that are in no row get one more.
            if (0..d.annotations.len()).any(|c| d.row_of(c) == d.rows.len()) {
                rows.push(RowDesc {
                    decoder: d.id.clone(),
                    id: "other".into(),
                    desc: "Other".into(),
                });
            }
        }
    }
    (rows, offsets)
}

impl Decoded {
    /// Settings and frames to store with [`lv_store::analysis::add_analyzer`].
    pub fn to_frames(&self, stack: &[StackEntry]) -> (Settings, Vec<lv_store::analysis::Frame>) {
        let (rows, offsets) = stack_rows(&self.stack);
        let frames = self
            .annotations
            .iter()
            .map(|a| {
                let d = &self.stack[a.level];
                lv_store::analysis::Frame {
                    start: a.start,
                    end: a.end,
                    row: (offsets[a.level] + d.row_of(a.class)) as i64,
                    kind: format!("{}.{}", d.id, d.class_id(a.class)),
                    text: a.texts.first().cloned().unwrap_or_default(),
                    data: (a.texts.len() > 1)
                        .then(|| serde_json::to_string(&a.texts[1..]).unwrap_or_default()),
                }
            })
            .collect();
        (
            Settings {
                stack: stack_text(stack),
                rows,
            },
            frames,
        )
    }
}

/// `strtoll(s, 0)`-like: decimal, `0x` hex or leading-0 octal.
fn parse_int(s: &str) -> Option<i64> {
    let (neg, t) = match s.strip_prefix('-') {
        Some(t) => (true, t),
        None => (false, s),
    };
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()?
    } else if t.len() > 1 && t.starts_with('0') {
        i64::from_str_radix(&t[1..], 8).ok()?
    } else {
        t.parse().ok()?
    };
    Some(if neg { -v } else { v })
}

/// `3`, `D3` or a channel name (case ignored) -> channel index.
pub fn resolve_channel(v: &str, names: &[String]) -> Option<usize> {
    let n = names.len().max(1);
    let t = v.trim();
    if let Ok(i) = t.parse::<usize>() {
        return (i < n).then_some(i);
    }
    if let Some(i) = names.iter().position(|c| c.eq_ignore_ascii_case(t)) {
        return Some(i);
    }
    t.strip_prefix(['D', 'd'])
        .and_then(|i| i.parse::<usize>().ok())
        .filter(|&i| i < n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dec2_parse_stack() {
        let s = parse_stack("uart:rx=D0:baudrate=115200, midi").unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].id, "uart");
        assert_eq!(
            s[0].args,
            vec![
                ("rx".to_string(), "D0".to_string()),
                ("baudrate".to_string(), "115200".to_string())
            ]
        );
        assert_eq!(stack_text(&s), "uart:rx=D0:baudrate=115200,midi");
        assert!(parse_stack("").is_err());
        assert!(parse_stack("uart:rx").is_err());
        assert!(parse_stack(":rx=0").is_err());
    }

    #[test]
    fn dec1_channel_names() {
        let names: Vec<String> = ["UART TX", "SCL", "SDA", "D3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(resolve_channel("2", &names), Some(2));
        assert_eq!(resolve_channel("D1", &names), Some(1));
        assert_eq!(resolve_channel("sda", &names), Some(2));
        assert_eq!(resolve_channel("uart tx", &names), Some(0));
        assert_eq!(resolve_channel("D3", &names), Some(3));
        assert_eq!(resolve_channel("9", &names), None);
        assert_eq!(resolve_channel("MISO", &names), None);
    }

    #[test]
    fn dec1_int_options() {
        assert_eq!(parse_int("115200"), Some(115_200));
        assert_eq!(parse_int("0x50"), Some(0x50));
        assert_eq!(parse_int("010"), Some(8));
        assert_eq!(parse_int("-3"), Some(-3));
        assert_eq!(parse_int("fast"), None);
    }
}
