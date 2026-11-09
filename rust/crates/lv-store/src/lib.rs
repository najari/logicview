//! `.lvcap` capture files: one SQLite database per capture (STO-1..3, STO-7).

pub mod export;
mod reader;
mod schema;
pub mod sigrok;
pub mod summary;
mod writer;

use std::path::PathBuf;

pub use reader::{CaptureFile, ChunkMeta};
pub use schema::{FORMAT, VERSION, now_rfc3339};
pub use writer::{CaptureWriter, WriteStats};

/// Raw samples per stored chunk (1 MiB at one byte per sample).
pub const CHUNK_SAMPLES: usize = 1 << 20;
/// Chunks written per transaction while recording.
pub const CHUNKS_PER_COMMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Recording,
    Complete,
    Aborted,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Recording => "recording",
            Status::Complete => "complete",
            Status::Aborted => "aborted",
        }
    }

    pub fn parse(s: &str) -> Status {
        match s {
            "complete" => Status::Complete,
            "aborted" => Status::Aborted,
            _ => Status::Recording,
        }
    }
}

/// What a new capture file records about itself.
#[derive(Debug, Clone)]
pub struct NewCapture {
    pub device: String,
    pub samplerate_hz: u64,
    pub channel_names: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CaptureInfo {
    pub id: i64,
    pub created_at: String,
    pub device: String,
    pub samplerate_hz: u64,
    pub num_channels: usize,
    pub num_samples: u64,
    pub status: Status,
    pub channel_names: Vec<String>,
    /// `channel.enabled`: shown in the viewer (UI-6).
    pub channel_enabled: Vec<bool>,
    /// `capture.trigger_json`: the trigger and where it fired (CAP-8).
    pub trigger: Option<String>,
}

impl CaptureInfo {
    pub fn duration_secs(&self) -> f64 {
        self.num_samples as f64 / self.samplerate_hz.max(1) as f64
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0} already exists; choose another name or allow overwriting")]
    Exists(PathBuf),
    #[error("{0} is not a logicview capture (.lvcap) file")]
    NotLvcap(PathBuf),
    #[error(
        "{path} uses format version {version}; this logicview reads up to version {VERSION}. Update logicview to open it"
    )]
    NewerVersion { path: PathBuf, version: i64 },
    #[error("capture file is damaged: {0}")]
    Corrupt(String),
    #[error("{path}: {msg}")]
    Format { path: PathBuf, msg: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Fold a leftover WAL back into the capture so it is one file again. The
/// writer does this itself unless a reader had the file open at the end.
pub fn fold_wal(path: &std::path::Path) -> Result<()> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    let _: String = conn.query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))?;
    Ok(())
}

/// Rename channels and choose which are shown (UI-6) in a capture file
/// that is not being recorded.
pub fn update_channels(path: &std::path::Path, names: &[String], enabled: &[bool]) -> Result<()> {
    let mut conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    let tx = conn.transaction()?;
    for (i, (name, on)) in names.iter().zip(enabled).enumerate() {
        tx.execute(
            "UPDATE channel SET name = ?1, enabled = ?2
             WHERE capture_id = (SELECT id FROM capture ORDER BY id LIMIT 1) AND idx = ?3",
            rusqlite::params![name, *on as i64, i as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}
