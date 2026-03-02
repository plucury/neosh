use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    Init,
    Connected,
    HelloDone,
    Authenticated,
    Attached,
    Detached,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientEvent {
    QuicConnected,
    HelloAck,
    AuthOk,
    AttachOk,
    ResumeOk,
    Detached,
    Closed,
    Error,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateError {
    #[error("invalid state transition")]
    InvalidTransition,
}

pub fn transition(state: ClientState, event: ClientEvent) -> Result<ClientState, StateError> {
    let next = match (state, event) {
        (ClientState::Init, ClientEvent::QuicConnected) => ClientState::Connected,
        (ClientState::Connected, ClientEvent::HelloAck) => ClientState::HelloDone,
        (ClientState::HelloDone, ClientEvent::AuthOk) => ClientState::Authenticated,
        (ClientState::Authenticated, ClientEvent::AttachOk | ClientEvent::ResumeOk) => {
            ClientState::Attached
        }
        (ClientState::Attached, ClientEvent::Detached) => ClientState::Detached,
        (ClientState::Attached, ClientEvent::Closed)
        | (ClientState::Detached, ClientEvent::Closed) => ClientState::Closed,
        (_, ClientEvent::Error) => ClientState::Closed,
        _ => return Err(StateError::InvalidTransition),
    };

    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_new_session_path() {
        let mut s = ClientState::Init;
        s = transition(s, ClientEvent::QuicConnected).unwrap();
        s = transition(s, ClientEvent::HelloAck).unwrap();
        s = transition(s, ClientEvent::AuthOk).unwrap();
        s = transition(s, ClientEvent::AttachOk).unwrap();
        assert_eq!(s, ClientState::Attached);
    }

    #[test]
    fn invalid_transition_rejected() {
        assert_eq!(
            transition(ClientState::Init, ClientEvent::AuthOk),
            Err(StateError::InvalidTransition)
        );
    }
}
