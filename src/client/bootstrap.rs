use std::process::Command;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapPayload {
    pub session_id: Uuid,
    pub auth_token: String,
    pub auth_token_expires_in_seconds: u64,
    pub quic_addr: String,
    pub cert_fingerprint: String,
}

#[derive(Debug, Error)]
pub enum BootstrapError {
    #[error("ssh command failed: {0}")]
    SshCommand(String),
    #[error("invalid bootstrap json: {0}")]
    InvalidJson(String),
    #[error("invalid bootstrap payload: {0}")]
    InvalidPayload(String),
}

pub fn run_remote_command(target: &str, remote_command: &str) -> Result<String, BootstrapError> {
    let output = Command::new("ssh")
        .arg(target)
        .arg(remote_command)
        .output()
        .map_err(|e| BootstrapError::SshCommand(e.to_string()))?;

    if !output.status.success() {
        return Err(BootstrapError::SshCommand(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn resolve_remote_working_directory(target: &str) -> Result<String, BootstrapError> {
    let pwd = run_remote_command(target, "sh -lc 'pwd'")?;
    if pwd.is_empty() {
        return Err(BootstrapError::SshCommand(
            "remote working directory is empty".to_string(),
        ));
    }
    Ok(pwd)
}

#[allow(non_snake_case)]
pub fn runRemoteCommand(target: &str, remote_command: &str) -> Result<String, BootstrapError> {
    run_remote_command(target, remote_command)
}

#[allow(non_snake_case)]
pub fn resolveRemoteWorkingDirectory(target: &str) -> Result<String, BootstrapError> {
    resolve_remote_working_directory(target)
}

pub fn parse_bootstrap_payload(raw: &str) -> Result<BootstrapPayload, BootstrapError> {
    let payload: BootstrapPayload =
        serde_json::from_str(raw).map_err(|e| BootstrapError::InvalidJson(e.to_string()))?;
    validate_bootstrap_payload(&payload)?;
    Ok(payload)
}

pub fn validate_bootstrap_payload(payload: &BootstrapPayload) -> Result<(), BootstrapError> {
    if payload.auth_token.is_empty() {
        return Err(BootstrapError::InvalidPayload(
            "auth_token is empty".to_string(),
        ));
    }
    if payload.auth_token_expires_in_seconds == 0 {
        return Err(BootstrapError::InvalidPayload(
            "auth_token_expires_in_seconds must be > 0".to_string(),
        ));
    }
    if !is_valid_host_port(&payload.quic_addr) {
        return Err(BootstrapError::InvalidPayload(
            "quic_addr is not host:port".to_string(),
        ));
    }
    if !payload.cert_fingerprint.starts_with("sha256:") {
        return Err(BootstrapError::InvalidPayload(
            "cert_fingerprint must start with sha256:".to_string(),
        ));
    }
    let hex = &payload.cert_fingerprint[7..];
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(BootstrapError::InvalidPayload(
            "cert_fingerprint hex is invalid".to_string(),
        ));
    }
    Ok(())
}

fn is_valid_host_port(input: &str) -> bool {
    if input.is_empty() {
        return false;
    }

    if let Some(rest) = input.strip_prefix('[') {
        let Some((host, port)) = rest.split_once("]:") else {
            return false;
        };
        if host.is_empty() {
            return false;
        }
        return port.parse::<u16>().is_ok();
    }

    let Some((host, port)) = input.rsplit_once(':') else {
        return false;
    };
    if host.is_empty() {
        return false;
    }
    port.parse::<u16>().is_ok()
}

pub fn run_ssh_bootstrap(
    target: &str,
    remote_command: &str,
) -> Result<BootstrapPayload, BootstrapError> {
    let stdout = run_remote_command(target, remote_command)?;
    parse_bootstrap_payload(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_bootstrap_payload() {
        let raw = r#"{
          "session_id":"550e8400-e29b-41d4-a716-446655440000",
          "auth_token":"opaque",
          "auth_token_expires_in_seconds":60,
          "quic_addr":"127.0.0.1:30001",
          "cert_fingerprint":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        }"#;

        let p = parse_bootstrap_payload(raw).unwrap();
        assert_eq!(p.auth_token, "opaque");
    }

    #[test]
    fn reject_invalid_fingerprint() {
        let payload = BootstrapPayload {
            session_id: Uuid::new_v4(),
            auth_token: "t".into(),
            auth_token_expires_in_seconds: 60,
            quic_addr: "127.0.0.1:30001".into(),
            cert_fingerprint: "sha256:xyz".into(),
        };
        assert!(validate_bootstrap_payload(&payload).is_err());
    }

    #[test]
    fn accept_hostname_quic_addr() {
        let payload = BootstrapPayload {
            session_id: Uuid::new_v4(),
            auth_token: "t".into(),
            auth_token_expires_in_seconds: 60,
            quic_addr: "example.com:30001".into(),
            cert_fingerprint:
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
        };
        assert!(validate_bootstrap_payload(&payload).is_ok());
    }
}
