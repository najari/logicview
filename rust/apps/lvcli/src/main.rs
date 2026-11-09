//! `lvcli`: logicview from the command line (AUT-1, AUT-2).

mod synth;
mod units;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use lv_capture::{Capture, CaptureReport, CaptureRequest, Outcome, Trigger};
use lv_fx2lafw::{FoundDevice, Fx2lafw};
use lv_store::CaptureFile;

use units::{fmt_bytes, fmt_rate, fmt_samples, parse_si};

#[derive(Parser)]
#[command(name = "lvcli", version, about = "logicview 명령줄 도구")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 연결된 장치 목록
    Devices,
    /// 수집해서 .lvcap 파일로 저장
    Capture {
        /// 샘플레이트 (예: 24M, 500k)
        #[arg(short, long, default_value = "24M")]
        rate: String,
        /// 샘플 수 (예: 50M). 없으면 Ctrl+C까지 수집
        #[arg(short, long)]
        samples: Option<String>,
        /// 출력 파일
        #[arg(short, long, default_value = "capture.lvcap")]
        output: PathBuf,
        /// 같은 이름의 파일을 덮어씀
        #[arg(long)]
        overwrite: bool,
        /// 이 시간(초)이 지나면 멈춤 (수동 중지와 같음)
        #[arg(long)]
        time: Option<f64>,
        /// 트리거 (예: 0=r,3=1). r 상승, f 하강, e 에지, 1 High, 0 Low의 AND
        #[arg(short, long)]
        trigger: Option<String>,
        /// 트리거 앞 구간 비율 % (샘플 수를 정했을 때)
        #[arg(long, default_value_t = 10)]
        pre: u8,
        /// 채널 이름, 쉼표로 구분 (예: TX,RX,SCL,SDA)
        #[arg(long)]
        names: Option<String>,
        #[command(flatten)]
        dev: DeviceArgs,
    },
    /// 같은 수집을 여러 번 반복하고 결과를 표로 (시제품 관문, AUT-2)
    Bench {
        #[arg(short, long, default_value = "24M")]
        rate: String,
        #[arg(short, long, default_value = "50M")]
        samples: String,
        #[arg(short = 'n', long, default_value_t = 5)]
        runs: usize,
        /// 캡처 파일을 둘 폴더 (기본: 임시 폴더, 끝나면 지움)
        #[arg(long)]
        dir: Option<PathBuf>,
        /// 캡처 파일을 남김
        #[arg(long)]
        keep: bool,
        #[command(flatten)]
        dev: DeviceArgs,
    },
    /// .lvcap 파일 정보
    Info { file: PathBuf },
    /// .lvcap을 다른 형식으로 내보내기 (STO-5, STO-6)
    Export {
        file: PathBuf,
        /// csv (Saleae CSV), bin (Saleae 바이너리, 채널마다 digital_N.bin), sr (sigrok), raw (샘플마다 1바이트)
        #[arg(short, long, value_enum)]
        format: ExportFormat,
        /// 출력 파일 (bin은 폴더). 없으면 입력 이름에 확장자만 바꿈
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// 내보낼 채널, 쉼표로 구분 (예: 0,1). 없으면 모든 채널 (sr은 항상 모두)
        #[arg(short, long, value_delimiter = ',')]
        channels: Vec<usize>,
    },
    /// sigrok .sr 파일을 .lvcap으로 불러오기 (STO-5)
    Import {
        file: PathBuf,
        /// 출력 파일. 없으면 입력 이름.lvcap
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        overwrite: bool,
    },
    /// 저장 속도 측정: 압축이 안 되는 무작위 데이터와 0만 있는 데이터
    StoreBench {
        #[arg(short, long, default_value = "50M")]
        samples: String,
        #[arg(short, long, default_value = "store-bench.lvcap")]
        output: PathBuf,
    },
    /// 화면 시험용 합성 캡처 만들기
    Synth {
        #[arg(short, long, default_value = "50M")]
        samples: String,
        #[arg(short, long, default_value = "24M")]
        rate: String,
        #[arg(short, long, default_value = "synth.lvcap")]
        output: PathBuf,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ExportFormat {
    Csv,
    Bin,
    Sr,
    Raw,
}

/// Capture options beyond the device.
struct CaptureOpts {
    time: Option<f64>,
    trigger: Option<Trigger>,
    names: Option<Vec<String>>,
}

#[derive(clap::Args)]
struct DeviceArgs {
    /// 장치 위치 (예: usb/1-8). 없으면 첫 장치
    #[arg(short, long)]
    device: Option<String>,
    /// 전송 하나의 크기(바이트). 기본은 10 ms 분량
    #[arg(long)]
    transfer_size: Option<usize>,
    /// 동시에 큐에 넣을 전송 수. 기본은 500 ms 분량, 최대 32
    #[arg(long)]
    transfers: Option<usize>,
    /// 저장 대기열 크기(MiB)
    #[arg(long, default_value_t = 64)]
    queue_mib: usize,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("오류: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Devices => devices(),
        Cmd::Capture {
            rate,
            samples,
            output,
            overwrite,
            time,
            trigger,
            pre,
            names,
            dev,
        } => {
            let opts = CaptureOpts {
                time,
                trigger: trigger
                    .map(|t| Trigger::parse(&t, pre))
                    .transpose()
                    .map_err(|e| anyhow::anyhow!("트리거: {e}"))?,
                names: names.map(|n| n.split(',').map(|s| s.trim().to_string()).collect()),
            };
            capture(&rate, samples.as_deref(), &output, overwrite, &opts, &dev)
        }
        Cmd::Bench {
            rate,
            samples,
            runs,
            dir,
            keep,
            dev,
        } => bench(&rate, &samples, runs, dir, keep, &dev),
        Cmd::Info { file } => info(&file),
        Cmd::Export {
            file,
            format,
            output,
            channels,
        } => export(&file, format, output, &channels),
        Cmd::Import {
            file,
            output,
            overwrite,
        } => import(&file, output, overwrite),
        Cmd::StoreBench { samples, output } => synth::store_bench(parse_si(&samples)?, &output),
        Cmd::Synth {
            samples,
            rate,
            output,
        } => synth::run(parse_si(&samples)?, parse_si(&rate)?, &output),
    }
}

fn devices() -> Result<()> {
    let list = lv_fx2lafw::list_devices()?;
    if list.is_empty() {
        println!(
            "지원하는 장치가 없습니다. 애널라이저가 꽂혀 있고 WinUSB 드라이버가 설치됐는지 확인하세요."
        );
        return Ok(());
    }
    println!("{:<12} {:<10} {:<26} 펌웨어", "위치", "VID:PID", "모델");
    for d in list {
        println!(
            "{:<12} {:04x}:{:04x}  {:<26} {}",
            d.location,
            d.info.vendor_id(),
            d.info.product_id(),
            d.name(),
            if d.has_firmware {
                "fx2lafw 올라감"
            } else {
                "없음 (열 때 올림)"
            }
        );
    }
    Ok(())
}

fn pick_device(args: &DeviceArgs) -> Result<FoundDevice> {
    let list = lv_fx2lafw::list_devices()?;
    let found = match &args.device {
        Some(loc) => list.into_iter().find(|d| &d.location == loc),
        None => list.into_iter().next(),
    };
    found.with_context(|| match &args.device {
        Some(loc) => format!("{loc}에 지원하는 장치가 없습니다 (`lvcli devices`로 확인)"),
        None => "지원하는 장치가 없습니다. 애널라이저가 꽂혀 있고 WinUSB 드라이버가 설치됐는지 확인하세요".into(),
    })
}

fn open_device(args: &DeviceArgs) -> Result<Fx2lafw> {
    let found = pick_device(args)?;
    let t = Instant::now();
    let dev = lv_fx2lafw::open(&found, &lv_fx2lafw::default_firmware_dirs())?;
    eprintln!(
        "장치: {} {} (fx2lafw {}.{}, REVID {}, 여는 데 {} ms)",
        dev.name,
        dev.location,
        dev.fw_version.0,
        dev.fw_version.1,
        dev.revid,
        t.elapsed().as_millis()
    );
    Ok(dev)
}

fn request(
    rate: u64,
    samples: Option<u64>,
    output: &Path,
    overwrite: bool,
    args: &DeviceArgs,
) -> CaptureRequest {
    let mut req = CaptureRequest::new(rate, samples, output.to_path_buf());
    req.overwrite = overwrite;
    req.queue_bytes = args.queue_mib << 20;
    req.transfer_size = args.transfer_size;
    req.transfer_count = args.transfers;
    req
}

fn capture(
    rate: &str,
    samples: Option<&str>,
    output: &Path,
    overwrite: bool,
    opts: &CaptureOpts,
    args: &DeviceArgs,
) -> Result<()> {
    let rate = parse_si(rate)?;
    let samples = samples.map(parse_si).transpose()?;
    let mut req = request(rate, samples, output, overwrite, args);
    if let Some(names) = &opts.names {
        if names.len() > 8 {
            bail!("채널 이름은 8개까지입니다 ({}개를 줌)", names.len());
        }
        for (i, n) in names.iter().enumerate() {
            req.channel_names[i] = n.clone();
        }
    }
    req.trigger = opts.trigger.clone();
    if let Some(t) = &req.trigger {
        eprintln!(
            "트리거: {} (앞 구간 {} 샘플)",
            t.spec(),
            fmt_samples(req.pre_trigger_samples())
        );
    }
    let dev = open_device(args)?;
    let cap = Capture::start(dev, req)?;
    let deadline = opts.time.map(Duration::from_secs_f64);

    // Ctrl+C stops the capture cleanly instead of killing the process.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc_hook(move || stop.store(true, std::sync::atomic::Ordering::Relaxed));
    }
    let t = Instant::now();
    while !cap.is_finished() {
        if stop.load(std::sync::atomic::Ordering::Relaxed)
            || deadline.is_some_and(|d| t.elapsed() >= d)
        {
            cap.stop();
        }
        std::thread::sleep(Duration::from_millis(100));
        let p = cap.progress().snapshot();
        eprint!(
            "\r{:>7.2} s  받음 {:>12}  저장 {:>12}  대기열 {:>3}/{}{}   ",
            t.elapsed().as_secs_f64(),
            fmt_samples(p.received),
            fmt_samples(p.written),
            p.queued,
            p.queue_capacity,
            if p.triggered {
                ""
            } else {
                "  트리거 대기"
            }
        );
    }
    eprintln!();
    let (_, report) = cap.wait();
    let report = report?;
    print_report(&report);
    if report.outcome == Outcome::NotTriggered {
        let _ = std::fs::remove_file(output);
    }
    if !report.outcome.is_ok() {
        bail!("{}", report.outcome.describe());
    }
    Ok(())
}

