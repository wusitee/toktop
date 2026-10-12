//! Minimal protobuf wire-format reader for the few Antigravity fields we use.

#[derive(Debug, Clone, Copy)]
pub enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

/// Iterates over the top-level fields of one message. Stops at the first
/// malformed field.
pub struct Fields<'a> {
    buf: &'a [u8],
}

pub fn fields(buf: &[u8]) -> Fields<'_> {
    Fields { buf }
}

fn read_varint(buf: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = buf.split_first()?;
        *buf = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn take<'a>(buf: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    if buf.len() < len {
        return None;
    }
    let (head, rest) = buf.split_at(len);
    *buf = rest;
    Some(head)
}

impl<'a> Iterator for Fields<'a> {
    type Item = (u64, Value<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.buf.is_empty() {
            return None;
        }
        let parsed = (|| {
            let key = read_varint(&mut self.buf)?;
            let value = match key & 7 {
                0 => Value::Varint(read_varint(&mut self.buf)?),
                1 => Value::Fixed64(u64::from_le_bytes(take(&mut self.buf, 8)?.try_into().ok()?)),
                2 => {
                    let len = usize::try_from(read_varint(&mut self.buf)?).ok()?;
                    Value::Bytes(take(&mut self.buf, len)?)
                }
                5 => Value::Fixed32(u32::from_le_bytes(take(&mut self.buf, 4)?.try_into().ok()?)),
                _ => return None,
            };
            Some((key >> 3, value))
        })();
        if parsed.is_none() {
            self.buf = &[];
        }
        parsed
    }
}

/// First varint of a packed repeated field.
pub fn fields_packed_first(packed: &[u8]) -> Option<u64> {
    let mut buf = packed;
    read_varint(&mut buf)
}

pub fn varint(buf: &[u8], field: u64) -> Option<u64> {
    fields(buf).find_map(|(number, value)| match value {
        Value::Varint(v) if number == field => Some(v),
        _ => None,
    })
}

pub fn bytes(buf: &[u8], field: u64) -> Option<&[u8]> {
    fields(buf).find_map(|(number, value)| match value {
        Value::Bytes(b) if number == field => Some(b),
        _ => None,
    })
}

pub fn string(buf: &[u8], field: u64) -> Option<&str> {
    bytes(buf, field)
        .and_then(|b| std::str::from_utf8(b).ok())
        .filter(|text| !text.trim().is_empty())
}

/// `google.protobuf.Timestamp` message to epoch milliseconds.
pub fn timestamp_ms(buf: &[u8]) -> Option<i64> {
    let seconds = i64::try_from(varint(buf, 1)?).ok()?;
    let nanos = varint(buf, 2).unwrap_or(0) as i64;
    Some(seconds * 1000 + nanos / 1_000_000)
}
