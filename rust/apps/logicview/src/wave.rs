//! Waveform rendering into one mesh (UI-2, UI-3).
//!
//! Zoomed out, every pixel column folds the summary entries (or raw samples)
//! it covers into one `[or, and]` pair, so a channel is low, high or
//! changing there. Zoomed in past one sample per pixel, the raw samples are
//! drawn as level runs with vertical edges. Either way the work is bounded by
//! the plot width, not by the capture length.

use eframe::egui::{Color32, Mesh, Rect, pos2};
use lv_store::summary::{EMPTY, Entry, LEVEL_BLOCK, merge};

use crate::capture_view::CaptureView;

/// What is visible: the sample at the left edge and samples per point.
#[derive(Debug, Clone, Copy)]
pub struct View {
    pub start: f64,
    pub spp: f64,
}

impl View {
    pub fn sample_at(&self, x_from_left: f32) -> f64 {
        self.start + x_from_left as f64 * self.spp
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Row {
    pub channel: u8,
    pub hi: f32,
    pub lo: f32,
    pub color: Color32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WaveStats {
    /// Summary level used; 0 = raw samples.
    pub level: usize,
    /// Zoomed in past one sample per point.
    pub sample_mode: bool,
    pub quads: usize,
}

const LINE: f32 = 1.5;

struct Painter {
    mesh: Mesh,
    quads: usize,
}

impl Painter {
    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color32) {
        if x1 > x0 && y1 > y0 {
            self.mesh
                .add_colored_rect(Rect::from_min_max(pos2(x0, y0), pos2(x1, y1)), c);
            self.quads += 1;
        }
    }
    fn hline(&mut self, x0: f32, x1: f32, y: f32, c: Color32) {
        self.rect(x0, y - LINE / 2.0, x1, y + LINE / 2.0, c);
    }
    fn vline(&mut self, x: f32, y0: f32, y1: f32, c: Color32) {
        self.rect(
            x - LINE / 2.0,
            y0 - LINE / 2.0,
            x + LINE / 2.0,
            y1 + LINE / 2.0,
            c,
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    None,
    Low,
    High,
    Changing,
}

fn state(e: Option<Entry>, bit: u8) -> St {
    match e {
        None => St::None,
        Some(e) if e[1] & bit != 0 => St::High,
        Some(e) if e[0] & bit == 0 => St::Low,
        Some(_) => St::Changing,
    }
}

pub fn build(
    cv: &mut CaptureView,
    plot: Rect,
    rows: &[Row],
    view: View,
) -> lv_store::Result<(Mesh, WaveStats)> {
    let mut p = Painter {
        mesh: Mesh::default(),
        quads: 0,
    };
    let n = cv.num_samples();
    let width = plot.width().max(1.0);
    let mut stats = WaveStats::default();
    if n == 0 {
        return Ok((p.mesh, stats));
    }

    if view.spp < 1.0 {
        stats.sample_mode = true;
        let a = view.start.floor().max(0.0) as u64;
        let b = ((view.start + width as f64 * view.spp).ceil() as u64 + 1).min(n);
        if a >= b {
            return Ok((p.mesh, stats));
        }
        let raw = cv.raw(a, b)?;
        let x_of = |k: u64| -> f32 {
            (plot.left() + ((k as f64 - view.start) / view.spp) as f32)
                .clamp(plot.left(), plot.right())
        };
        for row in rows {
            let bit = 1u8 << row.channel;
            let mut run_start = a;
            let mut v = raw[0] & bit != 0;
            for (i, &s) in raw.iter().enumerate().skip(1) {
                let nv = s & bit != 0;
                if nv != v {
                    let k = a + i as u64;
                    let x = x_of(k);
                    p.hline(
                        x_of(run_start),
                        x,
                        if v { row.hi } else { row.lo },
                        row.color,
                    );
                    p.vline(x, row.hi, row.lo, row.color);
                    run_start = k;
                    v = nv;
                }
            }
            p.hline(
                x_of(run_start),
                x_of(b),
                if v { row.hi } else { row.lo },
                row.color,
            );
        }
        stats.quads = p.quads;
        return Ok((p.mesh, stats));
    }

    // One entry per pixel column.
    let cols = width.ceil() as usize;
    let level = CaptureView::level_for(view.spp);
    stats.level = level;
    let mut px: Vec<Option<Entry>> = vec![None; cols];
    let span = |c: usize| {
        let s0 = view.start + c as f64 * view.spp;
        (s0, s0 + view.spp)
    };
    if level == 0 {
        let a = view.start.floor().max(0.0) as u64;
        let b = ((view.start + cols as f64 * view.spp).ceil() as u64).min(n);
        if a < b {
            let raw = cv.raw(a, b)?;
            for (c, slot) in px.iter_mut().enumerate() {
                let (s0, s1) = span(c);
                let i0 = (s0.floor().max(a as f64) as u64).saturating_sub(a) as usize;
                let i1 = ((s1.ceil().min(b as f64)) as u64).saturating_sub(a) as usize;
                if i0 < i1 {
                    let mut e = EMPTY;
                    for &s in &raw[i0..i1] {
                        e = merge(e, [s, s]);
                    }
                    *slot = Some(e);
                }
            }
        }
    } else {
        let entries = cv.level(level);
        let blk = LEVEL_BLOCK[level] as f64;
        for (c, slot) in px.iter_mut().enumerate() {
            let (s0, s1) = span(c);
            if s1 <= 0.0 {
                continue;
            }
            let e0 = (s0.max(0.0) / blk).floor() as usize;
            let e1 = ((s1 / blk).ceil() as usize).min(entries.len());
            if e0 < e1 {
                *slot = Some(entries[e0..e1].iter().copied().fold(EMPTY, merge));
            }
        }
    }

    let x_at = |c: usize| plot.left() + c as f32;
    for row in rows {
        let bit = 1u8 << row.channel;
        let fill = row.color.gamma_multiply(0.45);
        let mut run_start = 0usize;
        let mut run = state(px[0], bit);
        for c in 1..=cols {
            let s = if let Some(&e) = px.get(c) {
                state(e, bit)
            } else {
                St::None
            };
            if s == run && c < cols {
                continue;
            }
            let (x0, x1) = (x_at(run_start), x_at(c).min(plot.right()));
            match run {
                St::Low => p.hline(x0, x1, row.lo, row.color),
                St::High => p.hline(x0, x1, row.hi, row.color),
                St::Changing => {
                    p.rect(x0, row.hi, x1, row.lo, fill);
                    p.hline(x0, x1, row.hi, row.color);
                    p.hline(x0, x1, row.lo, row.color);
                }
                St::None => {}
            }
            if matches!((run, s), (St::Low, St::High) | (St::High, St::Low)) {
                p.vline(x1, row.hi, row.lo, row.color);
            }
            run_start = c;
            run = s;
        }
    }
    stats.quads = p.quads;
    Ok((p.mesh, stats))
}

/// A tick step of 1, 2 or 5 × 10^k seconds near `approx`.
pub fn nice_step(approx: f64) -> f64 {
    if approx <= 0.0 || !approx.is_finite() {
        return 1.0;
    }
    let mag = 10f64.powf(approx.log10().floor());
    let r = approx / mag;
    let m = if r < 1.5 {
        1.0
    } else if r < 3.5 {
        2.0
    } else if r < 7.5 {
        5.0
    } else {
        10.0
    };
    m * mag
}

/// Format a time: the unit follows the size of the time itself, the number
/// of decimals follows `step` (the tick spacing), so deep zooms read
/// `100.0005 ms` rather than `100000500 ns`.
pub fn fmt_time(t: f64, step: f64) -> String {
    let mag = t.abs().max(step);
    let (scale, unit) = if mag >= 1.0 {
        (1.0, "s")
    } else if mag >= 1e-3 {
        (1e3, "ms")
    } else if mag >= 1e-6 {
        (1e6, "µs")
    } else {
        (1e9, "ns")
    };
    let v = t * scale;
    let s = step * scale;
    let decimals = if s >= 1.0 {
        0
    } else {
        ((-s.log10()) - 1e-9).ceil() as usize
    };
    format!("{v:.decimals$} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui2_nice_steps() {
        assert_eq!(nice_step(0.9), 1.0);
        assert_eq!(nice_step(2.6), 2.0);
        assert_eq!(nice_step(4.0), 5.0);
        assert!((nice_step(3.3e-6) - 2e-6).abs() < 1e-18);
    }

    #[test]
    fn ui2_time_labels() {
        assert_eq!(fmt_time(0.5, 0.1), "500 ms");
        assert_eq!(fmt_time(0.0, 0.2), "0 ms");
        assert_eq!(fmt_time(1.5e-6, 5e-7), "1.5 µs");
        assert_eq!(fmt_time(2.0, 1.0), "2 s");
        assert_eq!(fmt_time(1.2, 0.2), "1.2 s");
        assert_eq!(fmt_time(0.0015, 0.0005), "1.5 ms");
        assert_eq!(fmt_time(0.004, 0.002), "4 ms");
        // Deep zoom far into the capture.
        assert_eq!(fmt_time(0.1000005, 5e-7), "100.0005 ms");
        assert_eq!(fmt_time(0.1005, 1e-4), "100.5 ms");
    }

    #[test]
    fn ui3_level_choice() {
        assert_eq!(CaptureView::level_for(0.5), 0);
        assert_eq!(CaptureView::level_for(63.0), 0);
        assert_eq!(CaptureView::level_for(64.0), 1);
        assert_eq!(CaptureView::level_for(25_000.0), 2);
        assert_eq!(CaptureView::level_for(1e7), 3);
    }
}
