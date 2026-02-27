# neoshd v0.1.0 Implementation Guide

## Goal

Build `neoshd` as a QUIC-based remote terminal daemon implementing
`neosh_protocol_v0.1.0.md` with:

- one QUIC connection per logical session
- control stream JSON framing
- PTY byte-stream transport
- opaque `auth_token` and `resume_token`

## Runtime Architecture

`neoshd` should run as one process with these runtime units:

- `Listener`: QUIC/TLS accept loop (ALPN `neosh/1`)
- `ConnectionActor`: per-connection control flow and stream ownership
- `SessionManager`: lifecycle and session registry
- `TokenService`: opaque token issue/validate/revoke
- `TerminalRuntime`: PTY process creation and IO bridging
- `ReplayBuffer`: bounded ring buffer per session

v0.1.0 fixed choices:

- QUIC/TLS stack: `quinn` (+ `rustls`)
- PTY stack: `portable-pty`
- token/session storage: in-memory only
- observability: structured logs only (no metrics backend required)

Recommended ownership:

- one connection is bound to one `session_id`
- `ConnectionActor` is the only writer for control stream responses
- PTY output fanout goes through `ReplayBuffer` before live `STDOUT`

## Suggested Package Layout

```text
neoshd/
  cmd/neoshd/main.*
  internal/config/
  internal/transport/quic/
  internal/protocol/
    framing.*
    messages.*
    dispatcher.*
  internal/session/
    manager.*
    store.*
    state_machine.*
  internal/token/
    service.*
    store.*
    generator.*
  internal/terminal/
    pty.*
    bridge.*
  internal/replay/
    ring_buffer.*
  internal/observability/
    log.*
```

## Protocol Implementation

### 0. Bootstrap Token Issuance Interface

`auth_token` issuance must be implemented explicitly for SSH bootstrap.

Recommended interface:

- `neoshd bootstrap` subcommand (invoked over SSH).
- Input context from SSH session/user identity.
- Output JSON (stdout) with:
  - `session_id`
  - `auth_token`
  - `auth_token_expires_in_seconds`

Behavior:

- create a new session owned by SSH-authenticated user
- issue single-use opaque `auth_token` bound to `session_id + user_id`
- persist token record before returning response

Session policy in v0.1.0:

- each bootstrap invocation MUST create a new `session_id`
- bootstrap MUST NOT reuse existing sessions

Failure:

- return non-zero exit and JSON error payload
- never print partial token data

### 1. Connection Bootstrap

1. Accept QUIC connection and enforce ALPN `neosh/1`.
2. Require exactly one bidirectional control stream.
3. Decode control frames (`uint32_be + json`).
4. Process order:
   - `HELLO` -> `HELLO_ACK`
   - `AUTH` -> `AUTH_OK`
   - `ATTACH` or `RESUME` -> `ATTACH_OK` or `RESUME_OK`
5. Reject out-of-order requests with `ERROR(PROTOCOL_ERROR)`.

### 2. Control Message Dispatcher

Use a strict stateful dispatcher:

- pre-auth: only `HELLO`, `AUTH`, `PING`
- post-auth pre-attach: `ATTACH`, `RESUME`, `PING`, `CLOSE`
- attached: `RESIZE`, `DETACH`, `PING`, `CLOSE`

Unknown type or invalid payload:

- respond with `ERROR(PROTOCOL_ERROR)`

### 3. Data Streams

- create/accept `STDIN` and `STDOUT` streams only after attach/resume success
- reject early data stream usage
- `STDIN` bytes -> PTY stdin
- PTY stdout -> `ReplayBuffer` -> `STDOUT` stream

## Session State Machine

States:

- `CREATED`
- `ATTACHED`
- `DETACHED`
- `EXPIRED`
- `TERMINATED`

Required transitions:

- `AUTH_OK` => `CREATED`
- `ATTACH_OK` or `RESUME_OK` => `ATTACHED`
- `DETACH` => `DETACHED`
- detached inactivity timeout => `EXPIRED`
- `CLOSE` => `TERMINATED`

Validation rules:

- reject illegal transitions and return `ERROR(PROTOCOL_ERROR)`
- `EXPIRED`/`TERMINATED` sessions cannot be resumed
- do not expire sessions in `ATTACHED` due to `session_timeout_seconds`

## ATTACH Exclusivity and Concurrency

`ATTACH` currently uses `attach_mode=exclusive`; server must enforce
single active attachment.

Session attachment fields:

- `attached_conn_id` (nullable)
- `attach_epoch` (monotonic integer)
- `attached_at`

Rules:

- `ATTACH(exclusive)` succeeds only when `attached_conn_id` is null.
- acquire attach atomically (CAS/transaction) on session row.
- if already attached by another connection, return `ERROR(ATTACH_DENIED)`.
- on connection close or `DETACH`, clear `attached_conn_id`.
- for stale connection cleanup, clear ownership only if `attach_epoch`
  still matches current owner (prevents late cleanup races).

Atomic attach algorithm (single statement/transaction):

1. Load current session row with lock.
2. If `attached_conn_id IS NOT NULL`, fail with `ATTACH_DENIED`.
3. Set:
   - `attached_conn_id = new_conn_id`
   - `attach_epoch = attach_epoch + 1`
   - `attached_at = now`
4. Commit.

Stale cleanup algorithm:

1. Capture `(conn_id, epoch)` when connection becomes attached.
2. On disconnect cleanup, execute conditional clear:
   - `WHERE session_id = ? AND attached_conn_id = conn_id AND attach_epoch = epoch`
3. Only if row matched:
   - clear `attached_conn_id`
   - transition `ATTACHED -> DETACHED` (unless already `TERMINATED/EXPIRED`)
