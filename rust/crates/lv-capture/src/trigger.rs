//! Software trigger (CAP-8): an AND of per-channel conditions, checked on
//! the writer thread. Samples before the trigger are kept in a ring buffer
//! so the capture can start some way before it.

use std::collections::VecDeque;

use lv_store::{MAX_CHANNELS, sample_at};

/// Condition on one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    Rising,
    Falling,
    Edge,
    High,
    Low,
}

impl Cond {
    fn parse(s: &str) -> Option<Cond> {
        Some(match s.to_ascii_lowercase().as_str() {
            "r" | "rise" | "rising" => Cond::Rising,
            "f" | "fall" | "falling" => Cond::Falling,
            "e" | "edge" | "c" | "change" => Cond::Edge,
            "1" | "h" | "high" => Cond::High,
            "0" | "l" | "low" => Cond::Low,
            _ => return None,
        })
    }

    fn code(self) -> char {
        match self {
            Cond::Rising => 'r',
            Cond::Falling => 'f',
            Cond::Edge => 'e',
            Cond::High => '1',
            Cond::Low => '0',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// (channel, condition), at most one per channel.
    pub conditions: Vec<(usize, Cond)>,
    /// Share of the capture before the trigger, 0..=100 (needs a sample
    /// count; with a manual stop the capture starts at the trigger).
    pub pre_percent: u8,
}

impl Trigger {
    /// Parse `0=r,3=1` (sigrok-cli style; `D0=r` also works). Conditions:
    /// `r` rising, `f` falling, `e` either edge, `1` high, `0` low.
    pub fn parse(spec: &str, pre_percent: u8) -> Result<Trigger, String> {
        if pre_percent > 100 {
            return Err(format!("pre-trigger share {pre_percent}% is over 100%"));
        }
        let mut conditions: Vec<(usize, Cond)> = Vec::new();
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (ch, cond) = part
                .split_once('=')
                .ok_or(format!("'{part}': write channel=condition, e.g. 0=r"))?;
            let ch = ch.trim();
            let ch = ch
                .strip_prefix(['D', 'd'])
                .unwrap_or(ch)
                .parse::<usize>()
                .ok()
                .filter(|&c| c < MAX_CHANNELS)
                .ok_or(format!("'{part}': channel must be 0..{}", MAX_CHANNELS - 1))?;
            let cond = Cond::parse(cond.trim()).ok_or(format!(
                "'{part}': condition must be r (rising), f (falling), e (edge), 1 (high) or 0 (low)"
            ))?;
            if conditions.iter().any(|&(c, _)| c == ch) {
                return Err(format!("channel {ch} has two conditions"));
            }
            conditions.push((ch, cond));
        }
        if conditions.is_empty() {
            return Err("trigger has no conditions".into());
        }
        conditions.sort_by_key(|&(c, _)| c);
        Ok(Trigger {
            conditions,
            pre_percent,
        })
    }

    /// The highest channel a condition is on.
    pub fn max_channel(&self) -> usize {
        self.conditions.iter().map(|&(c, _)| c).max().unwrap_or(0)
    }

    /// Canonical text form, e.g. `0=r,3=1`.
    pub fn spec(&self) -> String {
        self.conditions
            .iter()
            .map(|&(c, k)| format!("{c}={}", k.code()))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// `capture.trigger_json` once the trigger fired at file sample `sample`.
    pub fn to_json(&self, sample: u64) -> String {
        format!(
            "{{\"conditions\":\"{}\",\"pre_percent\":{},\"sample\":{sample}}}",
            self.spec(),
            self.pre_percent
        )
    }
}

/// Finds the first sample that meets every condition.
#[derive(Debug, Clone)]
pub struct Matcher {
    /// Bytes per sample.
    unit: usize,
    level_mask: u16,
    level_value: u16,
    rise: u16,
    fall: u16,
    edge: u16,
    prev: Option<u16>,
}

impl Matcher {
    /// A matcher for samples of `unit` bytes.
    pub fn new(t: &Trigger, unit: usize) -> Matcher {
        let mut m = Matcher {
            unit,
            level_mask: 0,
            level_value: 0,
            rise: 0,
            fall: 0,
            edge: 0,
            prev: None,
        };
        for &(c, cond) in &t.conditions {
            let bit = 1u16 << c;
            match cond {
                Cond::High => {
                    m.level_mask |= bit;
                    m.level_value |= bit;
                }
                Cond::Low => m.level_mask |= bit,
                Cond::Rising => m.rise |= bit,
                Cond::Falling => m.fall |= bit,
                Cond::Edge => m.edge |= bit,
            }
        }
        m
    }

    fn hit(&self, prev: u16, cur: u16) -> bool {
        cur & self.level_mask == self.level_value
            && !prev & cur & self.rise == self.rise
            && prev & !cur & self.fall == self.fall
            && (prev ^ cur) & self.edge == self.edge
    }

    /// Sample index in `data` of the trigger sample, carrying the last
    /// sample across calls so edges between blocks count.
    pub fn find(&mut self, data: &[u8]) -> Option<usize> {
        let n = data.len() / self.unit;
        if n == 0 {
            return None;
        }
        let at = |i| sample_at(data, i, self.unit);
        let last = at(n - 1);
        let needs_edge = self.rise | self.fall | self.edge != 0;
        let mut start = 0;
        let mut prev = match self.prev {
            Some(p) => p,
            None if needs_edge => {
                // An edge needs a sample before it.
                start = 1;
                at(0)
            }
            None => at(0), // a level can fire on the very first sample
        };
        for i in start..n {
            let cur = at(i);
            if self.hit(prev, cur) {
                self.prev = Some(cur);
                return Some(i);
            }
            prev = cur;
        }
        self.prev = Some(last);
        None
    }
}

/// Keeps the newest samples seen before the trigger (whole samples are
/// pushed, so the bytes stay aligned).
#[derive(Debug)]
pub struct PreBuffer {
    /// Bytes kept.
    cap: usize,
    unit: usize,
    buf: VecDeque<u8>,
}

impl PreBuffer {
    /// Keep `samples` samples of `unit` bytes.
    pub fn new(samples: usize, unit: usize) -> PreBuffer {
        let cap = samples * unit;
        PreBuffer {
            cap,
            unit,
            buf: VecDeque::with_capacity(cap.min(64 << 20)),
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        if self.cap == 0 {
            return;
        }
        let data = &data[data.len().saturating_sub(self.cap)..];
        let over = (self.buf.len() + data.len()).saturating_sub(self.cap);
        self.buf.drain(..over);
        self.buf.extend(data);
    }

    /// Samples kept.
    pub fn len(&self) -> usize {
        self.buf.len() / self.unit
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The kept samples, oldest first, as up to two slices.
    pub fn as_slices(&self) -> (&[u8], &[u8]) {
        self.buf.as_slices()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_all(t: &str, blocks: &[&[u8]]) -> Option<usize> {
        let mut m = Matcher::new(&Trigger::parse(t, 0).unwrap(), 1);
        let mut base = 0;
        for b in blocks {
            if let Some(i) = m.find(b) {
                return Some(base + i);
            }
            base += b.len();
        }
        None
    }

    #[test]
    fn cap8_parse() {
        let t = Trigger::parse("D3=1, 0=r", 10).unwrap();
        assert_eq!(t.conditions, vec![(0, Cond::Rising), (3, Cond::High)]);
        assert_eq!(t.spec(), "0=r,3=1");
        assert_eq!(
            t.to_json(42),
            r#"{"conditions":"0=r,3=1","pre_percent":10,"sample":42}"#
        );
        assert_eq!(Trigger::parse("15=f", 0).unwrap().max_channel(), 15);
        for bad in ["", "16=r", "0=x", "0=r,0=f", "0r"] {
            assert!(Trigger::parse(bad, 0).is_err(), "{bad}");
        }
        assert!(Trigger::parse("0=r", 101).is_err());
    }

    #[test]
    fn cap8_conditions() {
        let d = [0b00u8, 0b00, 0b01, 0b11, 0b10, 0b00, 0b01];
        assert_eq!(find_all("0=r", &[&d]), Some(2));
        assert_eq!(find_all("0=f", &[&d]), Some(4));
        assert_eq!(find_all("1=e", &[&d]), Some(3));
        assert_eq!(find_all("1=1", &[&d]), Some(3));
        assert_eq!(find_all("0=0", &[&d]), Some(0)); // level fires at once
        assert_eq!(find_all("0=r,1=1", &[&d]), None); // never together
        assert_eq!(find_all("1=r,0=1", &[&d]), Some(3));
        assert_eq!(find_all("1=f,0=0", &[&d]), Some(5));
        assert_eq!(find_all("2=1", &[&d]), None);
        // An edge needs a sample before it: a high first sample is not a rise.
        assert_eq!(find_all("0=r", &[&[1, 1, 0, 1]]), Some(3));
    }

    #[test]
    fn cap8_edge_across_blocks() {
        assert_eq!(find_all("0=r", &[&[0, 0], &[1, 1]]), Some(2));
        assert_eq!(find_all("0=f", &[&[1], &[], &[0]]), Some(1));
    }

    #[test]
    fn cap8_pre_buffer_keeps_newest() {
        let mut p = PreBuffer::new(5, 1);
        p.push(&[1, 2, 3]);
        p.push(&[4, 5, 6, 7]);
        let (a, b) = p.as_slices();
        assert_eq!([a, b].concat(), vec![3, 4, 5, 6, 7]);
        p.push(&[10, 11, 12, 13, 14, 15, 16]);
        let (a, b) = p.as_slices();
        assert_eq!([a, b].concat(), vec![12, 13, 14, 15, 16]);
        let mut z = PreBuffer::new(0, 1);
        z.push(&[1, 2]);
        assert!(z.is_empty());
        // Two-byte samples: three kept, the oldest whole sample dropped.
        let mut w = PreBuffer::new(3, 2);
        w.push(&[1, 0, 2, 0]);
        w.push(&[3, 0, 4, 1]);
        assert_eq!(w.len(), 3);
        let (a, b) = w.as_slices();
        assert_eq!([a, b].concat(), vec![2, 0, 3, 0, 4, 1]);
    }

    #[test]
    fn ch16_trigger_on_upper_channels() {
        // D12 rises at sample 3; D9 is high from sample 2.
        let s: [u16; 5] = [0x0000, 0x0000, 0x0200, 0x1200, 0x1200];
        let data: Vec<u8> = s.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut m = Matcher::new(&Trigger::parse("12=r,9=1", 0).unwrap(), 2);
        assert_eq!(m.find(&data[..4]), None);
        assert_eq!(m.find(&data[4..]), Some(1)); // sample 3 overall
        let mut m = Matcher::new(&Trigger::parse("9=r", 0).unwrap(), 2);
        assert_eq!(m.find(&data), Some(2));
    }
}
