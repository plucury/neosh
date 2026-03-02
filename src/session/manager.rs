use std::collections::HashMap;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Created,
    Attached,
    Detached,
    Expired,
    Terminated,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: Uuid,
    pub owner_user_id: String,
    pub quic_addr: String,
    pub cert_fingerprint: String,
    pub state: SessionState,
    pub attached_conn_id: Option<Uuid>,
    pub attach_epoch: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("session not found")]
    NotFound,
    #[error("attach denied")]
    AttachDenied,
    #[error("session not attachable")]
    InvalidState,
    #[error("permission denied")]
    PermissionDenied,
}

#[derive(Debug, Default)]
pub struct SessionManager {
    sessions: HashMap<Uuid, Session>,
}

impl SessionManager {
    pub fn create_session(
        &mut self,
        owner_user_id: String,
        quic_addr: String,
        cert_fingerprint: String,
    ) -> Uuid {
        let session_id = Uuid::new_v4();
        self.create_session_with_id(session_id, owner_user_id, quic_addr, cert_fingerprint);
        session_id
    }

    pub fn create_session_with_id(
        &mut self,
        session_id: Uuid,
        owner_user_id: String,
        quic_addr: String,
        cert_fingerprint: String,
    ) {
        self.sessions.insert(
            session_id,
            Session {
                session_id,
                owner_user_id,
                quic_addr,
                cert_fingerprint,
                state: SessionState::Created,
                attached_conn_id: None,
                attach_epoch: 0,
            },
        );
    }

    pub fn session(&self, session_id: Uuid) -> Option<&Session> {
        self.sessions.get(&session_id)
    }

    pub fn assert_owner(&self, session_id: Uuid, user_id: &str) -> Result<(), SessionError> {
        let session = self
            .sessions
            .get(&session_id)
            .ok_or(SessionError::NotFound)?;
        if session.owner_user_id == user_id {
            Ok(())
        } else {
            Err(SessionError::PermissionDenied)
        }
    }

    pub fn attach_exclusive(
        &mut self,
        session_id: Uuid,
        conn_id: Uuid,
    ) -> Result<u64, SessionError> {
        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or(SessionError::NotFound)?;

        match session.state {
            SessionState::Expired | SessionState::Terminated => {
                return Err(SessionError::InvalidState);
            }
            _ => {}
        }

        if session.attached_conn_id.is_some() {
            return Err(SessionError::AttachDenied);
        }

        session.attach_epoch = session.attach_epoch.saturating_add(1);
        session.attached_conn_id = Some(conn_id);
        session.state = SessionState::Attached;

        Ok(session.attach_epoch)
    }

    pub fn detach(&mut self, session_id: Uuid, conn_id: Uuid) -> Result<(), SessionError> {
        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or(SessionError::NotFound)?;
        if session.attached_conn_id != Some(conn_id) {
            return Err(SessionError::AttachDenied);
        }

        session.attached_conn_id = None;
        if session.state != SessionState::Terminated && session.state != SessionState::Expired {
            session.state = SessionState::Detached;
        }
        Ok(())
    }

    pub fn conditional_stale_cleanup(
        &mut self,
        session_id: Uuid,
        conn_id: Uuid,
        epoch: u64,
    ) -> bool {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return false;
        };

        if session.attached_conn_id == Some(conn_id) && session.attach_epoch == epoch {
            session.attached_conn_id = None;
            if session.state != SessionState::Terminated && session.state != SessionState::Expired {
                session.state = SessionState::Detached;
            }
            return true;
        }

        false
    }

    pub fn terminate(&mut self, session_id: Uuid) -> Result<(), SessionError> {
        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or(SessionError::NotFound)?;
        session.state = SessionState::Terminated;
        session.attached_conn_id = None;
        Ok(())
    }

    pub fn expire(&mut self, session_id: Uuid) -> Result<(), SessionError> {
        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or(SessionError::NotFound)?;
        session.state = SessionState::Expired;
        session.attached_conn_id = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_manager() -> (SessionManager, Uuid) {
        let mut manager = SessionManager::default();
        let session_id = manager.create_session(
            "alice".to_string(),
            "203.0.113.10:30001".to_string(),
            "sha256:abc".to_string(),
        );
        (manager, session_id)
    }

    #[test]
    fn attach_exclusive_only_one_owner() {
        let (mut manager, session_id) = build_manager();
        let c1 = Uuid::new_v4();
        let c2 = Uuid::new_v4();

        assert!(manager.attach_exclusive(session_id, c1).is_ok());
        assert_eq!(
            manager.attach_exclusive(session_id, c2),
            Err(SessionError::AttachDenied)
        );
    }

    #[test]
    fn stale_cleanup_does_not_detach_new_owner() {
        let (mut manager, session_id) = build_manager();
        let c1 = Uuid::new_v4();
        let e1 = manager.attach_exclusive(session_id, c1).unwrap();
        assert!(manager.detach(session_id, c1).is_ok());

        let c2 = Uuid::new_v4();
        let _e2 = manager.attach_exclusive(session_id, c2).unwrap();

        let cleaned = manager.conditional_stale_cleanup(session_id, c1, e1);
        assert!(!cleaned);

        let session = manager.session(session_id).unwrap();
        assert_eq!(session.attached_conn_id, Some(c2));
        assert_eq!(session.state, SessionState::Attached);
    }

    #[test]
    fn stale_cleanup_detaches_current_owner_when_epoch_matches() {
        let (mut manager, session_id) = build_manager();
        let conn = Uuid::new_v4();
        let epoch = manager.attach_exclusive(session_id, conn).unwrap();

        let cleaned = manager.conditional_stale_cleanup(session_id, conn, epoch);
        assert!(cleaned);

        let session = manager.session(session_id).unwrap();
        assert_eq!(session.attached_conn_id, None);
        assert_eq!(session.state, SessionState::Detached);
    }

    #[test]
    fn renew_auth_owner_check() {
        let (manager, session_id) = build_manager();
        assert!(manager.assert_owner(session_id, "alice").is_ok());
        assert_eq!(
            manager.assert_owner(session_id, "bob"),
            Err(SessionError::PermissionDenied)
        );
    }
}