fn print_report(r: &CaptureReport) {
    let s = &r.stream;
    println!("결과: {}", r.outcome.describe());
    if let Some(at) = r.trigger_sample {
        println!(
            "  트리거 위치: 샘플 {} ({:.6} s)",
            fmt_samples(at),
            at as f64 / r.info.samplerate_hz.max(1) as f64
        );
    }
    println!(
        "  샘플 {} ({}), {:.3} s, 실효 {}",
        fmt_samples(r.info.num_samples),
        fmt_rate(r.info.samplerate_hz),
        s.elapsed.as_secs_f64(),
        fmt_rate((r.info.num_samples as f64 / s.elapsed.as_secs_f64().max(1e-9)) as u64)
    );
    println!(
        "  전송 {}건 (빈 전송 {}), 크기 {} × {}개, 첫 데이터 {} ms",
        s.transfers,
        s.empty_transfers,
        fmt_bytes(s.transfer_size as u64),
        s.transfer_count,
        s.first_data.map(|d| d.as_millis()).unwrap_or(0)
    );
    println!(
        "  저장 {} → {} (압축 {:.1}배), 압축 {:.0} ms, DB {:.0} ms, 마무리 {:.0} ms",
        fmt_bytes(r.write.raw_bytes),
        fmt_bytes(r.write.stored_bytes),
        r.write.ratio(),
        r.write.compress_time.as_secs_f64() * 1e3,
        r.write.db_time.as_secs_f64() * 1e3,
        r.finish_time.as_secs_f64() * 1e3
    );
    println!(
        "  대기열 최대 {} / {} ({:.0}%)",
        fmt_bytes(r.queue_peak_bytes as u64),
        fmt_bytes(r.queue_capacity_bytes as u64),
        100.0 * r.queue_peak_bytes as f64 / r.queue_capacity_bytes.max(1) as f64
    );
}

