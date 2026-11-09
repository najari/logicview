//! Software trigger (CAP-8): an AND of per-channel conditions, checked on
//! the writer thread. Samples before the trigger are kept in a ring buffer
//! so the capture can start some way before it.

use std::collections::VecDeque;

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
                .filter(|&c| c < 8)
                .ok_or(format!("'{part}': channel must be 0..7"))?;
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
    level_mask: u8,
    level_value: u8,
    rise: u8,
    fall: u8,
    edge: u8,
    prev: Option<u8>,
}

impl Matcher {
    pub fn new(t: &Trigger) -> Matcher {
        let mut m = Matcher {
            level_mask: 0,
            level_value: 0,
            rise: 0,
            fall: 0,
            edge: 0,
            prev: None,
        };
        for &(c, cond) in &t.conditions {
            let bit = 1u8 << c;
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

    fn hit(&self, prev: u8, cur: u8) -> bool {
        cur & self.level_mask == self.level_value
            && !prev & cur & self.rise == self.rise
            && prev & !cur & self.fall == self.fall
            && (prev ^ cur) & self.edge == self.edge
    }

    /// Index in `data` of the trigger sample, carrying the last sample
    /// across calls so edges between blocks count.
    pub fn find(&mut self, data: &[u8]) -> Option<usize> {
        let &last = data.last()?;
        let needs_edge = self.rise | self.fall | self.edge != 0;
        let mut start = 0;
        let mut prev = match self.prev {
            Some(p) => p,
            None if needs_edge => {
                // An edge needs a sample before it.
                start = 1;
                data[0]
            }
            None => data[0], // a level can fire on the very first sample
        };
        for (i, &cur) in data.iter().enumerate().skip(start) {
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

/// Keeps the newest `cap` samples seen before the trigger.
#[derive(Debug)]
pub struct PreBuffer {
    cap: usize,
    buf: VecDeque<u8>,
}

impl PreBuffer {
    pub fn new(cap: usize) -> PreBuffer {
        PreBuffer {
            cap,
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

    pub fn len(&self) -> usize {
        self.buf.len()
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
        let mut m = Matcher::new(&Trigger::parse(t, 0).unwrap());
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
        for bad in ["", "8=r", "0=x", "0=r,0=f", "0r"] {
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
        let mut p = PreBuffer::new(5);
        p.push(&[1, 2, 3]);
        p.push(&[4, 5, 6, 7]);
        let (a, b) = p.as_slices();
        assert_eq!([a, b].concat(), vec![3, 4, 5, 6, 7]);
        p.push(&[10, 11, 12, 13, 14, 15, 16]);
        let (a, b) = p.as_slices();
        assert_eq!([a, b].concat(), vec![12, 13, 14, 15, 16]);
        let mut z = PreBuffer::new(0);
        z.push(&[1, 2]);
        assert!(z.is_empty());
    }
}
