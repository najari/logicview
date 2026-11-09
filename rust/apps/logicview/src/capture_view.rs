//! An opened capture: zoom summaries in memory, raw chunks on demand.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use lv_store::summary::{Entry, LEVEL_BLOCK, MAX_LEVEL};
use lv_store::{CHUNK_SAMPLES, CaptureFile, CaptureInfo};

/// Raw chunks kept decompressed (16 MiB).
const CACHE_CHUNKS: usize = 16;

pub struct CaptureView {
    file: CaptureFile,
    /// `levels[l]` for l = 1..=3; index 0 unused (raw data comes from chunks).
    levels: [Vec<Entry>; MAX_LEVEL + 1],
    cache: VecDeque<(usize, Arc<Vec<u8>>)>,
}

impl CaptureView {
    pub fn open(path: &Path) -> lv_store::Result<Self> {
        let file = CaptureFile::open(path)?;
        let mut levels: [Vec<Entry>; MAX_LEVEL + 1] = Default::default();
        for (l, level) in levels.iter_mut().enumerate().skip(1) {
            *level = file.summary(l)?;
        }
        Ok(Self {
            file,
            levels,
            cache: VecDeque::new(),
        })
    }

    pub fn info(&self) -> &CaptureInfo {
        &self.file.info
    }

    pub fn path(&self) -> &Path {
        self.file.path()
    }

    pub fn num_samples(&self) -> u64 {
        self.file.info.num_samples
    }

    pub fn level(&self, l: usize) -> &[Entry] {
        &self.levels[l]
    }

    fn chunk(&mut self, idx: usize) -> lv_store::Result<Arc<Vec<u8>>> {
        if let Some(pos) = self.cache.iter().position(|(i, _)| *i == idx) {
            let item = self.cache.remove(pos).expect("cached chunk");
            let data = item.1.clone();
            self.cache.push_front(item);
            return Ok(data);
        }
        let data = Arc::new(self.file.read_chunk(idx)?);
        self.cache.push_front((idx, data.clone()));
        self.cache.truncate(CACHE_CHUNKS);
        Ok(data)
    }

    /// Raw samples `[start, end)`, clipped to the capture.
    pub fn raw(&mut self, start: u64, end: u64) -> lv_store::Result<Vec<u8>> {
        let end = end.min(self.num_samples());
        let mut out = Vec::with_capacity(end.saturating_sub(start) as usize);
        let mut pos = start;
        while pos < end {
            let idx = (pos / CHUNK_SAMPLES as u64) as usize;
            let chunk = self.chunk(idx)?;
            let off = (pos % CHUNK_SAMPLES as u64) as usize;
            let take = ((end - pos) as usize).min(chunk.len() - off);
            out.extend_from_slice(&chunk[off..off + take]);
            pos += take as u64;
        }
        Ok(out)
    }

    /// The coarsest level whose entries are no wider than `samples_per_px`
    /// (UI-3). 0 means raw samples.
    pub fn level_for(samples_per_px: f64) -> usize {
        (0..=MAX_LEVEL)
            .rev()
            .find(|&l| LEVEL_BLOCK[l] as f64 <= samples_per_px)
            .unwrap_or(0)
    }
}
