//! Exports in Saleae Logic 2 formats (STO-6), the same files the MCP tools
//! `export_raw_data_csv` and `export_raw_data_binary` write:
//!
//! - CSV: `Time [s],Channel 0,...`, one row at sample 0 and one at every
//!   change of the exported channels.
//! - Binary v0: one `digital_N.bin` per channel: `<SALEAE>`, version 0,
//!   type 0, initial state, begin and end time, transition count, then the
//!   time of every transition (f64 seconds).

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::{CaptureFile, Error, Result};

/// Channels to export: the given ones, or every channel of the capture.
fn pick_channels(f: &CaptureFile, channels: &[usize]) -> Result<Vec<usize>> {
    let n = f.info.num_channels;
    if channels.is_empty() {
        return Ok((0..n).collect());
    }
    if let Some(bad) = channels.iter().find(|&&c| c >= n) {
        return Err(Error::Format {
            path: f.path().to_path_buf(),
            msg: format!("channel {bad} does not exist; the capture has channels 0..{n}"),
        });
    }
    Ok(channels.to_vec())
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::Io(path.to_path_buf(), e)
}

/// Write a CSV of the selected channels. Returns the number of data rows.
pub fn csv(f: &CaptureFile, out: &Path, channels: &[usize]) -> Result<u64> {
    let chans = pick_channels(f, channels)?;
    let mask = chans.iter().fold(0u8, |m, &c| m | 1 << c);
    let rate = f.info.samplerate_hz.max(1) as f64;
    let file = File::create(out).map_err(io(out))?;
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let mut header = String::from("Time [s]");
    for c in &chans {
        header.push_str(&format!(",Channel {c}"));
    }
    header.push('\n');
    w.write_all(header.as_bytes()).map_err(io(out))?;

    let mut rows = 0u64;
    let mut prev: Option<u8> = None;
    let mut line = String::with_capacity(64);
    f.for_each_chunk(|first, data| {
        for (i, &b) in data.iter().enumerate() {
            let v = b & mask;
            if prev == Some(v) {
                continue;
            }
            prev = Some(v);
            line.clear();
            line.push_str(&format!("{:.9}", (first + i as u64) as f64 / rate));
            for &c in &chans {
                line.push(',');
                line.push(if v >> c & 1 == 1 { '1' } else { '0' });
            }
            line.push('\n');
            w.write_all(line.as_bytes()).map_err(io(out))?;
            rows += 1;
        }
        Ok(())
    })?;
    w.flush().map_err(io(out))?;
    Ok(rows)
}

/// Write the raw samples, one byte each (like `sigrok-cli -O binary`).
pub fn raw(f: &CaptureFile, out: &Path) -> Result<u64> {
    let mut w = BufWriter::with_capacity(1 << 20, File::create(out).map_err(io(out))?);
    f.for_each_chunk(|_, data| w.write_all(data).map_err(io(out)))?;
    w.flush().map_err(io(out))?;
    Ok(f.info.num_samples)
}

/// Offset of the transition count in a binary v0 file.
const COUNT_OFFSET: u64 = 8 + 4 + 4 + 4 + 8 + 8;

