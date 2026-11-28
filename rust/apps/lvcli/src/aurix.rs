//! `lvcli aurix`: an AURIX device through a CMSIS-DAP probe (the slot 1
//! firmware of the WeAct board) on its DAP pins, and the TAS server that
//! lets the aurix_debugger tools use it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use lv_dap::link::{DEFAULT_CLOCK_HZ, Link, Reset, Target};
use lv_dap::sim::SimWire;
use lv_dap::wire::Wire;
use lv_dap::{cmsis, mwtrace};

use crate::units::{fmt_rate, parse_si};

#[derive(Subcommand)]
pub enum AurixCmd {
    /// CMSIS-DAP 프로브 목록 (WeAct 보드는 슬롯 1에서 보임)
    Probes,
    /// 장치에 연결해 JTAG ID와 상태를 보여 줌
    Info {
        /// 리셋하고 코어를 멈춘 채 연결
        #[arg(long)]
        reset: bool,
        #[command(flatten)]
        probe: ProbeArgs,
    },
    /// 메모리 읽기
    Read {
        /// 주소 (예: 0xF0000464)
        address: String,
        /// 바이트 수 (예: 4, 0x100, 1k)
        #[arg(default_value = "4")]
        length: String,
        /// 읽은 바이트를 파일로 저장
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        probe: ProbeArgs,
    },
    /// 메모리 쓰기
    Write {
        /// 주소 (예: 0x70000000)
        address: String,
        /// 쓸 바이트, 16진수 (예: 78563412 또는 "78 56 34 12")
        data: String,
        #[command(flatten)]
        probe: ProbeArgs,
    },
    /// 장치 리셋
    Reset {
        /// 리셋 뒤 코어를 멈춰 둠
        #[arg(long)]
        halt: bool,
        #[command(flatten)]
        probe: ProbeArgs,
    },
    /// TAS 서버: probe-rs 포크·aurix-debugger 앱이 TasServer 대신 붙음
    Serve {
        /// 포트 (Infineon TasServer와 같은 기본값)
        #[arg(long, default_value_t = lv_tas::DEFAULT_PORT)]
        port: u16,
        /// 받을 주소. 다른 PC에서 붙으려면 0.0.0.0
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        #[command(flatten)]
        probe: ProbeArgs,
    },
    /// miniWiggler USB 캡처(.pcap, .pcapng)를 DAP 텔레그램으로 풀어 보여 줌
    Trace {
        file: PathBuf,
        /// USB 전송 경계(flush)도 보여 줌
        #[arg(long)]
        flush: bool,
    },
}

#[derive(clap::Args)]
pub struct ProbeArgs {
    /// 프로브 일련번호. 없으면 하나뿐인 프로브
    #[arg(long)]
    probe: Option<String>,
    /// 연결한 뒤의 DAP 클럭 (예: 5M, 500k)
    #[arg(long, default_value = "5M")]
    speed: String,
    /// 프로브 없이 가짜 타깃으로 (명령과 TAS 서버를 시험해 볼 때). 값은 가짜 타깃의 JTAG ID
    #[arg(long, value_name = "JTAG ID", num_args = 0..=1, default_missing_value = "0x20205083")]
    sim: Option<String>,
}

/// Opens the probe (or the pretend device) without connecting to the device.
fn open(args: &ProbeArgs) -> Result<Box<dyn Target>> {
    let hz = parse_si(&args.speed)?;
    if !(1_000..=100_000_000).contains(&hz) {
        bail!("--speed는 1 kHz에서 100 MHz 사이입니다");
    }
    let mut target: Box<dyn Target> = if let Some(id) = &args.sim {
        let mut sim = SimWire::new();
        sim.set_jtag_id(address(id)?);
        Box::new(Link::new(sim))
    } else {
        let wire: Box<dyn Wire> = Box::new(cmsis::open(args.probe.as_deref())?);
        Box::new(Link::new(wire))
    };
    if hz as u32 != DEFAULT_CLOCK_HZ {
        target.set_clock_hz(hz as u32)?;
    }
    Ok(target)
}

fn connect(args: &ProbeArgs, reset: Reset) -> Result<(Box<dyn Target>, u32)> {
    let mut target = open(args)?;
    let id = target.connect(reset)?;
    Ok((target, id))
}

