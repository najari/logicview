//! Reading capture files (STO-4, STO-7).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::schema::{FORMAT, VERSION};
use crate::summary::{self, Entry, LEVEL_FACTOR, MAX_LEVEL};
use crate::{CHUNK_SAMPLES, CaptureInfo, Error, Result, Status};

#[derive(Debug, Clone, Copy)]
pub struct ChunkMeta {
    pub first_sample: u64,
    pub n_samples: u64,
}

pub struct CaptureFile {
    conn: Connection,
    path: PathBuf,
    pub info: CaptureInfo,
    chunks: Vec<ChunkMeta>,
}

impl CaptureFile {
    /// Open read-only. Works while the file is still being recorded.
    pub fn open(path: &Path) -> Result<Self> {
        if let Err(e) = std::fs::metadata(path) {
            return Err(Error::Io(path.to_path_buf(), e));
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let meta = |key: &str| -> Result<Option<String>> {
            conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                .optional()
                .map_err(|_| Error::NotLvcap(path.to_path_buf()))
        };
        if meta("format")?.as_deref() != Some(FORMAT) {
            return Err(Error::NotLvcap(path.to_path_buf()));
        }
        let version: i64 = meta("version")?
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| Error::NotLvcap(path.to_path_buf()))?;
        if version > VERSION {
            return Err(Error::NewerVersion {
                path: path.to_path_buf(),
                version,
            });
        }
        let mut file = Self {
            conn,
            path: path.to_path_buf(),
            info: CaptureInfo {
                id: 0,
                created_at: String::new(),
                device: String::new(),
                samplerate_hz: 0,
                num_channels: 0,
                num_samples: 0,
                status: Status::Recording,
                channel_names: Vec::new(),
                channel_enabled: Vec::new(),
                trigger: None,
            },
            chunks: Vec::new(),
        };
        file.refresh()?;
        Ok(file)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Re-read the capture row and chunk list (for a file still recording).
    pub fn refresh(&mut self) -> Result<()> {
        let mut info = self.conn.query_row(
            "SELECT id, created_at, device, samplerate_hz, num_channels, num_samples, status,
                    trigger_json
             FROM capture ORDER BY id LIMIT 1",
            [],
            |r| {
                Ok(CaptureInfo {
                    id: r.get(0)?,
                    created_at: r.get(1)?,
                    device: r.get(2)?,
                    samplerate_hz: r.get::<_, i64>(3)? as u64,
                    num_channels: r.get::<_, i64>(4)? as usize,
                    num_samples: r.get::<_, i64>(5)? as u64,
                    status: Status::parse(&r.get::<_, String>(6)?),
                    channel_names: Vec::new(),
                    channel_enabled: Vec::new(),
                    trigger: r.get(7)?,
                })
            },
        )?;
        let mut stmt = self
            .conn
            .prepare("SELECT name, enabled FROM channel WHERE capture_id = ?1 ORDER BY idx")?;
        let channels: Vec<(String, bool)> = stmt
            .query_map([info.id], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)))?
            .collect::<std::result::Result<_, _>>()?;
        (info.channel_names, info.channel_enabled) = channels.into_iter().unzip();
        let mut stmt = self.conn.prepare(
            "SELECT first_sample, n_samples FROM sample_chunk WHERE capture_id = ?1 ORDER BY chunk_idx",
        )?;
        self.chunks = stmt
            .query_map([info.id], |r| {
                Ok(ChunkMeta {
                    first_sample: r.get::<_, i64>(0)? as u64,
                    n_samples: r.get::<_, i64>(1)? as u64,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        if info.status == Status::Recording {
            // num_samples is only final at the end; count what is stored.
            info.num_samples = self
                .chunks
                .last()
                .map(|c| c.first_sample + c.n_samples)
                .unwrap_or(0);
        }
        self.info = info;
        Ok(())
    }

    pub fn chunks(&self) -> &[ChunkMeta] {
        &self.chunks
    }

    /// Bytes the stored sample chunks take.
    pub fn stored_bytes(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(length(data)), 0) FROM sample_chunk WHERE capture_id = ?1",
            [self.info.id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Decompressed samples of one chunk.
    pub fn read_chunk(&self, idx: usize) -> Result<Vec<u8>> {
        let (codec, n, data): (String, i64, Vec<u8>) = self
            .conn
            .prepare_cached(
                "SELECT codec, n_samples, data FROM sample_chunk WHERE capture_id = ?1 AND chunk_idx = ?2",
            )?
            .query_row(params![self.info.id, idx as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
        let out = match codec.as_str() {
            "raw" => data,
            "zstd" => zstd::bulk::decompress(&data, n as usize)
                .map_err(|e| Error::Corrupt(format!("chunk {idx}: {e}")))?,
            other => {
                return Err(Error::Corrupt(format!(
                    "chunk {idx}: unknown codec {other}"
                )));
            }
        };
        if out.len() as i64 != n {
            return Err(Error::Corrupt(format!(
                "chunk {idx}: {} samples instead of {n}",
                out.len()
            )));
        }
        Ok(out)
    }

    /// Write a self-contained copy of the capture to `dest` (`VACUUM INTO`),
    /// including anything still in the WAL. `dest` must not exist.
    pub fn save_copy(&self, dest: &Path) -> Result<()> {
        if dest.exists() {
            return Err(Error::Exists(dest.to_path_buf()));
        }
        let dest_str = dest.to_str().ok_or_else(|| Error::Format {
            path: dest.to_path_buf(),
            msg: "path is not valid Unicode".into(),
        })?;
        self.conn.execute("VACUUM INTO ?1", [dest_str])?;
        Ok(())
    }

    /// Visit every chunk in order with its first sample index.
    pub fn for_each_chunk(&self, mut f: impl FnMut(u64, &[u8]) -> Result<()>) -> Result<()> {
        for (idx, meta) in self.chunks.iter().enumerate() {
            let data = self.read_chunk(idx)?;
            f(meta.first_sample, &data)?;
        }
        Ok(())
    }

    /// Samples `[start, start + len)`, clipped to the capture.
    pub fn read_samples(&self, start: u64, len: u64) -> Result<Vec<u8>> {
        let end = (start + len).min(self.info.num_samples);
        let mut out = Vec::with_capacity(end.saturating_sub(start) as usize);
        let mut pos = start;
        while pos < end {
            let idx = (pos / CHUNK_SAMPLES as u64) as usize;
            let chunk = self.read_chunk(idx)?;
            let off = (pos % CHUNK_SAMPLES as u64) as usize;
            let take = ((end - pos) as usize).min(chunk.len() - off);
            out.extend_from_slice(&chunk[off..off + take]);
            pos += take as u64;
        }
        Ok(out)
    }

    /// Level-1 summary entries of chunks `from_chunk..`, in sample order
    /// (to extend a copy while recording, UI-8).
    pub fn summary_l1_from(&self, from_chunk: usize) -> Result<Vec<Entry>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT data FROM summary WHERE capture_id = ?1 AND level = 1 AND chunk_idx >= ?2
             ORDER BY chunk_idx",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![self.info.id, from_chunk as i64])?;
        while let Some(r) = rows.next()? {
            let blob: Vec<u8> = r.get(0)?;
            out.extend(summary::from_blob(&blob));
        }
        Ok(out)
    }

    /// All entries of a summary level (1..=3), in sample order.
    pub fn summary(&self, level: usize) -> Result<Vec<Entry>> {
        assert!((1..=MAX_LEVEL).contains(&level), "summary level {level}");
        let mut stmt = self.conn.prepare_cached(
            "SELECT data FROM summary WHERE capture_id = ?1 AND level = ?2 ORDER BY chunk_idx",
        )?;
        let mut out = Vec::new();
        let mut rows = stmt.query(params![self.info.id, level as i64])?;
        while let Some(r) = rows.next()? {
            let blob: Vec<u8> = r.get(0)?;
            out.extend(summary::from_blob(&blob));
        }
        if out.is_empty() && level > 1 {
            // Still recording: build it from the level below.
            return Ok(summary::combine(&self.summary(level - 1)?, LEVEL_FACTOR));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::{ChannelState, channel_state, summarize_raw};
    use crate::{CaptureWriter, NewCapture};

    fn pattern(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| {
                let mut b = 0u8;
                for ch in 0..8 {
                    if (i >> (ch + 1)) & 1 == 1 {
                        b |= 1 << ch;
                    }
                }
                b
            })
            .collect()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("lv-store-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn sto1_write_then_read_back() {
        let path = temp("roundtrip.lvcap");
        let cap = NewCapture {
            device: "test".into(),
            samplerate_hz: 24_000_000,
            channel_names: (0..8).map(|i| format!("D{i}")).collect(),
        };
        let n = CHUNK_SAMPLES * 2 + 12_345;
        let data = pattern(n);
        let mut w = CaptureWriter::create(&path, &cap, true).unwrap();
        // Odd push sizes cross chunk borders.
        for part in data.chunks(240_128) {
            w.push(part).unwrap();
        }
        let (info, stats) = w.finish(Status::Complete).unwrap();
        assert_eq!(info.num_samples, n as u64);
        assert_eq!(stats.chunks, 3);
        assert!(
            stats.ratio() > 10.0,
            "pattern compresses well: {}",
            stats.ratio()
        );
        assert!(!PathBuf::from(format!("{}-wal", path.display())).exists());

        let f = CaptureFile::open(&path).unwrap();
        assert_eq!(f.info.num_samples, n as u64);
        assert_eq!(f.info.status, Status::Complete);
        assert_eq!(f.info.channel_names[3], "D3");
        assert_eq!(f.chunks().len(), 3);
        assert_eq!(f.read_samples(0, n as u64).unwrap(), data);
        let mid = CHUNK_SAMPLES as u64 - 100;
        assert_eq!(
            f.read_samples(mid, 300).unwrap(),
            data[mid as usize..mid as usize + 300]
        );
        // Summaries match a direct computation.
        assert_eq!(f.summary(1).unwrap(), summarize_raw(&data, 64));
        assert_eq!(f.summary(2).unwrap(), summarize_raw(&data, 4096));
        let l3 = f.summary(3).unwrap();
        assert_eq!(l3, summarize_raw(&data, 262_144));
        assert_eq!(channel_state(l3[0], 0), ChannelState::Changing);
    }

    #[test]
    fn sto4_read_while_recording() {
        let path = temp("recording.lvcap");
        let cap = NewCapture {
            device: "test".into(),
            samplerate_hz: 1_000_000,
            channel_names: (0..8).map(|i| format!("D{i}")).collect(),
        };
        let mut w = CaptureWriter::create(&path, &cap, true).unwrap();
        w.push(&pattern(CHUNK_SAMPLES * crate::CHUNKS_PER_COMMIT + 10))
            .unwrap();
        let mut f = CaptureFile::open(&path).unwrap();
        assert_eq!(f.info.status, Status::Recording);
        // Whole chunks are visible: all of them, or fewer when a slow
        // (debug) build passed the 250 ms live commit in between.
        let n = f.info.num_samples;
        assert!(
            n > 0 && n.is_multiple_of(CHUNK_SAMPLES as u64),
            "{n} samples visible"
        );
        assert!(n <= (CHUNK_SAMPLES * crate::CHUNKS_PER_COMMIT) as u64);
        assert_eq!(f.summary(3).unwrap().len() as u64, n / 262_144);
        w.finish(Status::Complete).unwrap();
        f.refresh().unwrap();
        assert_eq!(f.info.status, Status::Complete);
    }

    #[test]
    fn ui8_partial_chunk_is_visible_while_recording() {
        let path = temp("live.lvcap");
        let cap = NewCapture {
            device: "test".into(),
            samplerate_hz: 20_000,
            channel_names: (0..8).map(|i| format!("D{i}")).collect(),
        };
        let mut w = CaptureWriter::create(&path, &cap, true).unwrap();
        let data = pattern(3000);
        w.push(&data[..1000]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        w.push(&data[1000..2000]).unwrap(); // writes the part-filled chunk
        let mut f = CaptureFile::open(&path).unwrap();
        assert_eq!(f.info.num_samples, 2000);
        assert_eq!(f.read_samples(0, 2000).unwrap(), data[..2000]);
        assert_eq!(f.summary(1).unwrap().len(), 2000usize.div_ceil(64));
        std::thread::sleep(std::time::Duration::from_millis(300));
        w.push(&data[2000..]).unwrap();
        f.refresh().unwrap();
        assert_eq!(f.info.num_samples, 3000);
        let (info, stats) = w.finish(Status::Complete).unwrap();
        assert_eq!(info.num_samples, 3000);
        assert_eq!(
            stats.raw_bytes, 3000,
            "partial writes are not counted twice"
        );
        f.refresh().unwrap();
        assert_eq!(f.chunks().len(), 1);
        assert_eq!(f.read_samples(0, 3000).unwrap(), data);
        // The reader kept the WAL alive; once it closes, it can be folded in.
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        assert!(wal.exists());
        drop(f);
        crate::fold_wal(&path).unwrap();
        assert!(!wal.exists());
        let g = CaptureFile::open(&path).unwrap();
        assert_eq!(g.read_samples(0, 3000).unwrap(), data);
    }

    #[test]
    fn ui6_save_copy_and_channels() {
        let path = temp("copy-src.lvcap");
        let cap = NewCapture {
            device: "test".into(),
            samplerate_hz: 1_000_000,
            channel_names: (0..8).map(|i| format!("D{i}")).collect(),
        };
        let mut w = CaptureWriter::create(&path, &cap, true).unwrap();
        let data = pattern(CHUNK_SAMPLES + 5);
        w.push(&data).unwrap();
        w.finish(Status::Complete).unwrap();

        let f = CaptureFile::open(&path).unwrap();
        let dest = temp("copy-dest.lvcap");
        let _ = std::fs::remove_file(&dest);
        f.save_copy(&dest).unwrap();
        assert!(matches!(f.save_copy(&dest), Err(Error::Exists(_))));
        drop(f);

        let mut names: Vec<String> = (0..8).map(|i| format!("D{i}")).collect();
        names[0] = "TX 송신".into();
        let mut enabled = vec![true; 8];
        enabled[5] = false;
        crate::update_channels(&dest, &names, &enabled).unwrap();
        let g = CaptureFile::open(&dest).unwrap();
        assert_eq!(g.read_samples(0, data.len() as u64).unwrap(), data);
        assert_eq!(g.info.channel_names, names);
        assert_eq!(g.info.channel_enabled, enabled);
    }

    #[test]
    fn sto7_rejects_other_files() {
        let path = temp("not-lvcap.db");
        let _ = std::fs::remove_file(&path);
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE x(y)").unwrap();
        drop(c);
        assert!(matches!(CaptureFile::open(&path), Err(Error::NotLvcap(_))));

        let path = temp("newer.lvcap");
        let _ = std::fs::remove_file(&path);
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta VALUES ('format','lvcap'), ('version','99');",
        )
        .unwrap();
        drop(c);
        assert!(matches!(
            CaptureFile::open(&path),
            Err(Error::NewerVersion { version: 99, .. })
        ));
    }
}
