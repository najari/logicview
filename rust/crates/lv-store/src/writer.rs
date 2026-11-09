//! Writing a capture while it is recorded (STO-1..3).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};

use crate::schema::{FORMAT, SCHEMA, VERSION, now_rfc3339};
use crate::summary::{self, Entry, LEVEL_FACTOR};
use crate::{CHUNK_SAMPLES, CHUNKS_PER_COMMIT, CaptureInfo, Error, NewCapture, Result, Status};

#[derive(Debug, Clone, Default)]
pub struct WriteStats {
    pub raw_bytes: u64,
    pub stored_bytes: u64,
    pub chunks: u64,
    pub compress_time: Duration,
    pub db_time: Duration,
}

impl WriteStats {
    pub fn ratio(&self) -> f64 {
        if self.stored_bytes == 0 {
            0.0
        } else {
            self.raw_bytes as f64 / self.stored_bytes as f64
        }
    }
}

pub struct CaptureWriter {
    conn: Connection,
    path: PathBuf,
    id: i64,
    info: CaptureInfo,
    pending: Vec<u8>,
    chunk_idx: i64,
    total: u64,
    l1: Vec<Entry>,
    uncommitted: usize,
    compressor: zstd::bulk::Compressor<'static>,
    stats: WriteStats,
}

fn remove_db_files(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", path.display()));
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| Error::Io(p.clone(), e))?;
        }
    }
    Ok(())
}

impl CaptureWriter {
    pub fn create(path: &Path, cap: &NewCapture, overwrite: bool) -> Result<Self> {
        if path.exists() {
            if !overwrite {
                return Err(Error::Exists(path.to_path_buf()));
            }
            remove_db_files(path)?;
        }
        let conn = Connection::open(path)?;
        // Large pages suit 1 MiB blobs; must be set before the first table.
        conn.execute_batch("PRAGMA page_size = 65536;")?;
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        let app = format!("logicview {}", env!("CARGO_PKG_VERSION"));
        for (k, v) in [
            ("format", FORMAT.to_string()),
            ("version", VERSION.to_string()),
            ("app", app),
        ] {
            conn.execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)",
                params![k, v],
            )?;
        }
        let created_at = now_rfc3339();
        conn.execute(
            "INSERT INTO capture(id, created_at, device, samplerate_hz, num_channels, num_samples, status)
             VALUES (1, ?1, ?2, ?3, ?4, 0, 'recording')",
            params![created_at, cap.device, cap.samplerate_hz as i64, cap.channel_names.len() as i64],
        )?;
        for (i, name) in cap.channel_names.iter().enumerate() {
            conn.execute(
                "INSERT INTO channel(capture_id, idx, name) VALUES (1, ?1, ?2)",
                params![i as i64, name],
            )?;
        }
        conn.execute_batch("BEGIN")?;
        let compressor =
            zstd::bulk::Compressor::new(1).map_err(|e| Error::Io(path.to_path_buf(), e))?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
            id: 1,
            info: CaptureInfo {
                id: 1,
                created_at,
                device: cap.device.clone(),
                samplerate_hz: cap.samplerate_hz,
                num_channels: cap.channel_names.len(),
                num_samples: 0,
                status: Status::Recording,
                channel_names: cap.channel_names.clone(),
            },
            pending: Vec::with_capacity(CHUNK_SAMPLES),
            chunk_idx: 0,
            total: 0,
            l1: Vec::new(),
            uncommitted: 0,
            compressor,
            stats: WriteStats::default(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Samples accepted so far (written or waiting for a full chunk).
    pub fn samples(&self) -> u64 {
        self.total
    }

    pub fn stats(&self) -> &WriteStats {
        &self.stats
    }

    /// Append samples (one byte each).
    pub fn push(&mut self, mut data: &[u8]) -> Result<()> {
        self.total += data.len() as u64;
        while !data.is_empty() {
            let room = CHUNK_SAMPLES - self.pending.len();
            let (now, rest) = data.split_at(room.min(data.len()));
            self.pending.extend_from_slice(now);
            data = rest;
            if self.pending.len() == CHUNK_SAMPLES {
                self.write_chunk()?;
            }
        }
        Ok(())
    }

    fn write_chunk(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let n = self.pending.len();
        let first = self.chunk_idx as u64 * CHUNK_SAMPLES as u64;
        let t = Instant::now();
        let packed = self
            .compressor
            .compress(&self.pending)
            .map_err(|e| Error::Io(self.path.clone(), e))?;
        let (codec, blob): (&str, &[u8]) = if packed.len() < n {
            ("zstd", &packed)
        } else {
            ("raw", &self.pending)
        };
        let l1 = summary::summarize_raw(&self.pending, LEVEL_FACTOR);
        self.stats.compress_time += t.elapsed();

        let t = Instant::now();
        self.conn
            .prepare_cached(
                "INSERT INTO sample_chunk(capture_id, chunk_idx, first_sample, n_samples, codec, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![self.id, self.chunk_idx, first as i64, n as i64, codec, blob])?;
        self.conn
            .prepare_cached(
                "INSERT INTO summary(capture_id, level, chunk_idx, data) VALUES (?1, 1, ?2, ?3)",
            )?
            .execute(params![self.id, self.chunk_idx, summary::to_blob(&l1)])?;
        self.uncommitted += 1;
        if self.uncommitted >= CHUNKS_PER_COMMIT {
            self.conn.execute_batch("COMMIT; BEGIN")?;
            self.uncommitted = 0;
        }
        self.stats.db_time += t.elapsed();

        self.stats.raw_bytes += n as u64;
        self.stats.stored_bytes += blob.len() as u64;
        self.stats.chunks += 1;
        self.l1.extend(l1);
        self.chunk_idx += 1;
        self.pending.clear();
        Ok(())
    }

    /// Write what is left, build the coarse summaries and close the file.
    pub fn finish(mut self, status: Status) -> Result<(CaptureInfo, WriteStats)> {
        self.write_chunk()?;
        let t = Instant::now();
        let mut level = self.l1.clone();
        for lv in 2..=summary::MAX_LEVEL as i64 {
            level = summary::combine(&level, LEVEL_FACTOR);
            self.conn.execute(
                "INSERT INTO summary(capture_id, level, chunk_idx, data) VALUES (?1, ?2, 0, ?3)",
                params![self.id, lv, summary::to_blob(&level)],
            )?;
        }
        self.conn.execute(
            "UPDATE capture SET num_samples = ?1, status = ?2 WHERE id = ?3",
            params![self.total as i64, status.as_str(), self.id],
        )?;
        self.conn.execute_batch("COMMIT")?;
        // Fold the WAL back in so the capture is a single file again. This
        // needs exclusive access; if a viewer still has the file open, the
        // WAL stays and SQLite folds it in later.
        if let Err(e) = self
            .conn
            .query_row("PRAGMA journal_mode = DELETE", [], |r| {
                r.get::<_, String>(0)
            })
        {
            log::info!("{}: keeping WAL ({e})", self.path.display());
            let _ = self.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
        }
        self.stats.db_time += t.elapsed();
        self.info.num_samples = self.total;
        self.info.status = status;
        Ok((self.info, self.stats))
    }
}