4. If row not matched, skip cleanup (ownership has changed).

Required behavior under race:

- Old connection cleanup MUST NOT detach a newer owner.
- Simultaneous `ATTACH` attempts: exactly one succeeds, others get `ATTACH_DENIED`.

## Opaque Token Implementation

### Data Model

`TokenRecord`:

- `token_hash` (SHA-256 of token)
- `token_type` (`auth_token` or `resume_token`)
- `session_id`
- `user_id`
- `jti`
- `expires_at`
- `consumed_at` (nullable)
- `revoked_at` (nullable)
- `created_at`

Never persist raw token.

### Generation

- generate 32 random bytes from CSPRNG
- encode with URL-safe base64 (no padding)
- store only hash + metadata
- return raw token once to caller

### Validation

Common:

- record exists
- not expired
- not revoked
- `session_id` binding matches request

`auth_token`:

- single-use only
- atomic consume (`consumed_at` null -> now)
- second use must fail with `ERROR(AUTH_FAILED)`

Atomic consume contract:

- Consume must be a compare-and-set update:
  - `UPDATE ... SET consumed_at = now WHERE token_hash = ? AND consumed_at IS NULL AND revoked_at IS NULL AND expires_at > now`
- affected rows:
  - `1` => token accepted
  - `0` => token already consumed/expired/revoked/not found, return `AUTH_FAILED`
- Never perform separate "read then write" consume logic.

`resume_token`:

- valid until expire/revoke
- optionally rotate on successful `RESUME`

### Storage Backend

Minimum:

- in-memory map with mutex/RW lock
- sweeper every 60 seconds

External persistence backends are out of scope in v0.1.0.

Cleanup:

- delete expired records
- keep consumed/revoked records briefly for audit

## Terminal Runtime

### PTY Startup

- create PTY on first `ATTACH` (or at session creation if needed)
- bind shell command and environment
- persist PTY handle in session context

### IO Bridge

- non-blocking reads from PTY
- append PTY output to replay ring buffer
- write output to `STDOUT` stream with backpressure handling
- forward client input bytes from `STDIN` to PTY

### Resize

- apply `RESIZE(rows, cols)` immediately to PTY
- ignore duplicates if unchanged

## Replay Behavior

- maintain bounded ring buffer per session (by bytes, e.g. 1-4 MB)
- on `RESUME` with `resume-v1`:
  - replay buffered bytes first
  - then switch to live output
  - report `RESUME_OK.replay_bytes`

## Error Mapping

Recommended code mapping:

- auth token invalid/expired/consumed => `AUTH_FAILED`
- resume token invalid/expired/revoked => `SESSION_EXPIRED`
- missing session => `SESSION_NOT_FOUND`
- attach conflict/policy deny => `ATTACH_DENIED`
- parse/order/state errors => `PROTOCOL_ERROR`
- unexpected internal exceptions => `INTERNAL_ERROR`

## Configuration

Minimum config keys:

- `listen_addr`
- `tls_cert_file`
- `tls_key_file`
- `alpn` (default `neosh/1`)
- `session_timeout_seconds` (default 600)
- `auth_token_ttl_seconds` (default 60)
- `resume_token_ttl_seconds` (default 86400)
- `replay_buffer_bytes` (default 1048576)
- `token_rotation_on_resume` (default false)

## Observability (Logs Only)

Logs (structured):

- `conn_open`, `conn_close`
- `auth_ok`, `auth_failed`
- `attach_ok`, `resume_ok`, `resume_failed`
- `token_issued`, `token_consumed`, `token_revoked`
- `session_expired`, `session_terminated`

## Concurrency and Safety

- serialize control message handling per connection
- lock session row/state during transition
- use atomic CAS for auth token consume
- enforce per-connection max frame size

Disconnect cleanup ordering:

1. Mark connection state `Closing` (reject new control/data requests).
2. Stop data stream pumps (`STDIN`->PTY and PTY->`STDOUT`).
3. Run conditional stale cleanup using `(session_id, conn_id, attach_epoch)`.
4. If ownership was released:
   - move session to `DETACHED` and start detached timeout clock.
5. Close QUIC streams/connection.

Race safety requirements:

- If reconnect/reattach wins before old cleanup runs, old cleanup must no-op.
- `CLOSE` handling must be idempotent with disconnect cleanup.

## End-to-End Flow (Reference)

1. `neosh` bootstrap via SSH gets `session_id + auth_token`
2. `neosh` connects QUIC and sends `HELLO`
3. `neoshd` replies `HELLO_ACK`
4. `neosh` sends `AUTH(auth_token)`
5. `neoshd` validates and consumes token, replies `AUTH_OK(resume_token)`
6. `neosh` sends `ATTACH` (or later `RESUME`)
7. `neoshd` replies `ATTACH_OK` / `RESUME_OK`
8. stdin/stdout streams start
9. optional `DETACH` then reconnect with `RESUME`
10. `CLOSE` or timeout ends session

## Implementation Order

1. Framing + message dispatcher + protocol order checks
2. Session manager and transition validation
3. Token service (in-memory backend first)
4. AUTH/ATTACH happy path
5. PTY bridge and data streams
6. RESUME + replay
7. Keepalive and observability

## Dependency Profile (v0.1.0)

Required dependencies:

- QUIC/TLS: `quinn` + `rustls`
- PTY: `portable-pty`
- JSON: `serde` + `serde_json`
- IDs/time/random: `uuid`, `chrono`, `rand`
- logging: `tracing` + `tracing-subscriber`

Storage profile:

- in-memory maps for sessions/tokens (single process, no Redis/SQLite)
- periodic in-process sweeper for token/session expiry cleanup

Out of scope in v0.1.0:

- external persistence backends
- request rate limiting
