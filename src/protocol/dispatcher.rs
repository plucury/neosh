use crate::protocol::messages::MessageKind;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    AwaitHello,
    AwaitAuth,
    AwaitAttachOrResume,
    Attached,
    Closed,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DispatchError {
    #[error("protocol error: out-of-order message")]
    ProtocolOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    Continue,
    Close,
}

#[derive(Debug)]
pub struct Dispatcher {
    state: ConnectionState,
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self {
            state: ConnectionState::AwaitHello,
        }
    }
}

impl Dispatcher {
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    pub fn on_message(&mut self, kind: MessageKind) -> Result<DispatchOutcome, DispatchError> {
        use ConnectionState::*;
        use MessageKind::*;

        match (self.state, kind) {
            (AwaitHello, Hello) => {
                self.state = AwaitAuth;
                Ok(DispatchOutcome::Continue)
            }
            (AwaitHello, Ping) => Ok(DispatchOutcome::Continue),
            (AwaitAuth, Auth) => {
                self.state = AwaitAttachOrResume;
                Ok(DispatchOutcome::Continue)
            }
            (AwaitAuth, Ping) => Ok(DispatchOutcome::Continue),
            (AwaitAttachOrResume, Attach | Resume) => {
                self.state = Attached;
                Ok(DispatchOutcome::Continue)
            }
            (AwaitAttachOrResume, Close) => {
                self.state = Closed;
                Ok(DispatchOutcome::Close)
            }
            (AwaitAttachOrResume, Ping) => Ok(DispatchOutcome::Continue),
            (Attached, Resize | Detach | Ping) => Ok(DispatchOutcome::Continue),
            (Attached, Close) => {
                self.state = Closed;
                Ok(DispatchOutcome::Close)
            }
            (Closed, _) => Err(DispatchError::ProtocolOrder),
            _ => Err(DispatchError::ProtocolOrder),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_auth_before_hello() {
        let mut d = Dispatcher::default();
        assert_eq!(
            d.on_message(MessageKind::Auth),
            Err(DispatchError::ProtocolOrder)
        );
    }

    #[test]
    fn allows_valid_attach_flow() {
        let mut d = Dispatcher::default();

        assert_eq!(
            d.on_message(MessageKind::Hello),
            Ok(DispatchOutcome::Continue)
        );
        assert_eq!(
            d.on_message(MessageKind::Auth),
            Ok(DispatchOutcome::Continue)
        );
        assert_eq!(
            d.on_message(MessageKind::Attach),
            Ok(DispatchOutcome::Continue)
        );
        assert_eq!(d.state(), ConnectionState::Attached);
    }

    #[test]
    fn allows_valid_resume_flow() {
        let mut d = Dispatcher::default();

        assert_eq!(
            d.on_message(MessageKind::Hello),
            Ok(DispatchOutcome::Continue)
        );
        assert_eq!(
            d.on_message(MessageKind::Auth),
            Ok(DispatchOutcome::Continue)
        );
        assert_eq!(
            d.on_message(MessageKind::Resume),
            Ok(DispatchOutcome::Continue)
        );
        assert_eq!(d.state(), ConnectionState::Attached);
    }

    #[test]
    fn resize_only_valid_when_attached() {
        let mut d = Dispatcher::default();
        assert_eq!(
            d.on_message(MessageKind::Resize),
            Err(DispatchError::ProtocolOrder)
        );

        d.on_message(MessageKind::Hello).unwrap();
        d.on_message(MessageKind::Auth).unwrap();
        d.on_message(MessageKind::Attach).unwrap();

        assert_eq!(
            d.on_message(MessageKind::Resize),
            Ok(DispatchOutcome::Continue)
        );
    }
}
