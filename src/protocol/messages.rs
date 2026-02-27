use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Hello,
    Auth,
    Attach,
    Resume,
    Resize,
    Detach,
    Close,
    Ping,
    Pong,
    Error,
}

#[derive(Debug, Deserialize)]
pub struct ControlEnvelope {
    #[serde(rename = "type")]
    pub msg_type: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorMessage {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

pub fn parse_message_kind(payload: &[u8]) -> Option<MessageKind> {
    let envelope: ControlEnvelope = serde_json::from_slice(payload).ok()?;
    match envelope.msg_type.as_str() {
        "HELLO" => Some(MessageKind::Hello),
        "AUTH" => Some(MessageKind::Auth),
        "ATTACH" => Some(MessageKind::Attach),
        "RESUME" => Some(MessageKind::Resume),
        "RESIZE" => Some(MessageKind::Resize),
        "DETACH" => Some(MessageKind::Detach),
        "CLOSE" => Some(MessageKind::Close),
        "PING" => Some(MessageKind::Ping),
        "PONG" => Some(MessageKind::Pong),
        "ERROR" => Some(MessageKind::Error),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_message_type() {
        let payload = br#"{"type":"HELLO"}"#;
        assert_eq!(parse_message_kind(payload), Some(MessageKind::Hello));
    }

    #[test]
    fn rejects_unknown_message_type() {
        let payload = br#"{"type":"FOO"}"#;
        assert_eq!(parse_message_kind(payload), None);
    }
}