fn bench(
    rate: &str,
    samples: &str,
    runs: usize,
    dir: Option<PathBuf>,
    keep: bool,
    args: &DeviceArgs,
) -> Result<()> {
    let rate = parse_si(rate)?;
    let samples = parse_si(samples)?;
    let dir = dir.unwrap_or_else(|| std::env::temp_dir().join("lvcli-bench"));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("{} 폴더를 만들 수 없습니다", dir.display()))?;
    let mut dev = Some(open_device(args)?);
    println!(
        "벤치: {} × {}, {}회, 파일 {}",
        fmt_rate(rate),
        fmt_samples(samples),
        runs,
        dir.display()
    );
    println!(
        "{:>3}  {:<8} {:>12} {:>8} {:>10} {:>8} {:>9} {:>8}",
        "회", "결과", "샘플", "시간 s", "실효", "압축", "대기열", "마무리"
    );
    let mut passed = 0;
    for run in 1..=runs {
        let path = dir.join(format!("bench-{run}.lvcap"));
        let d = dev.take().context("장치를 다시 열 수 없습니다")?;
        let cap = Capture::start(d, request(rate, Some(samples), &path, true, args))?;
        let (back, report) = cap.wait();
        dev = back;
        let r = report?;
        let ok = r.outcome == lv_capture::Outcome::Complete && r.info.num_samples == samples;
        // Check the file reads back with every sample.
        let readable = CaptureFile::open(&path)
            .map(|f| f.info.num_samples == r.info.num_samples)
            .unwrap_or(false);
        let ok = ok && readable;
        if ok {
            passed += 1;
        }
        println!(
            "{:>3}  {:<8} {:>12} {:>8.3} {:>10} {:>7.1}x {:>8.0}% {:>6.0}ms",
            run,
            if ok { "통과" } else { "실패" },
            fmt_samples(r.info.num_samples),
            r.stream.elapsed.as_secs_f64(),
            fmt_rate((r.info.num_samples as f64 / r.stream.elapsed.as_secs_f64().max(1e-9)) as u64),
            r.write.ratio(),
            100.0 * r.queue_peak_bytes as f64 / r.queue_capacity_bytes.max(1) as f64,
            r.finish_time.as_secs_f64() * 1e3
        );
        if !ok {
            println!("     {}", r.outcome.describe());
        }
        if !keep {
            let _ = std::fs::remove_file(&path);
        }
    }
    println!("통과 {passed}/{runs}");
    if passed != runs {
        bail!("{}회 중 {}회 실패", runs, runs - passed);
    }
    Ok(())
}

