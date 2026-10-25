//! Save states: the whole console written out as bytes, and read back.
//!
//! Each part of the console has a `save_state` that writes its fields in a
//! fixed order and a `load_state` that reads them back in the same order.
//! Only what changes while a game runs is saved: not the ROM, and not what
//! follows from the region or the cartridge (those are rebuilt instead).
//!
//! The reader never panics on a short or damaged file: it notes the
//! problem, hands back zeros, and `finish` reports it (`Nes::load_state`
//! then puts the console back as it was).

use crate::cartridge::Mirroring;

pub struct StateWriter {
    data: Vec<u8>
}

impl StateWriter {
    pub fn new() -> Self {
        StateWriter { data: Vec::new() }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    pub fn u8(&mut self, value: u8) {
        self.data.push(value);
    }

    pub fn bool(&mut self, value: bool) {
        self.u8(value as u8);
    }

    pub fn u16(&mut self, value: u16) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u64(&mut self, value: u64) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub fn usize(&mut self, value: usize) {
        self.u64(value as u64);
    }

    pub fn f32(&mut self, value: f32) {
        self.u32(value.to_bits());
    }

    pub fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }

    pub fn option_u8(&mut self, value: Option<u8>) {
        self.bool(value.is_some());
        self.u8(value.unwrap_or(0));
    }

    pub fn option_u64(&mut self, value: Option<u64>) {
        self.bool(value.is_some());
        self.u64(value.unwrap_or(0));
    }

    pub fn bytes(&mut self, bytes: &[u8]) {
        self.u32(bytes.len() as u32);
        self.data.extend_from_slice(bytes);
    }

    pub fn mirroring(&mut self, mirroring: Mirroring) {
        self.u8(match mirroring {
            Mirroring::Vertical => 0,
            Mirroring::Horizontal => 1,
            Mirroring::FourScreen => 2,
            Mirroring::SingleScreenLower => 3,
            Mirroring::SingleScreenUpper => 4,
        });
    }
}

impl Default for StateWriter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct StateReader<'a> {
    data: &'a [u8],
    pos: usize,
    error: Option<String>
}

impl<'a> StateReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        StateReader {
            data,
            pos: 0,
            error: None,
        }
    }

    fn fail(&mut self, error: String) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        match self.data.get(self.pos..self.pos + N) {
            Some(bytes) => {
                out.copy_from_slice(bytes);
                self.pos += N;
            }
            None => self.fail("the save state is cut short".into()),
        }
        out
    }

    pub fn u8(&mut self) -> u8 {
        self.take::<1>()[0]
    }

    pub fn bool(&mut self) -> bool {
        self.u8() != 0
    }

    pub fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }

    pub fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }

    pub fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }

    pub fn usize(&mut self) -> usize {
        self.u64() as usize
    }

    pub fn f32(&mut self) -> f32 {
        f32::from_bits(self.u32())
    }

    pub fn f64(&mut self) -> f64 {
        f64::from_bits(self.u64())
    }

    pub fn option_u8(&mut self) -> Option<u8> {
        let some = self.bool();
        let value = self.u8();
        some.then_some(value)
    }

    pub fn option_u64(&mut self) -> Option<u64> {
        let some = self.bool();
        let value = self.u64();
        some.then_some(value)
    }

    pub fn bytes_into(&mut self, out: &mut [u8]) {
        let len = self.u32() as usize;
        if len != out.len() {
            self.fail(format!(
                "a block of {len} bytes in the save state should be {} bytes",
                out.len()
            ));
            return;
        }
        match self.data.get(self.pos..self.pos + len) {
            Some(bytes) => {
                out.copy_from_slice(bytes);
                self.pos += len;
            }
            None => self.fail("the save state is cut short".into()),
        }
    }

    pub fn mirroring(&mut self) -> Mirroring {
        match self.u8() {
            0 => Mirroring::Vertical,
            1 => Mirroring::Horizontal,
            2 => Mirroring::FourScreen,
            3 => Mirroring::SingleScreenLower,
            4 => Mirroring::SingleScreenUpper,
            n => {
                self.fail(format!("unknown mirroring {n} in the save state"));
                Mirroring::Horizontal
            }
        }
    }

    pub fn finish(self) -> Result<(), String> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.pos != self.data.len() {
            return Err("the save state has extra data at the end".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_values_read_back_in_order() {
        let mut w = StateWriter::new();
        w.u8(0xAB);
        w.bool(true);
        w.u16(0x1234);
        w.u64(u64::MAX - 5);
        w.f32(-0.25);
        w.f64(1.0 / 3.0);
        w.option_u8(None);
        w.option_u64(Some(77));
        w.bytes(&[1, 2, 3]);
        w.mirroring(Mirroring::SingleScreenUpper);
        let data = w.into_bytes();

        let mut r = StateReader::new(&data);
        assert_eq!(r.u8(), 0xAB);
        assert!(r.bool());
        assert_eq!(r.u16(), 0x1234);
        assert_eq!(r.u64(), u64::MAX - 5);
        assert_eq!(r.f32(), -0.25);
        assert_eq!(r.f64(), 1.0 / 3.0);
        assert_eq!(r.option_u8(), None);
        assert_eq!(r.option_u64(), Some(77));
        let mut block = [0; 3];
        r.bytes_into(&mut block);
        assert_eq!(block, [1, 2, 3]);
        assert_eq!(r.mirroring(), Mirroring::SingleScreenUpper);
        assert_eq!(r.finish(), Ok(()));
    }

    #[test]
    fn test_short_or_long_data_is_an_error() {
        let mut r = StateReader::new(&[1, 2]);
        assert_eq!(r.u32(), 0);
        assert!(r.finish().unwrap_err().contains("cut short"));

        let mut r = StateReader::new(&[1, 2]);
        r.u8();
        assert!(r.finish().unwrap_err().contains("extra"));

        let mut w = StateWriter::new();
        w.bytes(&[0; 4]);
        let data = w.into_bytes();
        let mut r = StateReader::new(&data);
        let mut wrong_size = [0; 8];
        r.bytes_into(&mut wrong_size);
        assert!(r.finish().is_err());
    }
}