//! Zoom summaries (STO-3): for every block of samples, the bitwise OR and AND
//! of the 8-channel bytes. OR bit = the channel was high somewhere in the
//! block, AND bit = it was high everywhere, so per channel
//! (AND=1) -> stayed high, (OR=0) -> stayed low, otherwise it changed.

/// Samples per entry at each level. Level 0 is the raw data.
pub const LEVEL_BLOCK: [u64; 4] = [1, 64, 4096, 262_144];
/// Each level combines this many entries of the level below.
pub const LEVEL_FACTOR: usize = 64;
pub const MAX_LEVEL: usize = 3;

/// One summary entry: `[or, and]`.
pub type Entry = [u8; 2];

pub const EMPTY: Entry = [0x00, 0xff];

#[inline]
pub fn merge(a: Entry, b: Entry) -> Entry {
    [a[0] | b[0], a[1] & b[1]]
}

/// Summarize raw samples in blocks of `block` (the last block may be short).
pub fn summarize_raw(data: &[u8], block: usize) -> Vec<Entry> {
    data.chunks(block)
        .map(|c| {
            let mut or = 0u8;
            let mut and = 0xffu8;
            for &b in c {
                or |= b;
                and &= b;
            }
            [or, and]
        })
        .collect()
}

/// Combine entries `factor` at a time into the next level.
pub fn combine(entries: &[Entry], factor: usize) -> Vec<Entry> {
    entries
        .chunks(factor)
        .map(|c| c.iter().copied().fold(EMPTY, merge))
        .collect()
}

/// Flatten entries into the on-disk blob `[or, and, or, and, ...]`.
pub fn to_blob(entries: &[Entry]) -> Vec<u8> {
    entries.iter().flatten().copied().collect()
}

pub fn from_blob(blob: &[u8]) -> Vec<Entry> {
    blob.as_chunks::<2>().0.to_vec()
}

/// Per-channel state of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelState {
    Low,
    High,
    Changing,
}

#[inline]
pub fn channel_state(e: Entry, ch: u8) -> ChannelState {
    let m = 1u8 << ch;
    if e[1] & m != 0 {
        ChannelState::High
    } else if e[0] & m == 0 {
        ChannelState::Low
    } else {
        ChannelState::Changing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sto3_summary_states() {
        let data = [0b0000_0011u8, 0b0000_0001, 0b0000_0011, 0b0000_0001];
        let e = summarize_raw(&data, 4)[0];
        assert_eq!(channel_state(e, 0), ChannelState::High);
        assert_eq!(channel_state(e, 1), ChannelState::Changing);
        assert_eq!(channel_state(e, 2), ChannelState::Low);
    }

    #[test]
    fn sto3_levels_agree_with_raw() {
        // Pseudo-random data: summaries built level by level must equal a
        // summary built straight from the raw samples.
        let mut x = 0x1234_5678u32;
        let data: Vec<u8> = (0..300_000)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                // channel 7 constant high, channel 6 constant low
                ((x as u8) & 0b0011_1111) | 0b1000_0000 | u8::from(i % 1000 == 0)
            })
            .collect();
        let l1 = summarize_raw(&data, 64);
        let l2 = combine(&l1, 64);
        let l3 = combine(&l2, 64);
        assert_eq!(l2, summarize_raw(&data, 4096));
        assert_eq!(l3, summarize_raw(&data, 262_144));
        assert_eq!(from_blob(&to_blob(&l2)), l2);
        for e in l3 {
            assert_eq!(channel_state(e, 7), ChannelState::High);
            assert_eq!(channel_state(e, 6), ChannelState::Low);
        }
    }
}
