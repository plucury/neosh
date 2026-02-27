use std::time::Duration;

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::session::manager::{SessionError, SessionManager};
use crate::token::service::{TokenError, TokenService};

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub auth_token_ttl: Duration,
    pub bind_addr: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            auth_token_ttl: Duration::from_secs(60),
            bind_addr: "127.0.0.1:30000".to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BootstrapOutput {
    pub session_id: Uuid,
    pub auth_token: String,
    pub auth_token_expires_in_seconds: u64,
    pub quic_addr: String,
    pub cert_fingerprint: String,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Token(#[from] TokenError),
}

#[derive(Debug, Default)]
pub struct NeoshdRuntime {
    pub sessions: SessionManager,
    pub tokens: TokenService,
    pub config: RuntimeConfig,
}

impl NeoshdRuntime {
    pub fn with_config(config: RuntimeConfig) -> Self {
        Self {
            sessions: SessionManager::default(),
            tokens: TokenService::default(),
            config,
        }
    }

    pub fn new_session(&mut self, user_id: &str) -> BootstrapOutput {
        let session_id = self.sessions.create_session(
            user_id.to_string(),
            self.config.bind_addr.clone(),
            "sha256:local-dev-fingerprint".to_string(),
        );
        self.issue_bootstrap_for_session(session_id, user_id)
            .expect("new session should always exist")
    }

    pub fn renew_auth(&mut self, session_id: Uuid, user_id: &str) -> Result<BootstrapOutput, RuntimeError> {
        self.sessions.assert_owner(session_id, user_id)?;
        self.issue_bootstrap_for_session(session_id, user_id)
    }

    fn issue_bootstrap_for_session(
        &mut self,
        session_id: Uuid,
        user_id: &str,
    ) -> Result<BootstrapOutput, RuntimeError> {
        let auth_token = self
            .tokens
            .issue_auth_token(session_id, user_id, self.config.auth_token_ttl);

        let session = self.sessions.session(session_id).ok_or(SessionError::NotFound)?;
        Ok(BootstrapOutput {
            session_id,
            auth_token,
            auth_token_expires_in_seconds: self.config.auth_token_ttl.as_secs(),
            quic_addr: session.quic_addr.clone(),
            cert_fingerprint: session.cert_fingerprint.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renew_auth_reuses_existing_session_id() {
        let mut runtime = NeoshdRuntime::with_config(RuntimeConfig::default());
        let created = runtime.new_session("alice");

        let renewed = runtime.renew_auth(created.session_id, "alice").unwrap();
        assert_eq!(renewed.session_id, created.session_id);
        assert_ne!(renewed.auth_token, created.auth_token);
    }

    #[test]
    fn renew_auth_requires_owner_match() {
        let mut runtime = NeoshdRuntime::with_config(RuntimeConfig::default());
        let created = runtime.new_session("alice");

        let err = runtime.renew_auth(created.session_id, "bob").unwrap_err();
        assert!(matches!(err, RuntimeError::Session(SessionError::PermissionDenied)));
    }
}