/// A number with an optional `0x` or a k/M suffix.
fn number(s: &str) -> Result<u64> {
    let t = s.trim().replace('_', "");
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16)
            .with_context(|| format!("'{s}'을(를) 16진수로 읽을 수 없습니다")),
        None => parse_si(&t),
    }
}

fn address(s: &str) -> Result<u32> {
    let t = s.trim().replace('_', "");
    let hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"));
    let v = match hex {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => t.parse::<u64>(),
    }
    .with_context(|| format!("'{s}'을(를) 주소로 읽을 수 없습니다 (예: 0xF0000464)"))?;
    u32::try_from(v).with_context(|| format!("주소 {v:#x}는 32비트를 넘습니다"))
}

fn hex_bytes(s: &str) -> Result<Vec<u8>> {
    let digits: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != ',')
        .collect();
    let digits = digits.trim_start_matches("0x");
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        bail!("쓸 바이트는 16진수 두 자리씩입니다 (예: 78563412)");
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&digits[i..i + 2], 16)
                .with_context(|| format!("'{}'은(는) 16진수가 아닙니다", &digits[i..i + 2]))
        })
        .collect()
}

/// A JTAG ID code in words: manufacturer, part number, revision.
fn describe_id(id: u32) -> String {
    let manufacturer = id >> 1 & 0x7FF;
    format!(
        "{id:#010x} (제조사 {manufacturer:#05x}{}, 부품 {:#06x}, 리비전 {})",
        if manufacturer == 0x041 {
            " Infineon"
        } else {
            ""
        },
        id >> 12 & 0xFFFF,
        id >> 28
    )
}

fn hex_dump(address: u32, data: &[u8]) {
    for (i, row) in data.chunks(16).enumerate() {
        let hex: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
        let text: String = row
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!(
            "{:08x}  {:<47}  {text}",
            address.wrapping_add(i as u32 * 16),
            hex.join(" ")
        );
    }
}

pub fn run(cmd: AurixCmd) -> Result<()> {
    match cmd {
        AurixCmd::Probes => probes(),
        AurixCmd::Info { reset, probe } => {
            let reset = if reset { Reset::Halt } else { Reset::None };
            let (target, id) = connect(&probe, reset)?;
            println!("프로브    {}", target.name());
            println!("JTAG ID   {}", describe_id(id));
            println!("클럭      {}", fmt_rate(target.clock_hz() as u64));
            println!(
                "연결      {}",
                if reset == Reset::Halt {
                    "리셋 후 정지"
                } else {
                    "리셋 없이"
                }
            );
            Ok(())
        }
        AurixCmd::Read {
            address: a,
            length,
            output,
            probe,
        } => {
            let a = address(&a)?;
            let n = number(&length)? as usize;
            if n == 0 || n > 16 << 20 {
                bail!("길이는 1바이트에서 16 MiB 사이입니다");
            }
            let (mut target, _) = connect(&probe, Reset::None)?;
            let mut data = vec![0; n];
            target.read(a, &mut data)?;
            match output {
                Some(path) => {
                    std::fs::write(&path, &data)
                        .with_context(|| format!("{}에 쓸 수 없습니다", path.display()))?;
                    println!("{a:#010x}부터 {n} 바이트 -> {}", path.display());
                }
                None => hex_dump(a, &data),
            }
            Ok(())
        }
        AurixCmd::Write {
            address: a,
            data,
            probe,
        } => {
            let a = address(&a)?;
            let data = hex_bytes(&data)?;
            let (mut target, _) = connect(&probe, Reset::None)?;
            target.write(a, &data)?;
            println!("{a:#010x}에 {} 바이트를 썼습니다", data.len());
            Ok(())
        }
        AurixCmd::Reset { halt, probe } => {
            let reset = if halt { Reset::Halt } else { Reset::Run };
            let (_, id) = connect(&probe, reset)?;
            println!(
                "리셋했습니다{} · JTAG ID {id:#010x}",
                if halt { " (코어 정지)" } else { "" }
            );
            Ok(())
        }
        AurixCmd::Serve { port, bind, probe } => serve(&bind, port, &probe),
        AurixCmd::Trace { file, flush } => trace(&file, flush),
    }
}

