//! Little-endian PTP dataset reader / writer.

use crate::codes::dt;
use crate::dataset::Value;
use crate::{Error, Result};

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(Error::Parse(format!(
                "need {n} bytes at offset {}, only {} left",
                self.pos,
                self.remaining()
            )));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn arr<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("length checked"))
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.arr::<1>()?[0])
    }
    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.u8()? as i8)
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.arr()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.arr()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.arr()?))
    }

    /// PTP string: u8 number of UTF-16 code units (including the terminating NUL).
    pub fn string(&mut self) -> Result<String> {
        let n = self.u8()? as usize;
        let mut units = Vec::with_capacity(n);
        for _ in 0..n {
            units.push(self.u16()?);
        }
        while units.last() == Some(&0) {
            units.pop();
        }
        Ok(String::from_utf16_lossy(&units))
    }

    pub fn u16_array(&mut self) -> Result<Vec<u16>> {
        let n = self.u32()? as usize;
        self.check_count(n, 2)?;
        (0..n).map(|_| self.u16()).collect()
    }

    pub fn u32_array(&mut self) -> Result<Vec<u32>> {
        let n = self.u32()? as usize;
        self.check_count(n, 4)?;
        (0..n).map(|_| self.u32()).collect()
    }

    fn check_count(&self, n: usize, elem: usize) -> Result<()> {
        if n.saturating_mul(elem) > self.remaining() {
            return Err(Error::Parse(format!(
                "array of {n} elements does not fit at offset {}",
                self.pos
            )));
        }
        Ok(())
    }

    /// Reads a single value of PTP data type `ty`.
    pub fn value(&mut self, ty: u16) -> Result<Value> {
        if ty == dt::STR {
            return Ok(Value::Str(self.string()?));
        }
        if ty & dt::ARRAY_MASK != 0 {
            let elem = ty & !dt::ARRAY_MASK;
            let n = self.u32()? as usize;
            self.check_count(n, scalar_size(elem).unwrap_or(1).max(1))?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                match self.value(elem)? {
                    Value::Int(v) => out.push(v),
                    other => return Err(Error::Parse(format!("unexpected array element {other:?}"))),
                }
            }
            return Ok(Value::Array(out));
        }
        Ok(Value::Int(match ty {
            dt::INT8 => self.i8()? as i64,
            dt::UINT8 => self.u8()? as i64,
            dt::INT16 => self.i16()? as i64,
            dt::UINT16 => self.u16()? as i64,
            dt::INT32 => self.i32()? as i64,
            dt::UINT32 => self.u32()? as i64,
            dt::INT64 => self.i64()?,
            dt::UINT64 => self.u64()? as i64,
            dt::INT128 | dt::UINT128 => {
                let b = self.bytes(16)?;
                i64::from_le_bytes(b[..8].try_into().expect("8 bytes"))
            }
            other => return Err(Error::Parse(format!("unsupported data type 0x{other:04X}"))),
        }))
    }
}

pub fn scalar_size(ty: u16) -> Option<usize> {
    Some(match ty {
        dt::INT8 | dt::UINT8 => 1,
        dt::INT16 | dt::UINT16 => 2,
        dt::INT32 | dt::UINT32 => 4,
        dt::INT64 | dt::UINT64 => 8,
        dt::INT128 | dt::UINT128 => 16,
        _ => return None,
    })
}

/// Encodes an integer as PTP data type `ty` (for SetControlDevice A/B).
pub fn encode_int(ty: u16, v: i64) -> Result<Vec<u8>> {
    Ok(match ty {
        dt::INT8 | dt::UINT8 => vec![v as u8],
        dt::INT16 | dt::UINT16 => (v as u16).to_le_bytes().to_vec(),
        dt::INT32 | dt::UINT32 => (v as u32).to_le_bytes().to_vec(),
        dt::INT64 | dt::UINT64 => (v as u64).to_le_bytes().to_vec(),
        other => return Err(Error::Parse(format!("cannot encode integer as type 0x{other:04X}"))),
    })
}

/// Encodes a PTP string.
pub fn encode_string(s: &str) -> Vec<u8> {
    let mut units: Vec<u16> = s.encode_utf16().collect();
    if units.is_empty() {
        return vec![0];
    }
    units.push(0);
    let mut out = vec![units.len().min(255) as u8];
    for u in units.iter().take(255) {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_roundtrip() {
        let enc = encode_string("ILCE-6300");
        let mut r = Reader::new(&enc);
        assert_eq!(r.string().unwrap(), "ILCE-6300");
        assert!(r.is_empty());
        assert_eq!(encode_string(""), vec![0]);
    }

    #[test]
    fn ints_and_arrays() {
        let buf = [2, 0, 0, 0, 0x01, 0x50, 0x07, 0x50, 0xFF];
        let mut r = Reader::new(&buf);
        assert_eq!(r.u16_array().unwrap(), vec![0x5001, 0x5007]);
        assert_eq!(r.value(dt::INT8).unwrap(), Value::Int(-1));
        assert!(r.u8().is_err());
    }

    #[test]
    fn rejects_huge_arrays() {
        let buf = [0xFF, 0xFF, 0xFF, 0x7F];
        assert!(Reader::new(&buf).u16_array().is_err());
    }

    #[test]
    fn encodes_signed() {
        assert_eq!(encode_int(dt::UINT8, -1).unwrap(), vec![0xFF]);
        assert_eq!(encode_int(dt::INT16, -3).unwrap(), vec![0xFD, 0xFF]);
    }
}
