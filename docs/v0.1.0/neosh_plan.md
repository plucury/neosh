# neosh v0.1.0 CLI Plan

## Scope

Implement `neosh` (user-facing CLI) that bootstraps via SSH and then uses
QUIC protocol v0.1.0:

- Obtain `session_id` and short-lived `auth_token` from SSH bootstrap step.
- Connect via QUIC (`neosh/1`) and run control flow.
- Stream local terminal input/output over protocol data streams.
- Support detach/reconnect resume.

Binary role:

- `neosh`: interactive CLI entrypoint for connect/attach/resume/detach.
- `neosh` talks to remote `neoshd` over QUIC.

## Milestones

### M1: Bootstrap and Connection Setup

- Implement SSH bootstrap command execution to start `neoshd new`.
- Parse bootstrap response (`session_id`, `auth_token`, `quic_addr`, `cert_fingerprint`).
- Establish QUIC connection with TLS 1.3 and ALPN `neosh/1`.
- Pin and verify server certificate fingerprint from bootstrap before `AUTH`.
- Open control stream and implement frame codec (length-prefixed JSON).

Deliverables:

- Bootstrap module.
- QUIC connection manager.
- Control stream read/write primitives.

### M2: Handshake and Auth Flow

- Send `HELLO(protocol_version, capabilities)`.
- Validate `HELLO_ACK` negotiated capabilities.
- Send `AUTH(method=ssh-token, token=auth_token)`.
- Store `session_id` and `resume_token` from `AUTH_OK`.
- Implement typed `ERROR` handling and user-facing diagnostics.
- Treat `auth_token` as single-use bootstrap credential.

Deliverables:

- Handshake state machine.
- Capability negotiation checks.
- Error-to-CLI message mapping.

### M3: Attach/Resume and Terminal Wiring

- Fresh path: `ATTACH` -> `ATTACH_OK`.
- Resume path (post-`AUTH`): `RESUME` -> `RESUME_OK`.
- Open `STDIN` and `STDOUT` streams only after attach/resume success.
- Forward local keystrokes to `STDIN`.
- Render `STDOUT` bytes to local terminal.
- Send `RESIZE` on local terminal dimension changes.

Deliverables:

- Attach/resume controller.
- Terminal IO bridge.
- Signal handler for terminal resize.

### M4: Detach, Reconnect, Keepalive

- Implement user-triggered `DETACH`.
- Preserve reconnect metadata (`session_id`, `resume_token`, expiry).
- Reconnect flow must obtain fresh single-use `auth_token` via SSH
  `neoshd renew-auth --session-id <uuid> --user "$USER"`, then run
  `HELLO` -> `AUTH` -> `RESUME`.
- Reconnect with exponential backoff around the above resume flow.
- Implement `PING`/`PONG` keepalive behavior.
- Handle `CLOSE` and termination cleanup.
- If initial `AUTH` fails due to expired token, trigger SSH bootstrap to fetch a new token.

Deliverables:

- Resume cache and reconnect strategy.
- Keepalive scheduler.
- Graceful shutdown path.

## Local State Model

- `Disconnected`: no active QUIC connection.
- `Connecting`: QUIC handshake and control stream setup.
- `Authenticated`: `AUTH_OK` received.
- `Attached`: data streams active.
- `Detached`: session still resumable.
- `Closed`: terminal session ended.

Transition guards:

- No data stream operations before `ATTACH_OK` or `RESUME_OK`.
- Resume only if local `resume_token` is present/not expired and a fresh
  `auth_token` is obtained via SSH `renew-auth`.
- Never reuse `auth_token` after any `AUTH` attempt.

## Test Plan

### Unit Tests

- Frame encode/decode correctness.
- Handshake state machine transitions.
- Error mapping and retry decisions.
- Resume metadata expiry logic.
- Auth-token-expired flow retries bootstrap instead of blind QUIC retry.

### Integration Tests

- End-to-end attach and interactive command execution.
- Detach then resume within timeout.
- Resume failure on expired token.
- Resume requires SSH `renew-auth` and performs `AUTH` before `RESUME`.
- Initial auth failure on expired/consumed bootstrap token.
- Resize propagation to server PTY.

### Failure/Recovery Tests

- Network interruption and reconnect.
- Server sends `ERROR(PROTOCOL_ERROR)` on bad request.
- Server close during active stream.

## Acceptance Criteria

- Client can bootstrap, authenticate, attach, and run an interactive shell.
- Client can detach and resume successfully before timeout using
  SSH `renew-auth` + `AUTH` + `RESUME`.
- Protocol errors are surfaced clearly without undefined states.
- Keepalive and reconnect do not violate protocol ordering.
