//! `logicview --bench <file>`: a scripted zoom and pan run that prints frame
//! times and exits (UI-4 measurement).

use crate::wave::{View, WaveStats};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Warmup(u32),
    ZoomIn,
    ZoomOut,
    Pan { spp: f64, frames: u32, idx: usize },
    Done,
}

const PAN_SPPS: [f64; 4] = [0.25, 20.0, 2_000.0, 200_000.0];
const PAN_FRAMES: u32 = 60;
const WARMUP: u32 = 10;

#[derive(Debug)]
pub struct Bench {
    phase: Phase,
    samples: Vec<(f64, f64, WaveStats, &'static str)>,
    reported: bool,
}

impl Default for Bench {
    fn default() -> Self {
        Self {
            phase: Phase::Warmup(WARMUP),
            samples: Vec::new(),
            reported: false,
        }
    }
}

impl Bench {
    fn name(&self) -> &'static str {
        match self.phase {
            Phase::Warmup(_) => "warmup",
            Phase::ZoomIn => "zoom-in",
            Phase::ZoomOut => "zoom-out",
            Phase::Pan { .. } => "pan",
            Phase::Done => "done",
        }
    }

    /// Move the view one frame along the script.
    pub fn step(&mut self, view: &mut View, width: f32, n: u64) {
        let w = width as f64;
        let fit = n as f64 / w;
        let zoom = |view: &mut View, f: f64| {
            let centre = view.start + w / 2.0 * view.spp;
            view.spp *= f;
            view.start = centre - w / 2.0 * view.spp;
        };
        self.phase = match self.phase {
            Phase::Warmup(0) => {
                *view = View {
                    start: 0.0,
                    spp: fit,
                };
                Phase::ZoomIn
            }
            Phase::Warmup(k) => {
                *view = View {
                    start: 0.0,
                    spp: fit,
                };
                Phase::Warmup(k - 1)
            }
            Phase::ZoomIn => {
                zoom(view, 0.85);
                if view.spp <= 1.0 / 16.0 {
                    Phase::ZoomOut
                } else {
                    Phase::ZoomIn
                }
            }
            Phase::ZoomOut => {
                zoom(view, 1.0 / 0.85);
                if view.spp >= fit {
                    Phase::Pan {
                        spp: PAN_SPPS[0],
                        frames: PAN_FRAMES,
                        idx: 0,
                    }
                } else {
                    Phase::ZoomOut
                }
            }
            Phase::Pan { spp, frames, idx } => {
                if frames == PAN_FRAMES {
                    *view = View {
                        start: n as f64 * 0.1,
                        spp,
                    };
                } else {
                    view.start += w * spp * 0.05;
                }
                if frames > 0 {
                    Phase::Pan {
                        spp,
                        frames: frames - 1,
                        idx,
                    }
                } else if idx + 1 < PAN_SPPS.len() && PAN_SPPS[idx + 1] < fit {
                    Phase::Pan {
                        spp: PAN_SPPS[idx + 1],
                        frames: PAN_FRAMES,
                        idx: idx + 1,
                    }
                } else {
                    Phase::Done
                }
            }
            Phase::Done => Phase::Done,
        };
    }

    pub fn record(&mut self, (build_ms, cpu_ms): (f64, f64), stats: WaveStats) {
        if !matches!(self.phase, Phase::Warmup(_)) {
            let name = self.name();
            self.samples.push((build_ms, cpu_ms, stats, name));
        }
    }

    pub fn done(&self) -> bool {
        self.phase == Phase::Done
    }

    pub fn report(&mut self) {
        if self.reported {
            return;
        }
        self.reported = true;
        let stat = |v: &mut Vec<f64>| -> (f64, f64, f64) {
            if v.is_empty() {
                return (0.0, 0.0, 0.0);
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let avg = v.iter().sum::<f64>() / v.len() as f64;
            let p95 = v[((v.len() as f64 * 0.95) as usize).min(v.len() - 1)];
            (avg, p95, *v.last().unwrap())
        };
        // The CPU figure of a frame arrives one frame late; drop the first.
        let mut build: Vec<f64> = self.samples.iter().map(|s| s.0).collect();
        let mut cpu: Vec<f64> = self.samples.iter().skip(1).map(|s| s.1).collect();
        let (ba, bp, bm) = stat(&mut build);
        let (ca, cp, cm) = stat(&mut cpu);
        println!("bench frames {}", self.samples.len());
        println!("wave build ms: avg {ba:.2}  p95 {bp:.2}  max {bm:.2}");
        println!("frame cpu ms:  avg {ca:.2}  p95 {cp:.2}  max {cm:.2}");
        for mode in ["raw samples", "level 0", "level 1", "level 2", "level 3"] {
            let mut v: Vec<f64> = self
                .samples
                .iter()
                .filter(|s| {
                    let m = if s.2.sample_mode {
                        "raw samples".to_string()
                    } else {
                        format!("level {}", s.2.level)
                    };
                    m == mode
                })
                .map(|s| s.0)
                .collect();
            if !v.is_empty() {
                let n = v.len();
                let (a, p, m) = stat(&mut v);
                println!("  {mode:<12} frames {n:>4}  build avg {a:.2}  p95 {p:.2}  max {m:.2}");
            }
        }
    }
}
