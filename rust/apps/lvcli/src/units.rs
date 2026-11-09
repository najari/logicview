//! Parsing and printing quantities like `24M`, `500k`, `50M`.

use anyhow::{Result, bail};

/// Parse a decimal SI quantity: `24M` = 24,000,000, `500k`, `1.5M`, `12000`.
pub fn parse_si(s: &str) -> Result<u64> {
    let t = s.trim().trim_end_matches(['H', 'h', 'z', 'Z']).trim();
    let (num, mult) = match t.chars().last() {
        Some('k' | 'K') => (&t[..t.len() - 1], 1e3),
        Some('m' | 'M') => (&t[..t.len() - 1], 1e6),
        Some('g' | 'G') => (&t[..t.len() - 1], 1e9),
        _ => (t, 1.0),
    };
    let v: f64 = num.trim().replace('_', "").parse().map_err(|_| {
        anyhow::anyhow!("'{s}'을(를) 숫자로 읽을 수 없습니다 (예: 24M, 500k, 1000000)")
    })?;
    let v = v * mult;
    if !(v.is_finite() && v >= 1.0) {
        bail!("'{s}'은(는) 1 이상이어야 합니다");
    }
    Ok(v.round() as u64)
}

pub fn fmt_rate(hz: u64) -> String {
    let hz = hz as f64;
    if hz >= 1e6 {
        format!("{} MHz", trim(hz / 1e6))
    } else if hz >= 1e3 {
        format!("{} kHz", trim(hz / 1e3))
    } else {
        format!("{} Hz", trim(hz))
    }
}

pub fn fmt_samples(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn fmt_bytes(n: u64) -> String {
    let n = n as f64;
    if n >= (1u64 << 30) as f64 {
        format!("{:.2} GiB", n / (1u64 << 30) as f64)
    } else if n >= (1u64 << 20) as f64 {
        format!("{:.1} MiB", n / (1u64 << 20) as f64)
    } else if n >= 1024.0 {
        format!("{:.1} KiB", n / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aut1_parse_quantities() {
        assert_eq!(parse_si("24M").unwrap(), 24_000_000);
        assert_eq!(parse_si("24MHz").unwrap(), 24_000_000);
        assert_eq!(parse_si("500k").unwrap(), 500_000);
        assert_eq!(parse_si("1.5M").unwrap(), 1_500_000);
        assert_eq!(parse_si("50_000_000").unwrap(), 50_000_000);
        assert!(parse_si("abc").is_err());
        assert!(parse_si("0").is_err());
    }

    #[test]
    fn aut1_format_quantities() {
        assert_eq!(fmt_rate(24_000_000), "24 MHz");
        assert_eq!(fmt_rate(20_000), "20 kHz");
        assert_eq!(fmt_samples(50_186_752), "50,186,752");
        assert_eq!(fmt_samples(512), "512");
        assert_eq!(fmt_bytes(1 << 20), "1.0 MiB");
    }
}
