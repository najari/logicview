//! libsigrokdecode and the glib calls it needs, loaded from the decoder
//! runtime folder at run time (no import libraries, NFR-4).
//!
//! Struct layouts follow `libsigrokdecode.h` of the bundled build (0.6).

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};
use std::path::Path;

pub type gboolean = c_int;

#[repr(C)]
pub struct GSList {
    pub data: *mut c_void,
    pub next: *mut GSList,
}

#[repr(C)]
pub struct srd_decoder {
    pub id: *const c_char,
    pub name: *const c_char,
    pub longname: *const c_char,
    pub desc: *const c_char,
    pub license: *const c_char,
    pub inputs: *mut GSList,
    pub outputs: *mut GSList,
    pub tags: *mut GSList,
    pub channels: *mut GSList,
    pub opt_channels: *mut GSList,
    /// Items: `char *[2]` (id, description).
    pub annotations: *mut GSList,
    pub annotation_rows: *mut GSList,
    pub binary: *mut GSList,
    pub logic_output_channels: *mut GSList,
    pub options: *mut GSList,
    pub py_mod: *mut c_void,
    pub py_dec: *mut c_void,
}

#[repr(C)]
pub struct srd_channel {
    pub id: *const c_char,
    pub name: *const c_char,
    pub desc: *const c_char,
    pub order: c_int,
}

#[repr(C)]
pub struct srd_decoder_option {
    pub id: *const c_char,
    pub desc: *const c_char,
    pub def: *mut c_void,
    pub values: *mut GSList,
}

#[repr(C)]
pub struct srd_decoder_annotation_row {
    pub id: *const c_char,
    pub desc: *const c_char,
    /// Items: `GINT_TO_POINTER(class)`.
    pub ann_classes: *mut GSList,
}

/// Only the leading fields are used.
#[repr(C)]
pub struct srd_decoder_inst {
    pub decoder: *mut srd_decoder,
    pub sess: *mut c_void,
    pub py_inst: *mut c_void,
    pub inst_id: *const c_char,
}

#[repr(C)]
pub struct srd_pd_output {
    pub pdo_id: c_int,
    pub output_type: c_int,
    pub di: *mut srd_decoder_inst,
    pub proto_id: *const c_char,
}