fn info(path: &Path) -> Result<()> {
    let f = CaptureFile::open(path)?;
    let i = &f.info;
    let stored = f.stored_bytes()?;
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    println!("파일      {}", path.display());
    println!("만든 때   {}", i.created_at);
    println!("장치      {}", i.device);
    println!("상태      {}", i.status.as_str());
    println!("샘플레이트 {}", fmt_rate(i.samplerate_hz));
    println!(
        "샘플      {} ({:.6} s)",
        fmt_samples(i.num_samples),
        i.duration_secs()
    );
    println!("채널      {}", i.channel_names.join(", "));
    let hidden: Vec<String> = (0..i.channel_enabled.len())
        .filter(|&c| !i.channel_enabled[c])
        .map(|c| format!("D{c}"))
        .collect();
    if !hidden.is_empty() {
        println!("숨긴 채널 {}", hidden.join(", "));
    }
    if let Some(t) = &i.trigger {
        println!("트리거    {t}");
    }
    println!(
        "덩어리    {}개, 샘플 저장 {} (압축 {:.1}배), 파일 {}",
        f.chunks().len(),
        fmt_bytes(stored),
        i.num_samples as f64 / stored.max(1) as f64,
        fmt_bytes(file_size)
    );
    Ok(())
}

fn export(
    path: &Path,
    format: ExportFormat,
    output: Option<PathBuf>,
    channels: &[usize],
) -> Result<()> {
    let f = CaptureFile::open(path)?;
    let t = Instant::now();
    match format {
        ExportFormat::Csv => {
            let out = output.unwrap_or_else(|| path.with_extension("csv"));
            let rows = lv_store::export::csv(&f, &out, channels)?;
            println!(
                "{}: {} 행 ({:.2} s)",
                out.display(),
                fmt_samples(rows),
                t.elapsed().as_secs_f64()
            );
        }
        ExportFormat::Bin => {
            let out = output.unwrap_or_else(|| path.with_extension(""));
            let files = lv_store::export::saleae_binary(&f, &out, channels)?;
            for (p, n) in &files {
                println!("{}: 전이 {}", p.display(), fmt_samples(*n));
            }
            println!("{:.2} s", t.elapsed().as_secs_f64());
        }
        ExportFormat::Raw => {
            let out = output.unwrap_or_else(|| path.with_extension("bin"));
            let n = lv_store::export::raw(&f, &out)?;
            println!(
                "{}: {} 샘플 ({:.2} s)",
                out.display(),
                fmt_samples(n),
                t.elapsed().as_secs_f64()
            );
        }
        ExportFormat::Sr => {
            if !channels.is_empty() {
                bail!(".sr 내보내기는 항상 모든 채널을 씁니다 (--channels 없이 실행)");
            }
            let out = output.unwrap_or_else(|| path.with_extension("sr"));
            let n = lv_store::sigrok::export(&f, &out)?;
            let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            println!(
                "{}: {} 샘플, {} ({:.2} s)",
                out.display(),
                fmt_samples(n),
                fmt_bytes(size),
                t.elapsed().as_secs_f64()
            );
        }
    }
    Ok(())
}

