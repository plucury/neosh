# neoshd v0.1.0 Implementation Guide

## Goal

Build `neoshd` as a QUIC-based remote terminal session server implementing
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

## neoshd CLI Commands (v0.1.0)

### `neoshd new`

Purpose:

- SSH-bootstrap session entrypoint.
- create one new session, print bootstrap metadata, then keep serving that
  session in foreground until close/timeout.

Expected invocation:

- called over SSH after user authentication.

Example:

```bash
neoshd new --user "$USER"
```

All flags optional with defaults:

- `--port-range` (default: `30000:39999`): QUIC UDP port allocation range.
- `--bind-server` (default: `ssh`): bind address policy (`ssh`, `any`, or explicit `<ip-or-host>`).
- `--tls-cert` (default: empty): TLS cert file path override.
- `--tls-key` (default: empty): TLS key file path override.
- `--working-directory` (default: empty): initial shell working directory.
- `--command` (default: empty): command to run once before interactive shell.
- `--session-timeout` (default: `600`): detached-session timeout in seconds.
- `--auth-token-ttl` (default: `60`): bootstrap auth token TTL in seconds.
- `--resume-token-ttl` (default: `86400`): resume token TTL in seconds.
- `--quic-idle-timeout-seconds` (default: `60`): QUIC idle timeout.
- `--replay-buffer-bytes` (default: `1048576`): replay ring buffer size in bytes.

Port allocation policy for `neoshd new`:

- default port search range is `30000-39999`.
- default `--bind-server=ssh` means bind address is selected from the SSH
  connection endpoint (server address used by SSH session).
- `--bind-server=any` means bind wildcard address (`0.0.0.0` and/or `::`).
- `--bind-server=<ip-or-host>` means bind the explicitly provided address.
- if no port is available in range, startup MUST fail with a structured error.
- invalid `--bind-server` value MUST fail fast with a structured config error.
- returned `quic_addr` is the source of truth; client MUST always use returned
  address, not assumed defaults.
- returned `quic_addr` MUST be client-routable for this session (the client
  must be able to dial it directly on the intended QUIC path).

TLS behavior in v0.1.0:

- if `--tls-cert/--tls-key` are missing, `neoshd` MUST auto-generate a self-signed cert.
- generated cert/key are kept in process runtime memory and used for bootstrap fingerprint pinning.
- if only one of `--tls-cert` or `--tls-key` is provided, startup MUST fail with config error.
- certificate fingerprint MUST remain stable for the lifetime of a given
  `session_id` (from `new` until `TERMINATED/EXPIRED`).

Success output (stdout JSON):

```json
{
  "session_id": "uuid",
  "auth_token": "opaque-token",
  "auth_token_expires_in_seconds": 60,
  "quic_addr": "203.0.113.10:30001",
  "cert_fingerprint": "sha256:ab12cd34..."
}
```

After printing JSON, process continues running and serves this session.
`quic_addr` MUST be reachable by the client connection path.

Failure behavior:

- non-zero exit code.
- stdout/stderr returns structured error JSON.
- must not print partial token material.

### `neoshd version`

Purpose:

- print daemon version for interoperability checks.

Example:

```bash
neoshd version
```

Expected output:

- `neoshd/0.1.0`

### `neoshd renew-auth`

Purpose:

- issue a fresh single-use `auth_token` for an existing session to support
  reconnect/resume on a new QUIC connection.

Example:

```bash
neoshd renew-auth --session-id <uuid> --user "$USER"
```

Success output (stdout JSON):

```json
{
  "session_id": "uuid",
  "auth_token": "opaque-token",
  "auth_token_expires_in_seconds": 60,
  "quic_addr": "203.0.113.10:30001",
  "cert_fingerprint": "sha256:ab12cd34..."
}
```

Rules:

- server MUST verify SSH-authenticated user owns `session_id`.
- returned `quic_addr`/`cert_fingerprint` MUST reflect current active listener.
- token is single-use and consumed by next successful `AUTH`.
- for active sessions, `cert_fingerprint` returned by `renew-auth` MUST equal
  the fingerprint originally issued for that `session_id`.

## Protocol Implementation

### 0. Bootstrap Token Issuance Interface

`auth_token` issuance must be implemented explicitly for SSH bootstrap.

Recommended interface:

- `neoshd new` subcommand (invoked over SSH).
- `neoshd renew-auth --session-id <uuid>` subcommand (invoked over SSH).
- Input context from SSH session/user identity.
- Output JSON (stdout) with:
  - `session_id`
  - `auth_token`
  - `auth_token_expires_in_seconds`
  - `quic_addr`
  - `cert_fingerprint` (SHA-256 over DER cert)

Behavior:

- `neoshd new`:
  - create a new session owned by SSH-authenticated user
  - issue single-use opaque `auth_token` bound to `session_id + user_id`
  - persist token record before returning response
- `neoshd renew-auth`:
  - MUST NOT create a new `session_id`
  - MUST issue a fresh single-use opaque `auth_token` for existing `session_id`
  - MUST verify SSH-authenticated user owns target session before issuance
  - MUST persist token record before returning response

Session/process policy in v0.1.0:

- each `neoshd new` invocation MUST create a new `session_id`
- `neoshd renew-auth` MUST reuse the provided existing `session_id`
- bootstrap MUST return active QUIC address and cert fingerprint for pin validation
- bootstrap MUST ensure returned `quic_addr` is client-routable for this session
- one `neoshd new` process serves one session and exits on close/timeout

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
6. Client must complete certificate fingerprint pin verification before `AUTH`.

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

- `listen_addr` (derived from `bind_server` + selected port from `port_range`)
- `port_range` (default `30000:39999`)
- `bind_server` (default `ssh`)
- `tls_cert_file` (default empty; auto-generate when empty with key)
- `tls_key_file` (default empty; auto-generate when empty with cert)
- `alpn` (default `neosh/1`)
- `session_timeout_seconds` (default `600`)
- `auth_token_ttl_seconds` (default `60`)
- `resume_token_ttl_seconds` (default `86400`)
- `replay_buffer_bytes` (default `1048576`)
- `token_rotation_on_resume` (default `false`)

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

1. `neosh` bootstrap via SSH gets `session_id + auth_token + quic_addr + cert_fingerprint`
2. `neosh` connects QUIC to `quic_addr`
3. `neosh` verifies peer cert fingerprint matches bootstrap value
4. `neosh` sends `HELLO`
5. `neoshd` replies `HELLO_ACK`
6. `neosh` sends `AUTH(auth_token)`
7. `neoshd` validates and consumes token, replies `AUTH_OK(resume_token)`
8. `neosh` sends `ATTACH` (or later `RESUME`)
9. `neoshd` replies `ATTACH_OK` / `RESUME_OK`
10. stdin/stdout streams start
11. optional `DETACH`
12. reconnect path: SSH `neoshd renew-auth --session-id <uuid>` to get fresh `auth_token`
13. reconnect path: QUIC `HELLO` -> `AUTH` -> `RESUME`
14. `CLOSE` or timeout ends session

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
