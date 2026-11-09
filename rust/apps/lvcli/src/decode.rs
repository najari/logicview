//! `lvcli decoders` and `lvcli decode` (DEC-1, DEC-2).

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use lv_decode::{Control, DecoderInfo, parse_stack};
use lv_store::CaptureFile;

use crate::units::fmt_samples;

pub fn decoders(id: Option<&str>) -> Result<()> {
    let rt = lv_decode::runtime()?;
    let Some(id) = id else {
        let list = rt.decoders();
        for d in &list {
            println!("{:<20} {}", d.id, d.longname);
        }
        eprintln!("디코더 {}개 ({})", list.len(), rt.dir().display());
        return Ok(());
    };
    let d = rt.decoder(id)?;
    println!("{} - {}", d.id, d.longname);
    println!("{}", d.desc);
    if !d.inputs.is_empty() {
        println!("입력: {}", d.inputs.join(", "));
    }
    if !d.outputs.is_empty() {
        println!("출력: {}", d.outputs.join(", "));
    }
    let chans = |title: &str, list: &[lv_decode::ChannelInfo]| {
        if !list.is_empty() {
            println!("{title}:");
            for c in list {
                println!("  {:<12} {} ({})", c.id, c.name, c.desc);
            }
        }
    };
    chans("필수 채널", &d.channels);
    chans("선택 채널", &d.opt_channels);
    if !d.options.is_empty() {
        println!("옵션:");
        for o in &d.options {
            let def = o
                .default
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_default();
            let values = if o.values.is_empty() {
                String::new()
            } else {
                format!(
                    " [{}]",
                    o.values
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            println!("  {:<16} {} (기본 {def}){values}", o.id, o.desc);
        }
    }
    println!("주석 클래스:");
    for (i, (cid, desc)) in d.annotations.iter().enumerate() {
        println!("  {i:>2} {cid:<18} {desc}");
    }
    if !d.rows.is_empty() {
        println!("주석 행:");
        for r in &d.rows {
            let ids: Vec<&str> = r.classes.iter().map(|&c| d.class_id(c)).collect();
            println!("  {:<18} {} ({})", r.id, r.desc, ids.join(", "));
        }
    }
    Ok(())
}

/// Which annotations to print: by decoder level, `None` = all classes.
fn visible(
    spec: Option<&str>,
    stack: &[DecoderInfo],
) -> Result<Vec<(usize, Option<HashSet<usize>>)>> {
    let Some(spec) = spec else {
        return Ok(vec![(stack.len() - 1, None)]);
    };
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (id, classes) = match part.split_once('=') {
            Some((id, c)) => (id, Some(c)),
            None => (part, None),
        };
        let level = stack
            .iter()
            .position(|d| d.id == id)
            .with_context(|| format!("-A: '{id}' is not in the decoder stack"))?;
        let d = &stack[level];
        let set = match classes {
            None => None,
            Some(c) => {
                let mut set = HashSet::new();
                for name in c.split(':') {
                    if let Some(i) = d.annotations.iter().position(|a| a.0 == name) {
                        set.insert(i);
                    } else if let Some(r) = d.rows.iter().find(|r| r.id == name) {
                        set.extend(r.classes.iter().copied());
                    } else {
                        bail!("-A: {} has no annotation class or row '{name}'", d.id);
                    }
                }
                Some(set)
            }
        };
        out.push((level, set));
    }
    Ok(out)
}

/// Open a .lvcap, or import a .sr into the temp folder first.
fn open_input(path: &Path) -> Result<(CaptureFile, Option<PathBuf>)> {
    let is_sr = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sr"));
    if !is_sr {
        return Ok((CaptureFile::open(path)?, None));
    }
    let dir = std::env::temp_dir().join("lvcli-decode");
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(format!(
        "{}.lvcap",
        path.file_stem().unwrap_or_default().to_string_lossy()
    ));
    lv_store::sigrok::import(path, &out, true)?;
    Ok((CaptureFile::open(&out)?, Some(out)))
}

pub fn run(
    path: &Path,
    protocol: &str,
    annotations: Option<&str>,
    save: bool,
    label: Option<String>,
    quiet: bool,
) -> Result<()> {
    let stack = parse_stack(protocol)?;
    let (file, temp) = open_input(path)?;
    if save && temp.is_some() {
        bail!("--save은 .lvcap 파일에만 됩니다 (먼저 `lvcli import`로 바꾸세요)");
    }
    let rt = lv_decode::runtime()?;
    let ctl = Control::default();
    let t = Instant::now();
    let d = rt.decode(&file, &stack, &ctl)?;
    let secs = t.elapsed().as_secs_f64();
    let show = visible(annotations, &d.stack)?;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut shown = 0u64;
    for a in &d.annotations {
        let visible = show
            .iter()
            .any(|(l, set)| *l == a.level && set.as_ref().is_none_or(|s| s.contains(&a.class)));
        if !visible {
            continue;
        }
        shown += 1;
        if !quiet {
            let dec = &d.stack[a.level];
            writeln!(
                out,
                "{}-{} {}: {}: {}",
                a.start,
                a.end,
                d.proto_ids[a.level],
                dec.class_id(a.class),
                a.texts.first().map(String::as_str).unwrap_or("")
            )?;
        }
    }
    out.flush()?;
    drop(out);
    for m in &d.log {
        eprintln!("디코더: {m}");
    }
    eprintln!(
        "주석 {}개 (보인 것 {}), 샘플 {}, {:.2} s",
        d.annotations.len(),
        shown,
        fmt_samples(file.info.num_samples),
        secs
    );
    if save {
        let (settings, frames) = d.to_frames(&stack);
        let label =
            label.unwrap_or_else(|| d.stack.last().map(|x| x.name.clone()).unwrap_or_default());
        let path = file.path().to_path_buf();
        drop(file);
        let id = lv_store::analysis::add_analyzer(
            &path,
            &lv_store::analysis::NewAnalyzer {
                kind: lv_decode::analyzer_kind(&stack).into(),
                decoder_id: settings.stack.clone(),
                label: label.clone(),
                settings_json: settings.to_json(),
                input_id: None,
            },
            &frames,
        )?;
        eprintln!("저장: 분석기 {id} '{label}', 프레임 {}", frames.len());
    }
    Ok(())
}
