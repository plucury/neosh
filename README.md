# neosh

neosh is a QUIC-based remote terminal protocol.

- Spec: [`neosh_protocol_v0.1.0.md`](./neosh_protocol_v0.1.0.md)
- ALPN: `neosh/1`

## Server-Client Sequence

```mermaid
sequenceDiagram
    autonumber
    participant C as Client
    participant SSH as SSH Daemon
    participant S as Server

    Note over C,SSH: Bootstrap over SSH
    C->>SSH: SSH login
    C->>SSH: run `neoshd new --user $USER`
    SSH-->>C: session_id + auth_token + quic_addr + cert_fingerprint

    Note over C,S: QUIC + TLS 1.3 (ALPN neosh/1)

    C->>S: HELLO(protocol_version, capabilities)
    alt protocol unsupported
        S-->>C: ERROR(PROTOCOL_ERROR)
        S--xC: close connection
    else protocol accepted
        S-->>C: HELLO_ACK(protocol_version, capabilities, session_timeout_seconds)
    end

    C->>S: AUTH(method=ssh-token, token=auth_token)
    alt auth failed
        S-->>C: ERROR(AUTH_FAILED)
        S--xC: close connection
    else auth success
        S-->>C: AUTH_OK(session_id, resume_token, resume_token_expires_in_seconds)
    end

    alt fresh attach
        C->>S: ATTACH(session_id, attach_mode=exclusive)
        S-->>C: ATTACH_OK(session_id)
    else resume attach
        C->>S: RESUME(session_id, resume_token)
        S-->>C: RESUME_OK(session_id, replay_bytes)
        Note over S,C: replay buffered output first, then switch to live output
    end

    par data plane
        C->>S: STDIN stream (unidirectional, bytes)
    and
        S->>C: STDOUT stream (unidirectional, bytes)
    end

    opt keepalive
        C->>S: PING(nonce)
        S-->>C: PONG(nonce)
    end

    opt detach
        C->>S: DETACH
        Note over S: session -> DETACHED (can resume before timeout)
    end

    opt terminate
        C->>S: CLOSE
        Note over S: session -> TERMINATED
    end
```

## Token Semantics (v0.1.0)

- `auth_token`: opaque, short-lived, single-use, only for `AUTH`.
- `resume_token`: opaque, revocable, used only for `RESUME`.
- If `auth_token` expires before `AUTH`, client must run SSH bootstrap again.

## TLS Trust Bootstrap (v0.1.0)

- `neoshd` auto-generates (or loads) TLS cert/key at bootstrap time.
- SSH bootstrap response MUST include QUIC address and certificate fingerprint.
- Returned `quic_addr` MUST be client-routable for that session.
- `neosh` MUST pin and verify fingerprint on first QUIC handshake in that session.
- `neoshd` default bind policy follows SSH-bootstrap bind policy (`bind-server=ssh`,
  port range `30000-39999`).
