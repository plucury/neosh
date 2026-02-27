# neoshd v0.1.0 Plan

## Scope

Implement `neoshd` (remote daemon) compatible with
`neosh_protocol_v0.1.0.md`:

- One QUIC connection carries exactly one logical session.
- Control plane over one bidirectional control stream.
- Data plane over `STDIN` (client -> server) and `STDOUT` (server -> client).
- SSH-token bootstrap auth and server-issued resume token.

Binary role:

- `neoshd`: long-running server process for session lifecycle and PTY runtime.
- Accepts QUIC connections from `neosh` CLI.

## Milestones

### M1: Transport and Handshake Skeleton

- Stand up QUIC listener with ALPN `neosh/1` and TLS 1.3.
- Enforce one control stream per connection.
- Parse length-prefixed JSON control frames.
- Implement `HELLO` -> `HELLO_ACK`.
- Return `ERROR(PROTOCOL_ERROR)` for malformed/unsupported messages.

Deliverables:

- Listener startup config.
- Control stream frame codec.
- Protocol-version gate and basic error path.

### M2: Auth and Session Lifecycle

- Implement `AUTH(method=ssh-token)`.
- Integrate SSH bootstrap token validator (pluggable interface).
- On success emit `AUTH_OK(session_id, resume_token, resume_token_expires_in_seconds)`.
- Maintain session state machine: `CREATED`, `ATTACHED`, `DETACHED`, `EXPIRED`, `TERMINATED`.
- Enforce timeout policy from `session_timeout_seconds`.
- Implement opaque token store for both `auth_token` and `resume_token`.
- Enforce single-use `auth_token` consumption and anti-replay by `jti`.

Deliverables:

- Auth service abstraction (`validateAuthToken`, `issueResumeToken`).
- In-memory session registry with TTL and transitions.
- Structured `ERROR` responses (`AUTH_FAILED`, `SESSION_NOT_FOUND`, etc).
- Token repository interface with revocation and expiry index.

### M3: Attach/Resume and Data Streams

- Implement `ATTACH` -> `ATTACH_OK`.
- Implement `RESUME` -> `RESUME_OK`.
- Gate data streams: usable only after `ATTACH_OK` or `RESUME_OK`.
- Bind PTY process I/O to data streams.
- Handle `RESIZE`, `DETACH`, `CLOSE`.

Deliverables:

- PTY manager and stream wiring.
- Attach/Resume validator.
- Deterministic close behavior for control/data streams.

### M4: Replay, Keepalive, and Robustness

- Maintain per-session output ring buffer.
- On resume with negotiated `resume-v1`, replay buffered output before live output.
- Fill `RESUME_OK.replay_bytes`.
- Implement `PING`/`PONG`.
- Add rate limiting and payload size guards.

Deliverables:

- Replay subsystem with bounded memory.
- Keepalive handler.
- Defensive limits (max frame length, invalid stream ordering).

## Core Components

- `QuicServer`: connection accept, ALPN/TLS setup.
- `ControlStreamHandler`: framing, dispatch, response encode.
- `SessionManager`: state transitions, timeout, ownership.
- `AuthProvider`: SSH token verify and resume token issue/verify.
- `TokenStore`: opaque token record CRUD (token, session_id, user_id, exp, jti, status).
- `TerminalRuntime`: PTY spawn, resize, stdin/stdout transport.
- `ReplayBuffer`: ring buffer append/replay.

## Opaque Token Implementation (neoshd)

### Token Record Schema

- `token_hash`: SHA-256 of raw token string (never persist raw token).
- `token_type`: `auth_token` or `resume_token`.
- `session_id`: target session binding.
- `user_id`: bound authenticated user.
- `jti`: unique random id for replay detection.
- `expires_at`: absolute expiration timestamp (UTC).
- `consumed_at`: set when token is consumed (for single-use auth token).
- `revoked_at`: set when token is revoked.
- `created_at`: creation timestamp.

### Generation

- Raw token bytes: 32 bytes from CSPRNG.
- Encode for transport: URL-safe base64 without padding.
- Persist only `token_hash`; raw token only returned once to caller.
- `auth_token` TTL: 30-60 seconds.
- `resume_token` TTL: configurable, default 24 hours.

### Validation Rules

- Common checks: exists, not expired, not revoked, session/user binding match.
- `AUTH` (`auth_token`):
  - MUST be single-use.
  - Use atomic compare-and-set from `consumed_at = NULL` to now.
  - CAS failure means replay/duplicate use -> `ERROR(AUTH_FAILED)`.
- `RESUME` (`resume_token`):
  - Multi-use by default until expiry/revocation.
  - If token rotation enabled, issue new token on `RESUME_OK` and revoke old token.

### Storage and Cleanup

- Default backend: in-memory map + periodic sweeper.
- Optional backend: Redis/SQLite via `TokenStore` interface.
- Sweeper interval: 60s.
- Cleanup criteria: `expires_at < now` OR `revoked_at IS NOT NULL` older than retention window.
- Retain consumed `auth_token` rows for short audit window (for replay forensics).

### Security Controls

- Constant-time hash comparison on lookup path.
- Rate-limit failed `AUTH` and `RESUME` per source IP/session_id.
- Structured audit log fields:
  - `event`: `token_issued`/`token_consumed`/`token_rejected`/`token_revoked`
  - `token_type`, `session_id`, `user_id`, `reason`, `remote_addr`

## Test Plan

### Protocol Tests

- `HELLO` success and unsupported version failure.
- Unknown message type -> `ERROR(PROTOCOL_ERROR)`.
- `AUTH` success/failure.
- `AUTH` fails on expired/consumed token.
- `AUTH` concurrent double-submit: only one succeeds (CAS semantics).
- `ATTACH` and `RESUME` success/failure combinations.
- Data stream before attach/resume must be rejected.

### Session Tests

- Transition correctness and invalid transition rejection.
- Timeout expiry behavior.
- Resume token expiration and revocation.
- Resume token `session_id` mismatch rejection.
- Resume token rotation invalidates previous token when enabled.

### Replay Tests

- Replay happens before live output.
- `replay_bytes` accuracy.
- Empty buffer resume behavior.

### Reliability Tests

- Client disconnect/reconnect resume flow.
- Concurrent sessions isolation.
- Large output backpressure handling.

## Acceptance Criteria

- All mandatory control messages are implemented.
- Server rejects out-of-order flows with explicit `ERROR`.
- Resume works within timeout and token validity window.
- Replay behavior matches v0.1.0 semantics.
- Interop test with reference client passes end-to-end shell command execution.