fn probes() -> Result<()> {
    let list = cmsis::list_probes()?;
    if list.is_empty() {
        println!(
            "CMSIS-DAP 프로브가 없습니다. WeAct 보드는 버튼을 2초 눌러 슬롯 1(USB-시리얼 + DAP, 표시등 파란색)로 바꾸세요."
        );
        return Ok(());
    }
    println!("{:<10} {:<34} 일련번호", "VID:PID", "이름");
    for p in list {
        println!(
            "{:04x}:{:04x}  {:<34} {}",
            p.vendor_id, p.product_id, p.product, p.serial
        );
    }
    Ok(())
}

fn serve(bind: &str, port: u16, args: &ProbeArgs) -> Result<()> {
    let address: SocketAddr = format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("'{bind}:{port}'은(는) 주소가 아닙니다"))?;
    let target = open(args)?;
    let server = lv_tas::Server::start(target, address).with_context(|| {
        format!(
            "{address}에서 받을 수 없습니다. Infineon TasServer가 같은 포트를 쓰고 있으면 끄거나 --port로 다른 포트를 고르세요"
        )
    })?;
    let selector = if port == lv_tas::DEFAULT_PORT && bind == "127.0.0.1" {
        server.identifier().to_string()
    } else {
        let host = if bind == "0.0.0.0" {
            "<이 PC의 주소>"
        } else {
            bind
        };
        format!("{host}:{port}/{}", server.identifier())
    };
    println!("TAS 서버  {}", server.address());
    println!("대상      {}", server.identifier());
    println!("probe-rs  --probe \"058b:0000:{selector}\"");
    println!("Ctrl+C로 멈춥니다.");

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        crate::ctrlc_hook(move || stop.store(true, Ordering::Relaxed));
    }
    let mut shown = (0, 0, 0);
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
        let stats = server.stats();
        // A line when a client comes or goes, on a reset, and now and
        // then while frames run.
        let now = (
            server.sessions() + stats.connections.load(Ordering::Relaxed),
            stats.resets.load(Ordering::Relaxed),
            stats.frames.load(Ordering::Relaxed) / 1000,
        );
        if now != shown {
            shown = now;
            println!(
                "세션 {} · 프레임 {} · 전송 {} (실패 {}) · 리셋 {}",
                server.sessions(),
                stats.frames.load(Ordering::Relaxed),
                stats.transfers.load(Ordering::Relaxed),
                stats.failed_transfers.load(Ordering::Relaxed),
                now.1
            );
        }
    }
    println!("멈춥니다.");
    Ok(())
}

fn trace(file: &PathBuf, flush: bool) -> Result<()> {
    let bytes = std::fs::read(file)
        .with_context(|| format!("{}을(를) 읽을 수 없습니다", file.display()))?;
    let packets = mwtrace::read_capture(&bytes)
        .with_context(|| format!("{}: USBPcap 캡처가 아닙니다", file.display()))?;
    let commands = mwtrace::mpsse_commands(&packets)?;
    let events = mwtrace::dap_events(&commands);
    let mut telegrams = 0;
    for event in &events {
        match event {
            mwtrace::Event::Flush(_) if !flush => continue,
            mwtrace::Event::Telegram { .. } => telegrams += 1,
            _ => {}
        }
        println!("{}", mwtrace::describe(event));
    }
    eprintln!(
        "USB 패킷 {} · MPSSE 명령 {} · 텔레그램 {telegrams}",
        packets.len(),
        commands.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_addresses_and_bytes_are_read() {
        assert_eq!(address("0xF000_0464").unwrap(), 0xF000_0464);
        assert_eq!(address("4096").unwrap(), 4096);
        assert!(address("0x1_0000_0000").is_err());
        assert!(address("here").is_err());
        assert_eq!(number("0x100").unwrap(), 256);
        assert_eq!(number("1k").unwrap(), 1000);
        assert_eq!(hex_bytes("78 56 34 12").unwrap(), [0x78, 0x56, 0x34, 0x12]);
        assert_eq!(hex_bytes("0xCAFE").unwrap(), [0xCA, 0xFE]);
        assert!(hex_bytes("123").is_err());
        assert!(hex_bytes("zz").is_err());
    }

    #[test]
    fn the_id_code_names_infineon() {
        let text = describe_id(0x2020_5083);
        assert!(
            text.contains("Infineon") && text.contains("0x0205"),
            "{text}"
        );
        assert!(!describe_id(0x4BA0_0477).contains("Infineon"));
    }
}