#[repr(C)]
pub struct srd_proto_data {
    pub start_sample: u64,
    pub end_sample: u64,
    pub pdo: *mut srd_pd_output,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct srd_proto_data_annotation {
    pub ann_class: c_int,
    /// NULL-terminated array, longest text first.
    pub ann_text: *mut *const c_char,
}

pub const SRD_OK: c_int = 0;
pub const SRD_OUTPUT_ANN: c_int = 0;
pub const SRD_CONF_SAMPLERATE: c_int = 10000;
pub const SRD_LOG_WARN: c_int = 2;

pub type PdOutputCb = unsafe extern "C" fn(*mut srd_proto_data, *mut c_void);
/// The last argument is a `va_list`, which is a plain pointer on Windows x64.
pub type LogCb = unsafe extern "C" fn(*mut c_void, c_int, *const c_char, *mut c_void) -> c_int;
pub type GHashFunc = unsafe extern "C" fn(*const c_void) -> u32;
pub type GEqualFunc = unsafe extern "C" fn(*const c_void, *const c_void) -> gboolean;
pub type GDestroyNotify = unsafe extern "C" fn(*mut c_void);

macro_rules! api {
    ($($lib:ident { $($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)* })*) => {
        /// Function pointers resolved from the loaded DLLs.
        pub struct Api {
            $($(pub $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*)*
            _libs: Vec<libloading::os::windows::Library>,
        }

        impl Api {
            /// Load `libsigrokdecode-4.dll` and `libglib-2.0-0.dll` from `dir`.
            ///
            /// # Safety
            /// The DLLs must be the libsigrokdecode build these layouts match.
            pub unsafe fn load(dir: &Path) -> Result<Api, String> {
                use libloading::os::windows::{Library, LOAD_WITH_ALTERED_SEARCH_PATH};
                let open = |file: &str| -> Result<Library, String> {
                    let p = dir.join(file);
                    // Resolve the DLL's own dependencies next to it.
                    unsafe { Library::load_with_flags(&p, LOAD_WITH_ALTERED_SEARCH_PATH) }
                        .map_err(|e| format!("{}: {e}", p.display()))
                };
                $(let $lib = open(lib_file!($lib))?;)*
                Ok(Api {
                    $($($name: unsafe {
                        *$lib
                            .get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                                concat!(stringify!($name), "\0").as_bytes(),
                            )
                            .map_err(|e| format!("{}: {e}", stringify!($name)))?
                    },)*)*
                    _libs: vec![$($lib),*],
                })
            }
        }
    };
}

macro_rules! lib_file {
    (glib) => {
        "libglib-2.0-0.dll"
    };
    (srd) => {
        "libsigrokdecode-4.dll"
    };
}

api! {
    glib {
        g_free: fn(*mut c_void);
        g_strdup: fn(*const c_char) -> *mut c_char;
        g_strdup_vprintf: fn(*const c_char, *mut c_void) -> *mut c_char;
        g_str_hash: fn(*const c_void) -> u32;
        g_str_equal: fn(*const c_void, *const c_void) -> gboolean;
        g_hash_table_new_full: fn(GHashFunc, GEqualFunc, Option<GDestroyNotify>, Option<GDestroyNotify>) -> *mut c_void;
        g_hash_table_insert: fn(*mut c_void, *mut c_void, *mut c_void) -> gboolean;
        g_hash_table_destroy: fn(*mut c_void);
        g_variant_new_string: fn(*const c_char) -> *mut c_void;
        g_variant_new_int64: fn(i64) -> *mut c_void;
        g_variant_new_int32: fn(i32) -> *mut c_void;
        g_variant_new_uint64: fn(u64) -> *mut c_void;
        g_variant_new_double: fn(f64) -> *mut c_void;
        g_variant_ref_sink: fn(*mut c_void) -> *mut c_void;
        g_variant_unref: fn(*mut c_void);
        g_variant_get_type_string: fn(*mut c_void) -> *const c_char;
        g_variant_get_string: fn(*mut c_void, *mut usize) -> *const c_char;
        g_variant_get_int64: fn(*mut c_void) -> i64;
        g_variant_get_double: fn(*mut c_void) -> f64;
    }
    srd {
        srd_init: fn(*const c_char) -> c_int;
        srd_strerror: fn(c_int) -> *const c_char;
        srd_log_callback_set: fn(LogCb, *mut c_void) -> c_int;
        srd_log_loglevel_set: fn(c_int) -> c_int;
        srd_decoder_load_all: fn() -> c_int;
        srd_decoder_load: fn(*const c_char) -> c_int;
        srd_decoder_list: fn() -> *const GSList;
        srd_decoder_get_by_id: fn(*const c_char) -> *mut srd_decoder;
        srd_session_new: fn(*mut *mut c_void) -> c_int;
        srd_session_start: fn(*mut c_void) -> c_int;
        srd_session_metadata_set: fn(*mut c_void, c_int, *mut c_void) -> c_int;
        srd_session_send: fn(*mut c_void, u64, u64, *const u8, u64, u64) -> c_int;
        srd_session_send_eof: fn(*mut c_void) -> c_int;
        srd_session_terminate_reset: fn(*mut c_void) -> c_int;
        srd_session_destroy: fn(*mut c_void) -> c_int;
        srd_pd_output_callback_add: fn(*mut c_void, c_int, PdOutputCb, *mut c_void) -> c_int;
        srd_inst_new: fn(*mut c_void, *const c_char, *mut c_void) -> *mut srd_decoder_inst;
        srd_inst_channel_set_all: fn(*mut srd_decoder_inst, *mut c_void) -> c_int;
        srd_inst_stack: fn(*mut c_void, *mut srd_decoder_inst, *mut srd_decoder_inst) -> c_int;
    }
}

/// Iterate a GSList's data pointers.
///
/// # Safety
/// `l` must be a valid list (or NULL).
pub unsafe fn slist(mut l: *const GSList) -> impl Iterator<Item = *mut c_void> {
    std::iter::from_fn(move || {
        if l.is_null() {
            return None;
        }
        let item = unsafe { &*l };
        l = item.next;
        Some(item.data)
    })
}

/// # Safety
/// `p` must be NULL or a NUL-terminated string.
pub unsafe fn string(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned()
    }
}
