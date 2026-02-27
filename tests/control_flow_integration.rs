use std::time::{Duration, SystemTime};

use neoshd::protocol::dispatcher::Dispatcher;
use neoshd::protocol::messages::MessageKind;
use neoshd::session::manager::{SessionManager, SessionState};
use neoshd::token::service::TokenService;
use uuid::Uuid;

#[test]
fn integration_auth_attach_flow_updates_session_state() {
    let mut sessions = SessionManager::default();
    let mut tokens = TokenService::default();
    let mut dispatcher = Dispatcher::default();

    let session_id = sessions.create_session(
        "alice".to_string(),
        "203.0.113.10:30001".to_string(),
        "sha256:abc".to_string(),
    );

    let auth_token = tokens.issue_auth_token(session_id, "alice", Duration::from_secs(60));

    dispatcher.on_message(MessageKind::Hello).unwrap();
    dispatcher.on_message(MessageKind::Auth).unwrap();

    tokens
        .validate_and_consume_auth(&auth_token, session_id, "alice", SystemTime::now())
        .unwrap();

    let conn_id = Uuid::new_v4();
    sessions.attach_exclusive(session_id, conn_id).unwrap();
    dispatcher.on_message(MessageKind::Attach).unwrap();

    let session = sessions.session(session_id).unwrap();
    assert_eq!(session.state, SessionState::Attached);
    assert_eq!(session.attached_conn_id, Some(conn_id));
}

#[test]
fn integration_resume_flow_uses_resume_token_and_cleanup() {
    let mut sessions = SessionManager::default();
    let mut tokens = TokenService::default();
    let mut dispatcher = Dispatcher::default();

    let session_id = sessions.create_session(
        "alice".to_string(),
        "203.0.113.10:30001".to_string(),
        "sha256:abc".to_string(),
    );

    let auth_token = tokens.issue_auth_token(session_id, "alice", Duration::from_secs(60));
    let resume_token = tokens.issue_resume_token(session_id, "alice", Duration::from_secs(60));

    dispatcher.on_message(MessageKind::Hello).unwrap();
    dispatcher.on_message(MessageKind::Auth).unwrap();
    tokens
        .validate_and_consume_auth(&auth_token, session_id, "alice", SystemTime::now())
        .unwrap();

    tokens
        .validate_resume(&resume_token, session_id, "alice", SystemTime::now())
        .unwrap();

    let conn_id = Uuid::new_v4();
    let epoch = sessions.attach_exclusive(session_id, conn_id).unwrap();
    dispatcher.on_message(MessageKind::Resume).unwrap();

    assert!(sessions.conditional_stale_cleanup(session_id, conn_id, epoch));
    let session = sessions.session(session_id).unwrap();
    assert_eq!(session.state, SessionState::Detached);
    assert_eq!(session.attached_conn_id, None);
}
