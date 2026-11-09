//! sigrok session files (`.sr`, STO-5): a zip with `version` ("2"), an INI
//! `metadata` and the logic samples in `logic-1-1`, `logic-1-2`, ... (the
//! layout libsigrok's srzip output writes and its session loader reads).

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::{
    CHUNK_SAMPLES, CaptureFile, CaptureInfo, CaptureWriter, Error, NewCapture, Result, Status,
    WriteStats,
};

/// Logic samples per `logic-1-N` entry when exporting.
const ENTRY_SAMPLES: usize = 4 * CHUNK_SAMPLES;
/// Channels a `.lvcap` holds (one byte per sample).
const MAX_CHANNELS: usize = 8;

/// `24000000` -> `24 MHz`, like libsigrok's `sr_samplerate_string`.
pub fn rate_text(hz: u64) -> String {
    for (div, unit) in [(1_000_000_000, "GHz"), (1_000_000, "MHz"), (1_000, "kHz")] {
        if hz >= div && hz.is_multiple_of(div) {
            return format!("{} {unit}", hz / div);
        }
    }
    format!("{hz} Hz")
}

/// `24 MHz`, `24MHz`, `500 kHz`, `1.5 MHz`, `20000` -> Hz.
pub fn parse_rate(s: &str) -> Option<u64> {
    let s = s.trim();
    let s = s.strip_suffix("Hz").unwrap_or(s).trim_end();
    let (num, mult) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1e3),
        'M' => (&s[..s.len() - 1], 1e6),
        'G' | 'g' => (&s[..s.len() - 1], 1e9),
        _ => (s, 1.0),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v > 0.0).then(|| (v * mult).round() as u64)
}

fn format_err(path: &Path, msg: impl Into<String>) -> Error {
    Error::Format {
        path: path.to_path_buf(),
        msg: msg.into(),
    }
}

fn zip_err(path: &Path) -> impl Fn(zip::result::ZipError) -> Error + '_ {
    move |e| format_err(path, format!("not a readable sigrok session file ({e})"))
}

/// Write the capture as a sigrok session file. Returns the samples written.
pub fn export(f: &CaptureFile, out: &Path) -> Result<u64> {
    let io = |e: std::io::Error| Error::Io(out.to_path_buf(), e);
    let file = File::create(out).map_err(io)?;
    let mut z = ZipWriter::new(BufWriter::new(file));
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(1));

    z.start_file("version", stored).map_err(zip_err(out))?;
    z.write_all(b"2").map_err(io)?;

    let info = &f.info;
    let mut meta = format!(
        "[global]\nsigrok version=logicview {}\n\n[device 1]\ncapturefile=logic-1\n\
         total probes={}\nsamplerate={}\ntotal analog=0\n",
        env!("CARGO_PKG_VERSION"),
        info.num_channels,
        rate_text(info.samplerate_hz)
    );
    for (i, name) in info.channel_names.iter().enumerate() {
        let name: String = name.chars().filter(|c| !c.is_control()).collect();
        meta.push_str(&format!("probe{}={}\n", i + 1, name));
    }
    meta.push_str("unitsize=1\n");
    z.start_file("metadata", stored).map_err(zip_err(out))?;
    z.write_all(meta.as_bytes()).map_err(io)?;

    let mut entry = 0usize;
    let mut buf: Vec<u8> = Vec::with_capacity(ENTRY_SAMPLES);
    let mut flush = |z: &mut ZipWriter<BufWriter<File>>, buf: &mut Vec<u8>| -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        entry += 1;
        z.start_file(format!("logic-1-{entry}"), deflated)
            .map_err(zip_err(out))?;
        z.write_all(buf).map_err(io)?;
        buf.clear();
        Ok(())
    };
    f.for_each_chunk(|_, data| {
        let mut data = data;
        while !data.is_empty() {
            let take = (ENTRY_SAMPLES - buf.len()).min(data.len());
            buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if buf.len() == ENTRY_SAMPLES {
                flush(&mut z, &mut buf)?;
            }
        }
        Ok(())
    })?;
    flush(&mut z, &mut buf)?;
    z.finish().map_err(zip_err(out))?.flush().map_err(io)?;
    Ok(info.num_samples)
}

