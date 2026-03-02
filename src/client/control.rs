use serde_json::Value;
use thiserror::Error;

use crate::protocol::framing::{decode_frame, encode_frame};

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("json error: {0}")]
    Json(String),
}

pub fn encode_control_json(value: &Value) -> Result<Vec<u8>, ControlError> {
    let payload = serde_json::to_vec(value).map_err(|e| ControlError::Json(e.to_string()))?;
    Ok(encode_frame(&payload))
}

pub fn decode_control_json(frame: &[u8], max_size: usize) -> Result<Value, ControlError> {
    let payload =
        decode_frame(frame, max_size).map_err(|e| ControlError::Protocol(e.to_string()))?;
    serde_json::from_slice(payload).map_err(|e| ControlError::Json(e.to_string()))
}

pub fn message_type(value: &Value) -> Option<&str> {
    value.get("type").and_then(|v| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn control_frame_round_trip() {
        let msg = json!({"type":"PING","nonce":"n"});
        let frame = encode_control_json(&msg).unwrap();
        let decoded = decode_control_json(&frame, 4096).unwrap();
        assert_eq!(message_type(&decoded), Some("PING"));
    }
}