fn import(path: &Path, output: Option<PathBuf>, overwrite: bool) -> Result<()> {
    let out = output.unwrap_or_else(|| path.with_extension("lvcap"));
    let t = Instant::now();
    let (info, stats) = lv_store::sigrok::import(path, &out, overwrite)?;
    println!(
        "{}: {} 샘플 ({}), 채널 {}, 저장 {} (압축 {:.1}배), {:.2} s",
        out.display(),
        fmt_samples(info.num_samples),
        fmt_rate(info.samplerate_hz),
        info.channel_names.join(", "),
        fmt_bytes(stats.stored_bytes),
        stats.ratio(),
        t.elapsed().as_secs_f64()
    );
    Ok(())
}

/// Minimal Ctrl+C hook without an extra crate.
#[cfg(windows)]
fn ctrlc_hook(f: impl Fn() + Send + Sync + 'static) {
    use std::sync::OnceLock;
    static HANDLER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    unsafe extern "system" fn handler(ctrl: u32) -> i32 {
        if ctrl == 0 {
            // CTRL_C_EVENT
            if let Some(h) = HANDLER.get() {
                h();
            }
            1
        } else {
            0
        }
    }
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    if HANDLER.set(Box::new(f)).is_ok() {
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
}

#[cfg(not(windows))]
fn ctrlc_hook(_f: impl Fn() + Send + Sync + 'static) {}