/// What the `metadata` of a session file says about its logic data.
#[derive(Debug, Clone, PartialEq)]
pub struct SrMeta {
    pub capturefile: String,
    pub samplerate: u64,
    pub unitsize: usize,
    pub total_probes: usize,
    /// `probeN` names by channel index (N - 1).
    pub probes: Vec<(usize, String)>,
}

/// Parse the `metadata` INI of a session file (first `device` section).
pub fn parse_metadata(text: &str) -> std::result::Result<SrMeta, String> {
    let mut in_device = false;
    let mut found = false;
    let mut m = SrMeta {
        capturefile: String::new(),
        samplerate: 0,
        unitsize: 1,
        total_probes: 0,
        probes: Vec::new(),
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if found && in_device {
                break; // only the first device
            }
            in_device = name.starts_with("device ");
            found |= in_device;
            continue;
        }
        if !in_device {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "capturefile" => m.capturefile = v.to_string(),
            "samplerate" => {
                m.samplerate = parse_rate(v).ok_or(format!("unknown sample rate '{v}'"))?
            }
            "unitsize" => m.unitsize = v.parse().map_err(|_| format!("bad unitsize '{v}'"))?,
            "total probes" => {
                m.total_probes = v.parse().map_err(|_| format!("bad probe count '{v}'"))?
            }
            _ => {
                if let Some(n) = k
                    .strip_prefix("probe")
                    .and_then(|n| n.parse::<usize>().ok())
                    && n >= 1
                {
                    m.probes.push((n - 1, v.to_string()));
                }
            }
        }
    }
    if !found {
        return Err("no [device 1] section in metadata".into());
    }
    if m.capturefile.is_empty() {
        return Err("the file has no logic data (no capturefile)".into());
    }
    if m.samplerate == 0 {
        return Err("metadata has no sample rate".into());
    }
    if m.unitsize == 0 {
        return Err("unitsize is 0".into());
    }
    if m.total_probes == 0 {
        m.total_probes = m.probes.iter().map(|(i, _)| i + 1).max().unwrap_or(8);
    }
    Ok(m)
}

