use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Auth,
    Resume,
}

#[derive(Debug, Clone)]
pub struct TokenRecord {
    pub token_hash: [u8; 32],
    pub token_type: TokenType,
    pub session_id: Uuid,
    pub user_id: String,
    pub jti: Uuid,
    pub expires_at: SystemTime,
    pub consumed_at: Option<SystemTime>,
    pub revoked_at: Option<SystemTime>,
    pub created_at: SystemTime,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("token not found")]
    NotFound,
    #[error("token expired")]
    Expired,
    #[error("token revoked")]
    Revoked,
    #[error("token consumed")]
    Consumed,
    #[error("token/session/user mismatch")]
    BindingMismatch,
    #[error("token type mismatch")]
    TypeMismatch,
}

#[derive(Debug, Default)]
pub struct TokenService {
    records: Vec<TokenRecord>,
}

impl TokenService {
    pub fn issue_auth_token(&mut self, session_id: Uuid, user_id: &str, ttl: Duration) -> String {
        self.issue_token(TokenType::Auth, session_id, user_id, ttl)
    }

    pub fn issue_resume_token(&mut self, session_id: Uuid, user_id: &str, ttl: Duration) -> String {
        self.issue_token(TokenType::Resume, session_id, user_id, ttl)
    }

    fn issue_token(
        &mut self,
        token_type: TokenType,
        session_id: Uuid,
        user_id: &str,
        ttl: Duration,
    ) -> String {
        let raw = random_token();
        let hash = hash_token(raw.as_bytes());
        let now = SystemTime::now();
        self.records.push(TokenRecord {
            token_hash: hash,
            token_type,
            session_id,
            user_id: user_id.to_string(),
            jti: Uuid::new_v4(),
            expires_at: now + ttl,
            consumed_at: None,
            revoked_at: None,
            created_at: now,
        });
        raw
    }

    pub fn validate_and_consume_auth(
        &mut self,
        raw_token: &str,
        session_id: Uuid,
        user_id: &str,
        now: SystemTime,
    ) -> Result<(), TokenError> {
        let record = self
            .find_record_mut(raw_token)
            .ok_or(TokenError::NotFound)?;

        if record.token_type != TokenType::Auth {
            return Err(TokenError::TypeMismatch);
        }
        if record.session_id != session_id || record.user_id != user_id {
            return Err(TokenError::BindingMismatch);
        }
        if record.expires_at <= now {
            return Err(TokenError::Expired);
        }
        if record.revoked_at.is_some() {
            return Err(TokenError::Revoked);
        }
        if record.consumed_at.is_some() {
            return Err(TokenError::Consumed);
        }

        record.consumed_at = Some(now);
        Ok(())
    }

    pub fn validate_resume(
        &mut self,
        raw_token: &str,
        session_id: Uuid,
        user_id: &str,
        now: SystemTime,
    ) -> Result<(), TokenError> {
        let record = self
            .find_record_mut(raw_token)
            .ok_or(TokenError::NotFound)?;

        if record.token_type != TokenType::Resume {
            return Err(TokenError::TypeMismatch);
        }
        if record.session_id != session_id || record.user_id != user_id {
            return Err(TokenError::BindingMismatch);
        }
        if record.expires_at <= now {
            return Err(TokenError::Expired);
        }
        if record.revoked_at.is_some() {
            return Err(TokenError::Revoked);
        }
        Ok(())
    }

    pub fn revoke_token(&mut self, raw_token: &str, now: SystemTime) -> Result<(), TokenError> {
        let record = self
            .find_record_mut(raw_token)
            .ok_or(TokenError::NotFound)?;
        record.revoked_at = Some(now);
        Ok(())
    }

    pub fn cleanup_expired(&mut self, now: SystemTime, retention: Duration) {
        self.records.retain(|record| {
            if record.expires_at > now {
                return true;
            }

            let revoked_too_recent = record
                .revoked_at
                .and_then(|t| now.duration_since(t).ok())
                .map(|elapsed| elapsed <= retention)
                .unwrap_or(false);

            let consumed_too_recent = record
                .consumed_at
                .and_then(|t| now.duration_since(t).ok())
                .map(|elapsed| elapsed <= retention)
                .unwrap_or(false);

            revoked_too_recent || consumed_too_recent
        });
    }

    fn find_record_mut(&mut self, raw_token: &str) -> Option<&mut TokenRecord> {
        let hash = hash_token(raw_token.as_bytes());
        self.records
            .iter_mut()
            .find(|record| bool::from(record.token_hash.ct_eq(&hash)))
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hash_token(raw_token: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(raw_token);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_token_is_single_use() {
        let mut service = TokenService::default();
        let sid = Uuid::new_v4();
        let now = SystemTime::now();
        let token = service.issue_auth_token(sid, "alice", Duration::from_secs(60));

        assert!(
            service
                .validate_and_consume_auth(&token, sid, "alice", now)
                .is_ok()
        );
        assert_eq!(
            service.validate_and_consume_auth(&token, sid, "alice", now),
            Err(TokenError::Consumed)
        );
    }

    #[test]
    fn auth_token_expired_is_rejected() {
        let mut service = TokenService::default();
        let sid = Uuid::new_v4();
        let token = service.issue_auth_token(sid, "alice", Duration::from_secs(1));
        let late = SystemTime::now() + Duration::from_secs(2);

        assert_eq!(
            service.validate_and_consume_auth(&token, sid, "alice", late),
            Err(TokenError::Expired)
        );
    }

    #[test]
    fn resume_token_multi_use_until_revoked() {
        let mut service = TokenService::default();
        let sid = Uuid::new_v4();
        let now = SystemTime::now();
        let token = service.issue_resume_token(sid, "alice", Duration::from_secs(60));

        assert!(service.validate_resume(&token, sid, "alice", now).is_ok());
        assert!(service.validate_resume(&token, sid, "alice", now).is_ok());

        service.revoke_token(&token, now).unwrap();
        assert_eq!(
            service.validate_resume(&token, sid, "alice", now),
            Err(TokenError::Revoked)
        );
    }

    #[test]
    fn cleanup_keeps_recently_consumed_then_drops_after_retention() {
        let mut service = TokenService::default();
        let sid = Uuid::new_v4();
        let now = SystemTime::now();
        let token = service.issue_auth_token(sid, "alice", Duration::from_secs(1));

        service
            .validate_and_consume_auth(&token, sid, "alice", now)
            .unwrap();

        let after_expire = now + Duration::from_secs(2);
        service.cleanup_expired(after_expire, Duration::from_secs(10));
        assert_eq!(
            service.validate_and_consume_auth(&token, sid, "alice", after_expire),
            Err(TokenError::Expired)
        );

        let after_retention = now + Duration::from_secs(20);
        service.cleanup_expired(after_retention, Duration::from_secs(5));
        assert_eq!(
            service.validate_and_consume_auth(&token, sid, "alice", after_retention),
            Err(TokenError::NotFound)
        );
    }
}
