/// LEB128 unsigned integer encoding/decoding.

/// Encode a u64 as unsigned LEB128 into the given buffer.
/// Returns the number of bytes written.
pub fn encode_u64(mut value: u64, buf: &mut [u8]) -> usize {
    let mut i = 0;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf[i] = byte;
        i += 1;
        if value == 0 {
            break;
        }
    }
    i
}

/// Encode a u64 as unsigned LEB128, appending to a Vec.
pub fn encode_u64_vec(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Decode an unsigned LEB128 value from a byte slice.
/// Returns (value, bytes_consumed).
pub fn decode_u64(data: &[u8]) -> Result<(u64, usize), DecodeError> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    for (i, &byte) in data.iter().enumerate() {
        if shift >= 64 {
            return Err(DecodeError::Overflow);
        }
        let low7 = (byte & 0x7F) as u64;
        result |= low7 << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return Ok((result, i + 1));
        }
    }
    Err(DecodeError::Incomplete)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Not enough bytes to complete the LEB128 value.
    Incomplete,
    /// Value would overflow u64.
    Overflow,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Incomplete => write!(f, "incomplete LEB128 encoding"),
            DecodeError::Overflow => write!(f, "LEB128 value overflows u64"),
        }
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_zero() {
        let mut buf = [0u8; 10];
        let n = encode_u64(0, &mut buf);
        assert_eq!(n, 1);
        assert_eq!(buf[0], 0);
        let (val, consumed) = decode_u64(&buf[..n]).unwrap();
        assert_eq!(val, 0);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn roundtrip_small() {
        let mut buf = [0u8; 10];
        let n = encode_u64(127, &mut buf);
        assert_eq!(n, 1);
        let (val, consumed) = decode_u64(&buf[..n]).unwrap();
        assert_eq!(val, 127);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn roundtrip_128() {
        let mut buf = [0u8; 10];
        let n = encode_u64(128, &mut buf);
        assert_eq!(n, 2);
        let (val, consumed) = decode_u64(&buf[..n]).unwrap();
        assert_eq!(val, 128);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn roundtrip_large() {
        let mut buf = [0u8; 10];
        for &val in &[1000u64, 65535, 1_000_000, u64::MAX] {
            let n = encode_u64(val, &mut buf);
            let (decoded, consumed) = decode_u64(&buf[..n]).unwrap();
            assert_eq!(decoded, val);
            assert_eq!(consumed, n);
        }
    }

    #[test]
    fn encode_vec() {
        let mut v = Vec::new();
        encode_u64_vec(300, &mut v);
        let (val, consumed) = decode_u64(&v).unwrap();
        assert_eq!(val, 300);
        assert_eq!(consumed, v.len());
    }

    #[test]
    fn decode_incomplete() {
        assert_eq!(decode_u64(&[0x80]), Err(DecodeError::Incomplete));
        assert_eq!(decode_u64(&[]), Err(DecodeError::Incomplete));
    }

    #[test]
    fn decode_overflow() {
        // 11 bytes with continuation bits set — would overflow u64
        let data = [0x80; 11];
        assert_eq!(decode_u64(&data), Err(DecodeError::Overflow));
    }
}