/// Write `digital_N.bin` for the selected channels into `dir`. Returns the
/// files and their transition counts.
pub fn saleae_binary(
    f: &CaptureFile,
    dir: &Path,
    channels: &[usize],
) -> Result<Vec<(PathBuf, u64)>> {
    let chans = pick_channels(f, channels)?;
    std::fs::create_dir_all(dir).map_err(io(dir))?;
    let rate = f.info.samplerate_hz.max(1) as f64;
    let end_time = f.info.num_samples as f64 / rate;
    let initial = if f.info.num_samples > 0 {
        f.read_samples(0, 1)?[0]
    } else {
        0
    };

    struct Out {
        path: PathBuf,
        bit: u8,
        w: BufWriter<File>,
        count: u64,
    }
    let mut outs = Vec::new();
    for &c in &chans {
        let path = dir.join(format!("digital_{c}.bin"));
        let mut w = BufWriter::with_capacity(1 << 16, File::create(&path).map_err(io(&path))?);
        let state = u32::from(initial >> c & 1);
        let mut head = Vec::with_capacity(COUNT_OFFSET as usize + 8);
        head.extend_from_slice(b"<SALEAE>");
        head.extend_from_slice(&0i32.to_le_bytes()); // version
        head.extend_from_slice(&0i32.to_le_bytes()); // type: digital
        head.extend_from_slice(&state.to_le_bytes());
        head.extend_from_slice(&0f64.to_le_bytes()); // begin time
        head.extend_from_slice(&end_time.to_le_bytes());
        head.extend_from_slice(&0u64.to_le_bytes()); // count, filled in below
        w.write_all(&head).map_err(io(&path))?;
        outs.push(Out {
            path,
            bit: 1 << c,
            w,
            count: 0,
        });
    }

    let mut prev = initial;
    f.for_each_chunk(|first, data| {
        for (i, &b) in data.iter().enumerate() {
            let changed = b ^ prev;
            if changed == 0 {
                continue;
            }
            prev = b;
            let t = (first + i as u64) as f64 / rate;
            for o in outs.iter_mut().filter(|o| changed & o.bit != 0) {
                o.w.write_all(&t.to_le_bytes()).map_err(io(&o.path))?;
                o.count += 1;
            }
        }
        Ok(())
    })?;

    let mut done = Vec::new();
    for o in outs {
        let path = o.path;
        let mut file =
            o.w.into_inner()
                .map_err(|e| Error::Io(path.clone(), e.into_error()))?;
        file.seek(SeekFrom::Start(COUNT_OFFSET))
            .map_err(io(&path))?;
        file.write_all(&o.count.to_le_bytes()).map_err(io(&path))?;
        done.push((path, o.count));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CaptureWriter, NewCapture, Status};

    fn capture(name: &str, data: &[u8], rate: u64) -> CaptureFile {
        let dir = std::env::temp_dir().join("lv-store-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut w = CaptureWriter::create(
            &path,
            &NewCapture {
                device: "test".into(),
                samplerate_hz: rate,
                channel_names: (0..8).map(|i| format!("D{i}")).collect(),
            },
            true,
        )
        .unwrap();
        w.push(data).unwrap();
        w.finish(Status::Complete).unwrap();
        CaptureFile::open(&path).unwrap()
    }

    #[test]
    fn sto6_csv_rows_at_changes_only() {
        // D0: 1,1,0,0,1  D1: 0,0,0,1,1 (sample rate 10 kHz -> 100 µs a sample)
        let f = capture("export.lvcap", &[0b01, 0b01, 0b00, 0b10, 0b11], 10_000);
        let out = f.path().with_extension("csv");
        assert_eq!(csv(&f, &out, &[0, 1]).unwrap(), 4);
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            text,
            "Time [s],Channel 0,Channel 1\n\
             0.000000000,1,0\n\
             0.000200000,0,0\n\
             0.000300000,0,1\n\
             0.000400000,1,1\n"
        );
        // Only D1: rows where D1 changes.
        assert_eq!(csv(&f, &out, &[1]).unwrap(), 2);
        assert!(csv(&f, &out, &[8]).is_err());
    }

    #[test]
    fn sto6_binary_v0_layout() {
        let f = capture("export-bin.lvcap", &[0b01, 0b01, 0b00, 0b10, 0b11], 10_000);
        let dir = std::env::temp_dir().join("lv-store-tests").join("bin");
        let files = saleae_binary(&f, &dir, &[0, 1]).unwrap();
        assert_eq!(files.len(), 2);
        let b = std::fs::read(dir.join("digital_0.bin")).unwrap();
        assert_eq!(&b[..8], b"<SALEAE>");
        let i32_at = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let f64_at = |o: usize| f64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        assert_eq!((i32_at(8), i32_at(12), i32_at(16)), (0, 0, 1));
        assert_eq!(f64_at(20), 0.0);
        assert!((f64_at(28) - 0.0005).abs() < 1e-12);
        assert_eq!(u64::from_le_bytes(b[36..44].try_into().unwrap()), 2);
        assert!((f64_at(44) - 0.0002).abs() < 1e-12);
        assert!((f64_at(52) - 0.0004).abs() < 1e-12);
        assert_eq!(b.len(), 60);
        // D1 starts low and rises once.
        let b1 = std::fs::read(dir.join("digital_1.bin")).unwrap();
        assert_eq!(i32::from_le_bytes(b1[16..20].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(b1[36..44].try_into().unwrap()), 1);
    }
}