/// Import a sigrok session file into a new `.lvcap`. Only channels 0..8
/// are kept (the first byte of each sample unit).
pub fn import(src: &Path, out: &Path, overwrite: bool) -> Result<(CaptureInfo, WriteStats)> {
    let io = |e: std::io::Error| Error::Io(src.to_path_buf(), e);
    let mut z =
        ZipArchive::new(BufReader::new(File::open(src).map_err(io)?)).map_err(zip_err(src))?;
    let mut text = String::new();
    z.by_name("version")
        .map_err(zip_err(src))?
        .read_to_string(&mut text)
        .map_err(io)?;
    if !matches!(text.trim(), "1" | "2") {
        return Err(format_err(
            src,
            format!("session file version {} is not supported", text.trim()),
        ));
    }
    text.clear();
    z.by_name("metadata")
        .map_err(zip_err(src))?
        .read_to_string(&mut text)
        .map_err(io)?;
    let meta = parse_metadata(&text).map_err(|m| format_err(src, m))?;

    let channels = meta.total_probes.min(MAX_CHANNELS);
    if meta.total_probes > MAX_CHANNELS {
        log::warn!(
            "{}: {} channels, keeping the first {MAX_CHANNELS}",
            src.display(),
            meta.total_probes
        );
    }
    let mut names: Vec<String> = (0..channels).map(|i| format!("D{i}")).collect();
    for (i, name) in &meta.probes {
        if *i < channels {
            names[*i] = name.clone();
        }
    }

    // logic-1 alone, or logic-1-1, logic-1-2, ... in number order.
    let base = meta.capturefile.as_str();
    let mut entries: Vec<(u64, String)> = Vec::new();
    for name in z.file_names() {
        let name = name.map_err(zip_err(src))?;
        if name == base {
            entries.push((0, name.to_string()));
        } else if let Some(n) = name
            .strip_prefix(base)
            .and_then(|r| r.strip_prefix('-'))
            .and_then(|n| n.parse::<u64>().ok())
        {
            entries.push((n, name.to_string()));
        }
    }
    entries.sort();
    if entries.is_empty() {
        return Err(format_err(
            src,
            format!("no '{base}' sample data in the file"),
        ));
    }

    let file_name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut w = CaptureWriter::create(
        out,
        &NewCapture {
            device: format!("sigrok session {file_name}"),
            samplerate_hz: meta.samplerate,
            channel_names: names,
        },
        overwrite,
    )?;
    let unit = meta.unitsize;
    let mut raw = Vec::new();
    let mut samples = Vec::new();
    // Bytes of a sample unit split across two entries.
    let mut carry: Vec<u8> = Vec::new();
    for (_, name) in &entries {
        raw.clear();
        raw.extend_from_slice(&carry);
        carry.clear();
        z.by_name(name)
            .map_err(zip_err(src))?
            .read_to_end(&mut raw)
            .map_err(io)?;
        let whole = raw.len() / unit * unit;
        carry.extend_from_slice(&raw[whole..]);
        if unit == 1 {
            w.push(&raw[..whole])?;
        } else {
            samples.clear();
            samples.extend(raw[..whole].iter().step_by(unit));
            w.push(&samples)?;
        }
    }
    if !carry.is_empty() {
        log::warn!(
            "{}: {} trailing bytes are not a whole sample",
            src.display(),
            carry.len()
        );
    }
    w.finish(Status::Complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sto5_rates() {
        assert_eq!(rate_text(24_000_000), "24 MHz");
        assert_eq!(rate_text(500_000), "500 kHz");
        assert_eq!(rate_text(1_500_000), "1500 kHz");
        assert_eq!(rate_text(1234), "1234 Hz");
        for (s, hz) in [
            ("24 MHz", 24_000_000),
            ("24MHz", 24_000_000),
            ("500 kHz", 500_000),
            ("1.5 MHz", 1_500_000),
            ("20000", 20_000),
            ("1 GHz", 1_000_000_000),
        ] {
            assert_eq!(parse_rate(s), Some(hz), "{s}");
        }
        assert_eq!(parse_rate("fast"), None);
    }

    #[test]
    fn sto5_metadata() {
        let m = parse_metadata(
            "[global]\nsigrok version=0.6.0\n\n[device 1]\ncapturefile=logic-1\n\
             total probes=8\nsamplerate=1 MHz\ntotal analog=0\nprobe1=D0\nprobe3=SDA\nunitsize=1\n",
        )
        .unwrap();
        assert_eq!(m.capturefile, "logic-1");
        assert_eq!(m.samplerate, 1_000_000);
        assert_eq!(m.total_probes, 8);
        assert_eq!(m.probes, vec![(0, "D0".into()), (2, "SDA".into())]);
        assert!(parse_metadata("[device 1]\nsamplerate=1 MHz\n").is_err());
    }

    #[test]
    fn sto5_export_import_roundtrip() {
        let dir = std::env::temp_dir().join("lv-store-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("sr-src.lvcap");
        let n = ENTRY_SAMPLES + CHUNK_SAMPLES + 777; // spans two zip entries
        let data: Vec<u8> = (0..n).map(|i| ((i / 3) ^ (i / 1000)) as u8).collect();
        let mut names: Vec<String> = (0..8).map(|i| format!("D{i}")).collect();
        names[1] = "클럭".into();
        let mut w = CaptureWriter::create(
            &src,
            &NewCapture {
                device: "test".into(),
                samplerate_hz: 12_000_000,
                channel_names: names.clone(),
            },
            true,
        )
        .unwrap();
        w.push(&data).unwrap();
        w.finish(Status::Complete).unwrap();

        let sr = dir.join("roundtrip.sr");
        let f = CaptureFile::open(&src).unwrap();
        assert_eq!(export(&f, &sr).unwrap(), n as u64);

        let back = dir.join("roundtrip-back.lvcap");
        let (info, _) = import(&sr, &back, true).unwrap();
        assert_eq!(info.num_samples, n as u64);
        assert_eq!(info.samplerate_hz, 12_000_000);
        assert_eq!(info.channel_names, names);
        let g = CaptureFile::open(&back).unwrap();
        assert_eq!(g.read_samples(0, n as u64).unwrap(), data);
    }
}
