/// Schema-level string pool: packed null-terminated UTF-8 strings, max 64 KB.
/// Offsets are u16, used by DUT properties, scope names, field names, etc.
use std::collections::HashMap;

/// Builds a string pool by inserting strings and returning their offsets.
#[derive(Debug, Clone)]
pub struct StringPoolBuilder {
    data: Vec<u8>,
    index: HashMap<String, u16>,
}

impl StringPoolBuilder {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Insert a string into the pool. Returns its u16 offset.
    /// If the string already exists, returns the existing offset.
    /// Panics if the pool exceeds 64 KB.
    pub fn insert(&mut self, s: &str) -> u16 {
        if let Some(&offset) = self.index.get(s) {
            return offset;
        }
        let offset = self.data.len();
        assert!(
            offset + s.len() + 1 <= u16::MAX as usize,
            "string pool exceeds 64 KB limit"
        );
        let offset_u16 = offset as u16;
        self.data.extend_from_slice(s.as_bytes());
        self.data.push(0); // null terminator
        self.index.insert(s.to_owned(), offset_u16);
        offset_u16
    }

    /// Returns the serialized string pool bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Returns the total size of the pool in bytes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl Default for StringPoolBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Reader for a string pool. Reads null-terminated strings at given offsets.
#[derive(Debug)]
pub struct StringPoolReader<'a> {
    data: &'a [u8],
}

impl<'a> StringPoolReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Read the null-terminated string at the given offset.
    pub fn get(&self, offset: u16) -> Option<&'a str> {
        let start = offset as usize;
        if start >= self.data.len() {
            return None;
        }
        let end = self.data[start..]
            .iter()
            .position(|&b| b == 0)
            .map(|i| start + i)?;
        std::str::from_utf8(&self.data[start..end]).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_read() {
        let mut builder = StringPoolBuilder::new();
        let off_hello = builder.insert("hello");
        let off_world = builder.insert("world");
        let off_hello2 = builder.insert("hello");

        assert_eq!(off_hello, 0);
        assert_eq!(off_world, 6); // "hello\0" = 6 bytes
        assert_eq!(off_hello2, off_hello); // dedup

        let reader = StringPoolReader::new(builder.data());
        assert_eq!(reader.get(off_hello), Some("hello"));
        assert_eq!(reader.get(off_world), Some("world"));
    }

    #[test]
    fn empty_string() {
        let mut builder = StringPoolBuilder::new();
        let off = builder.insert("");
        assert_eq!(off, 0);

        let reader = StringPoolReader::new(builder.data());
        assert_eq!(reader.get(off), Some(""));
    }

    #[test]
    fn out_of_bounds() {
        let builder = StringPoolBuilder::new();
        let reader = StringPoolReader::new(builder.data());
        assert_eq!(reader.get(0), None);
        assert_eq!(reader.get(100), None);
    }
}
