//! A bit string in wire order: bit 0 is the first bit on the DAP1 line.
//! Packed LSB first, as FTDI MPSSE and CMSIS-DAP sequences carry it.

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Bits {
    bytes: Vec<u8>,
    len: usize,
}

impl Bits {
    pub fn new() -> Bits {
        Bits::default()
    }

    pub fn zeros(len: usize) -> Bits {
        Bits {
            bytes: vec![0; len.div_ceil(8)],
            len,
        }
    }

    /// All bits of `bytes`, LSB of the first byte first.
    pub fn from_bytes(bytes: &[u8]) -> Bits {
        Bits {
            bytes: bytes.to_vec(),
            len: bytes.len() * 8,
        }
    }

    /// The first `len` bits of `bytes`.
    pub fn from_bytes_len(bytes: &[u8], len: usize) -> Bits {
        let mut b = Bits::from_bytes(&bytes[..len.div_ceil(8)]);
        b.truncate(len);
        b
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, bit: bool) {
        if self.len.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if bit {
            self.bytes[self.len / 8] |= 1 << (self.len % 8);
        }
        self.len += 1;
    }

    /// The low `n` bits of `value`, least significant first.
    pub fn push_value(&mut self, value: u64, n: usize) {
        for i in 0..n {
            self.push(value >> i & 1 != 0);
        }
    }

    pub fn extend(&mut self, other: &Bits) {
        for bit in other.iter() {
            self.push(bit);
        }
    }

    pub fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        self.len = len;
        self.bytes.truncate(len.div_ceil(8));
        if !len.is_multiple_of(8) {
            let last = self.bytes.len() - 1;
            self.bytes[last] &= (1u8 << (len % 8)) - 1;
        }
    }

    pub fn get(&self, i: usize) -> bool {
        i < self.len && self.bytes[i / 8] >> (i % 8) & 1 != 0
    }

    /// `n` bits (at most 64) from `start`, as a number; bits past the end
    /// read as zero.
    pub fn value(&self, start: usize, n: usize) -> u64 {
        (0..n.min(64)).fold(0, |v, i| v | (self.get(start + i) as u64) << i)
    }

    pub fn slice(&self, start: usize, end: usize) -> Bits {
        let mut out = Bits::new();
        for i in start..end.min(self.len) {
            out.push(self.get(i));
        }
        out
    }

    pub fn iter(&self) -> impl Iterator<Item = bool> + '_ {
        (0..self.len).map(|i| self.get(i))
    }

    /// The position of the first 1 bit at or after `from`.
    pub fn find_one(&self, from: usize) -> Option<usize> {
        (from..self.len).find(|&i| self.get(i))
    }

    /// The packed bytes; unused high bits of the last byte are zero.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl std::fmt::Debug for Bits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bits({}: ", self.len)?;
        for bit in self.iter() {
            f.write_str(if bit { "1" } else { "0" })?;
        }
        f.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_pack_lsb_first() {
        let mut b = Bits::new();
        b.push(true);
        b.push_value(0b10110, 5);
        b.push_value(0xA5, 8);
        assert_eq!(b.len(), 14);
        assert_eq!(b.as_bytes(), &[0b0110_1101, 0b0010_1001]);
        assert_eq!(b.value(1, 5), 0b10110);
        assert_eq!(b.value(6, 8), 0xA5);
        assert_eq!(b.find_one(1), Some(2));
        assert_eq!(Bits::from_bytes(&[0, 0x10]).find_one(0), Some(12));
        assert_eq!(Bits::zeros(9).find_one(0), None);
    }

    #[test]
    fn truncate_clears_the_tail() {
        let mut b = Bits::from_bytes(&[0xFF, 0xFF]);
        b.truncate(11);
        assert_eq!(b.as_bytes(), &[0xFF, 0x07]);
        assert_eq!(
            Bits::from_bytes_len(&[0xFF, 0xFF, 0xFF], 3).as_bytes(),
            &[0x07]
        );
    }
}
