//! Synthetic captures for viewer tests (UI-4): every channel has a different
//! kind of activity, from a fast clock to long idle stretches.

use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use lv_store::{CHUNK_SAMPLES, CaptureWriter, NewCapture, Status};

use crate::units::{fmt_bytes, fmt_rate, fmt_samples};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Measure how fast captures can be stored (NFR-2): random bytes (zstd
/// cannot shrink them) and all-zero bytes (best case), pushed in 240 KB
/// blocks like the USB thread delivers them at 24 MHz.
pub fn store_bench(samples: u64, output: &Path) -> Result<()> {
    let block = 240_128usize;
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    // 64 MiB of distinct random bytes, so no chunk repeats itself.
    let pool = 64 << 20;
    let random: Vec<u8> = (0..pool / 8)
        .flat_map(|_| rng.next().to_le_bytes())
        .collect();
    let zeros = vec![0u8; pool];
    println!(
        "{:<8} {:>12} {:>10} {:>10} {:>8}",
        "데이터", "샘플", "시간 s", "속도", "압축"
    );
    for (name, data) in [("무작위", &random), ("0", &zeros)] {
        let mut w = CaptureWriter::create(
            output,
            &NewCapture {
                device: "store-bench".into(),
                samplerate_hz: 24_000_000,
                channel_names: (0..8).map(|i| format!("D{i}")).collect(),
            },
            true,
        )?;
        let t = Instant::now();
        let mut left = samples;
        let mut off = 0usize;
        while left > 0 {
            let n = (left as usize).min(block);
            if off + n > data.len() {
                off = 0;
            }
            w.push(&data[off..off + n])?;
            off += n;
            left -= n as u64;
        }
        let (info, stats) = w.finish(Status::Complete)?;
        let secs = t.elapsed().as_secs_f64();
        println!(
            "{:<8} {:>12} {:>10.3} {:>7.0} MB/s {:>7.1}x",
            name,
            fmt_samples(info.num_samples),
            secs,
            info.num_samples as f64 / secs / 1e6,
            stats.ratio()
        );
    }
    let _ = std::fs::remove_file(output);
    println!("24 MHz 수집에 필요한 속도: 24 MB/s");
    Ok(())
}

/// Channel activity:
/// - D0: UART at 115200 baud, a random byte every ~2 ms
/// - D1: 1 MHz clock
/// - D2: 1 kHz square wave
/// - D3: 10 kHz PWM, duty slowly sweeping
/// - D4: 1 ms bursts of a 2 MHz clock every 10 ms
/// - D5: random edges, about one per 300 samples
/// - D6: always low, D7: always high
pub fn run(samples: u64, rate: u64, output: &Path) -> Result<()> {
    let names = [
        "UART TX",
        "클럭 1 MHz",
        "1 kHz",
        "PWM",
        "버스트",
        "잡음",
        "항상 0",
        "항상 1",
    ];
    let mut w = CaptureWriter::create(
        output,
        &NewCapture {
            device: "합성 데이터 (lvcli synth)".into(),
            samplerate_hz: rate,
            channel_names: names.iter().map(|s| s.to_string()).collect(),
        },
        true,
    )?;
    let t = Instant::now();
    let r = rate as f64;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let bit = (r / 115_200.0).max(1.0);
    // UART state
    let mut uart_frame: Option<(u64, u16)> = None; // (start sample, 10-bit frame)
    let mut next_uart = 0u64;
    let mut noise = 0u8;
    let mut buf = Vec::with_capacity(CHUNK_SAMPLES);
    for i in 0..samples {
        let mut b = 0u8;
        // D0 UART: idle high, start bit 0, 8 data bits LSB first, stop bit 1.
        let mut tx = 1u8;
        if uart_frame.is_none() && i >= next_uart {
            let byte = (rng.next() & 0xff) as u16;
            uart_frame = Some((i, (byte << 1) | (1 << 9)));
            next_uart = i + (r * 0.002) as u64 + (rng.next() % (r * 0.001) as u64 + 1);
        }
        if let Some((start, frame)) = uart_frame {
            let idx = ((i - start) as f64 / bit) as u32;
            if idx < 10 {
                tx = ((frame >> idx) & 1) as u8;
            } else {
                uart_frame = None;
            }
        }
        b |= tx;
        let t_s = i as f64 / r;
        // D1 1 MHz clock
        if (t_s * 2e6) as u64 % 2 == 1 {
            b |= 1 << 1;
        }
        // D2 1 kHz
        if (t_s * 2e3) as u64 % 2 == 1 {
            b |= 1 << 2;
        }
        // D3 10 kHz PWM, duty sweeping over 1 s
        let phase = (t_s * 1e4).fract();
        let duty = 0.1 + 0.8 * (t_s.fract());
        if phase < duty {
            b |= 1 << 3;
        }
        // D4 bursts: 1 ms of 2 MHz every 10 ms
        if (t_s * 100.0).fract() < 0.1 && (t_s * 4e6) as u64 % 2 == 1 {
            b |= 1 << 4;
        }
        // D5 random edges
        if rng.next().is_multiple_of(300) {
            noise ^= 1;
        }
        b |= noise << 5;
        // D7 high
        b |= 1 << 7;
        buf.push(b);
        if buf.len() == CHUNK_SAMPLES {
            w.push(&buf)?;
            buf.clear();
        }
    }
    w.push(&buf)?;
    let (info, stats) = w.finish(Status::Complete)?;
    println!(
        "{}: {} 샘플 ({}), 저장 {} (압축 {:.1}배), {:.1} s",
        output.display(),
        fmt_samples(info.num_samples),
        fmt_rate(rate),
        fmt_bytes(stats.stored_bytes),
        stats.ratio(),
        t.elapsed().as_secs_f64()
    );
    Ok(())
}
