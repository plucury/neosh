use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FramingError {
    #[error("frame too short")]
    TooShort,
    #[error("frame length mismatch")]
    LengthMismatch,
    #[error("frame too large")]
    TooLarge,
}

pub fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u32;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn decode_frame(buf: &[u8], max_payload_len: usize) -> Result<&[u8], FramingError> {
    if buf.len() < 4 {
        return Err(FramingError::TooShort);
    }

    let payload_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if payload_len > max_payload_len {
        return Err(FramingError::TooLarge);
    }

    let expected = 4 + payload_len;
    if buf.len() != expected {
        return Err(FramingError::LengthMismatch);
    }

    Ok(&buf[4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_frame_codec() {
        let payload = br#"{"type":"PING","nonce":"n"}"#;
        let frame = encode_frame(payload);
        let decoded = decode_frame(&frame, 1024).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn rejects_length_mismatch() {
        let bad = vec![0, 0, 0, 5, b'x'];
        assert_eq!(decode_frame(&bad, 1024), Err(FramingError::LengthMismatch));
    }

    #[test]
    fn rejects_oversized_payload() {
        let mut frame = vec![0, 0, 16, 0];
        frame.extend_from_slice(&[0u8; 16]);
        assert_eq!(decode_frame(&frame, 15), Err(FramingError::TooLarge));
    }
}
