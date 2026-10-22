//! Hand-encoded `CastMessage` protobuf, so casting needs no protobuf toolchain.
//! Fields: protocol version (1), source (2), destination (3), namespace (4),
//! payload type (5) and the UTF-8 payload (6).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastMessage {
    pub source: String,
    pub destination: String,
    pub namespace: String,
    pub payload: String,
}

pub fn encode(message: &CastMessage) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.payload.len() + 96);
    varint_field(&mut out, 1, 0);
    string_field(&mut out, 2, &message.source);
    string_field(&mut out, 3, &message.destination);
    string_field(&mut out, 4, &message.namespace);
    varint_field(&mut out, 5, 0);
    string_field(&mut out, 6, &message.payload);
    out
}

/// `None` for binary payloads and malformed input.
pub fn decode(bytes: &[u8]) -> Option<CastMessage> {
    let mut pos = 0;
    let (mut source, mut destination, mut namespace, mut payload) = (String::new(), String::new(), String::new(), None);
    while pos < bytes.len() {
        let tag = read_varint(bytes, &mut pos)?;
        match tag & 7 {
            0 => {
                read_varint(bytes, &mut pos)?;
            }
            1 => pos = pos.checked_add(8).filter(|p| *p <= bytes.len())?,
            5 => pos = pos.checked_add(4).filter(|p| *p <= bytes.len())?,
            2 => {
                let len = usize::try_from(read_varint(bytes, &mut pos)?).ok()?;
                let end = pos.checked_add(len).filter(|e| *e <= bytes.len())?;
                let value = String::from_utf8_lossy(&bytes[pos..end]).into_owned();
                pos = end;
                match tag >> 3 {
                    2 => source = value,
                    3 => destination = value,
                    4 => namespace = value,
                    6 => payload = Some(value),
                    _ => {}
                }
            }
            _ => return None,
        }
    }
    payload.map(|payload| CastMessage { source, destination, namespace, payload })
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn varint_field(out: &mut Vec<u8>, field: u64, value: u64) {
    write_varint(out, field << 3);
    write_varint(out, value);
}

fn string_field(out: &mut Vec<u8>, field: u64, value: &str) {
    write_varint(out, field << 3 | 2);
    write_varint(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

fn read_varint(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        result |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(payload: &str) -> CastMessage {
        CastMessage { source: "s".into(), destination: "d".into(), namespace: "n".into(), payload: payload.into() }
    }

    #[test]
    fn round_trips() {
        let m = message(&("x".repeat(5_000) + "ë"));
        assert_eq!(decode(&encode(&m)), Some(m));
    }

    #[test]
    fn matches_protobuf_layout() {
        assert_eq!(
            encode(&message("p")),
            vec![0x08, 0x00, 0x12, 0x01, b's', 0x1a, 0x01, b'd', 0x22, 0x01, b'n', 0x28, 0x00, 0x32, 0x01, b'p']
        );
    }

    #[test]
    fn rejects_binary_and_truncated_input() {
        assert_eq!(decode(&[0x08, 0x00, 0x28, 0x01, 0x3a, 0x01, 0x00]), None);
        assert_eq!(decode(&[0x12, 0x7f]), None);
    }
}
