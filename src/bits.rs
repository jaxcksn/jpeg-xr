//! Bit reader (most significant bit first)

use crate::Error;

/// A code table: entry `peek8` holds `value << 4 | length`, or 0 where no code starts.
pub(crate) type Lut = [u8; 256];

/// Builds a [`Lut`] from codes listed by value, each a string of `0` and `1`, none longer than 8 bits.
pub(crate) const fn lut(codes: &[&str]) -> Lut {
    let mut t = [0u8; 256];
    let mut v = 0;
    while v < codes.len() {
        let c = codes[v].as_bytes();
        let mut code = 0usize;
        let mut i = 0;
        while i < c.len() {
            code = code * 2 + (c[i] - b'0') as usize;
            i += 1;
        }
        let len = c.len();
        let start = code << (8 - len);
        let mut j = 0;
        while j < 1 << (8 - len) {
            t[start + j] = ((v as u8) << 4) | len as u8;
            j += 1;
        }
        v += 1;
    }
    t
}

pub(crate) struct Bits<'a> {
    data: &'a [u8],
    /// In bits: 64 wide, so no slice's length in bits wraps on 32-bit targets.
    pos: u64,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    fn left(&self) -> u64 {
        self.data.len() as u64 * 8 - self.pos
    }

    /// The next 32 bits, zero past the end.
    #[inline]
    fn peek32(&self) -> u32 {
        let at = (self.pos >> 3) as usize;
        let mut v = 0u64;
        for i in 0..5 {
            v = (v << 8) | u64::from(self.data.get(at + i).copied().unwrap_or(0));
        }
        (((v << (self.pos & 7)) >> 8) & 0xFFFF_FFFF) as u32
    }

    /// `n` bits, `n <= 32`, as an unsigned number.
    #[inline]
    pub fn read(&mut self, n: u32) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        if self.left() < u64::from(n) {
            return Err(Error::Truncated);
        }
        let v = self.peek32() >> (32 - n);
        self.pos += u64::from(n);
        Ok(v)
    }

    #[inline]
    pub fn bit(&mut self) -> Result<bool, Error> {
        Ok(self.read(1)? != 0)
    }

    /// A symbol of the code table `t`.
    #[inline]
    pub fn vlc(&mut self, t: &Lut) -> Result<usize, Error> {
        let e = t[(self.peek32() >> 24) as usize];
        let len = u64::from(e & 15);
        if len == 0 {
            return Err(Error::Invalid("no such code"));
        }
        if self.left() < len {
            return Err(Error::Truncated);
        }
        self.pos += len;
        Ok(usize::from(e >> 4))
    }

    pub fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    /// The byte position, after [`Bits::align`].
    pub fn byte_pos(&self) -> usize {
        (self.pos >> 3) as usize
    }

    pub fn skip_bytes(&mut self, n: u64) -> Result<(), Error> {
        if self.left() / 8 < n {
            return Err(Error::Truncated);
        }
        self.pos += n * 8;
        Ok(())
    }

    /// VLW_ESC( ): a variable-length byte count.
    pub fn vlw(&mut self) -> Result<u64, Error> {
        let first = self.read(8)?;
        Ok(match first {
            0..=0xFA => u64::from(first) << 8 | u64::from(self.read(8)?),
            0xFB => u64::from(self.read(32)?),
            0xFC => u64::from(self.read(32)?) << 32 | u64::from(self.read(32)?),
            _ => 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_across_bytes_and_stops_at_the_end() {
        let mut b = Bits::new(&[0b1011_0011, 0b0101_1111]);
        assert_eq!(b.read(3).unwrap(), 0b101);
        assert_eq!(b.read(7).unwrap(), 0b100_1101);
        assert_eq!(b.read(6).unwrap(), 0b01_1111);
        assert_eq!(b.read(1), Err(Error::Truncated));
    }

    #[test]
    fn codes_decode_by_prefix() {
        const T: Lut = lut(&["10", "001", "00001", "0001", "11", "010", "00000", "011"]);
        let mut b = Bits::new(&[0b1000_1000, 0b0100_0000]);
        assert_eq!(b.vlc(&T).unwrap(), 0);
        assert_eq!(b.vlc(&T).unwrap(), 1);
        assert_eq!(b.vlc(&T).unwrap(), 2);
    }
}
