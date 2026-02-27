# neosh v0.1.0 Implementation Guide

## Goal

Build `neosh` as a user-facing CLI client implementing
`neosh_protocol_v0.1.0.md` with:

- SSH bootstrap to start remote `neoshd new`
- QUIC/TLS control and data streams
- certificate fingerprint pin verification before `AUTH`
- attach/detach/resume terminal session workflow

## Runtime Architecture

`neosh` should be split into these runtime units:

- `CliApp`: command parsing and lifecycle orchestration
- `BootstrapClient`: SSH execution and bootstrap JSON parsing
- `QuicClient`: QUIC/TLS dial, ALPN `neosh/1`, stream management
- `ControlPlane`: control frame encode/decode and stateful dispatch
- `TerminalBridge`: local TTY input/output + resize events
- `SessionCache`: local metadata (`session_id`, `resume_token`, expiry)

## Suggested Package Layout

```text
neosh/
  cmd/neosh/main.*
  internal/cli/
  internal/bootstrap/
  internal/transport/quic/
  internal/protocol/
    framing.*
    messages.*
    state_machine.*
  internal/session/
    cache.*
  internal/terminal/
    tty.*
    bridge.*
  internal/security/
    fingerprint.*
  internal/observability/
    log.*
```

## neosh CLI Commands (v0.1.0)

### `neosh connect`

Purpose:

- create a new remote session via SSH bootstrap and attach immediately.

Example:

```bash
neosh connect user@example.com
```

Behavior:

1. SSH execute: `neoshd new --user "$USER"`.
2. Parse bootstrap JSON:
   - `session_id`
   - `auth_token`
   - `auth_token_expires_in_seconds`
   - `quic_addr`
   - `cert_fingerprint`
3. Dial QUIC to `quic_addr`.
4. Verify server cert fingerprint matches bootstrap value.
5. Run `HELLO` -> `AUTH` -> `ATTACH`.
6. Start `STDIN`/`STDOUT` streams and terminal bridge.

### `neosh resume`

Purpose:

- reconnect to an existing detached session using cached metadata.

Example:

```bash
neosh resume --session-id <uuid>
```

Behavior:

- load `session_id`, `resume_token`, `quic_addr`, `cert_fingerprint` from cache
- obtain a fresh single-use `auth_token` for this `session_id` via SSH-side
  command: `neoshd renew-auth --session-id <uuid> --user "$USER"`
- dial QUIC and verify fingerprint
- run `HELLO` -> `AUTH` -> `RESUME`
- on `RESUME_OK`, switch to live terminal mode

### `neosh detach`

Purpose:

- send `DETACH` and return local terminal to shell without terminating session.

Execution model:

- primary path: in-session detach trigger (escape sequence, e.g. `Ctrl-] d`)
- optional standalone command `neosh detach` sends detach request over local
  IPC control socket to the active `neosh` process
- if no active local session controller exists, command fails with clear error

Minimum IPC contract:

- socket path: `$XDG_RUNTIME_DIR/neosh/<session_id>.sock`
- socket file mode: `0600` (owner read/write only)
- server must validate peer process UID equals session owner UID
- request JSON: `{"type":"DETACH","session_id":"<uuid>"}`
- response JSON:
  - success: `{"ok":true}`
  - failure: `{"ok":false,"error":"..."}`

### `neosh version`

Expected output:

- `neosh/0.1.0`

## Bootstrap Protocol Handling

### SSH Execution Contract

- command should be executed so variables resolve on remote side (not local),
  for example: `ssh host 'neoshd new --user \"$USER\"'`
- capture stdout as bootstrap payload
- non-zero exit or invalid JSON => bootstrap failure
- preferred long-term approach: server derives user identity from SSH context,
  avoiding user argument dependence

Resume re-auth contract:

- command: `ssh host 'neoshd renew-auth --session-id <uuid> --user \"$USER\"'`
- parse same JSON fields as bootstrap (`session_id`, `auth_token`,
  `auth_token_expires_in_seconds`, `quic_addr`, `cert_fingerprint`)
- if returned `quic_addr` differs from cache, replace cache with latest value
- if returned `cert_fingerprint` differs from cache for same `session_id`,
  treat as protocol/security error and abort resume

### Bootstrap JSON Validation

Must validate:

- `session_id` non-empty
- `auth_token` non-empty
- `auth_token_expires_in_seconds` > 0
- `quic_addr` parseable as host:port
- `cert_fingerprint` has supported format (`sha256:<hex>`)

On validation failure:

- abort before QUIC dial
- surface structured user-facing error

## TLS Fingerprint Verification

Rules:

- QUIC connection is established with TLS 1.3 and ALPN `neosh/1`.
- before sending `AUTH`, client MUST verify peer leaf cert fingerprint
  equals bootstrap `cert_fingerprint`.
- mismatch => terminate connection and report trust error.
- verification result is session-scoped; do not silently reuse old fingerprint
  for a different bootstrap response.

## Control Plane Implementation

### Frame Codec

- write: `uint32_be length + utf-8 json bytes`
- read: parse length then payload
- enforce max control frame size limit

### State Machine

- `Init`: no QUIC connection
- `Connected`: QUIC established, control stream open
- `HelloDone`: `HELLO_ACK` received
- `Authenticated`: `AUTH_OK` received
- `Attached`: `ATTACH_OK` or `RESUME_OK` received
- `Detached`: after local detach
- `Closed`: terminal ended

Valid sequence (new session):

1. `HELLO` -> `HELLO_ACK`
2. `AUTH` -> `AUTH_OK`
3. `ATTACH` -> `ATTACH_OK`

Valid sequence (resume):

1. `HELLO` -> `HELLO_ACK`
2. `AUTH` -> `AUTH_OK`
3. `RESUME` -> `RESUME_OK`

Error handling:

- any `ERROR` frame transitions to terminal failure path
- unknown type locally treated as protocol error and abort

## Data Plane and Terminal Bridge

### Stream Startup Order

- open/use `STDIN`/`STDOUT` only after attach/resume success
- early stream traffic is protocol violation on client side

### Local Terminal Handling

- switch local TTY to raw mode on attach
- forward key bytes to `STDIN`
- render `STDOUT` bytes to terminal as-is
- on terminal size change, send `RESIZE(rows, cols)`
- restore TTY mode on exit, detach, or error

## Session Cache

Persist minimal metadata locally:

- `session_id`
- `resume_token`
- `resume_token_expires_at`
- `quic_addr`
- `cert_fingerprint`
- `updated_at`

Rules:

- update cache on `AUTH_OK`/successful `RESUME`
- delete cache on terminal `CLOSE`/session invalidation
- never persist `auth_token`

## Retry and Recovery Policy

- `AUTH_FAILED` before attach on fresh `connect` path:
  - rerun SSH `neoshd new` bootstrap to obtain fresh token
- `AUTH_FAILED` before `RESUME` on reconnect/resume path:
  - rerun SSH `neoshd renew-auth --session-id <uuid>` to obtain fresh
    single-use `auth_token` for the same session, then retry `AUTH`
- `SESSION_EXPIRED` on resume:
  - drop cached session and require new `connect`
- transient network failure while attached:
  - attempt reconnect with bounded backoff:
    - SSH `renew-auth` for fresh `auth_token`
    - QUIC `HELLO` -> `AUTH` -> `RESUME`
- fingerprint mismatch:
  - hard fail; require explicit new bootstrap

## Logging (Client)

Structured log events:

- `bootstrap_start`, `bootstrap_ok`, `bootstrap_fail`
- `quic_connect_start`, `quic_connect_ok`, `quic_connect_fail`
- `fingerprint_verify_ok`, `fingerprint_verify_fail`
- `auth_ok`, `auth_fail`
- `attach_ok`, `resume_ok`, `resume_fail`
- `detach_sent`, `close_received`

## Test Plan

### Unit Tests

- control frame encode/decode
- bootstrap JSON validation
- fingerprint normalization and compare
- state machine transition guards
- session cache expiry/update behavior

### Integration Tests

- `connect` happy path to interactive shell
- `detach` then `resume` happy path
- `resume` path must execute SSH `renew-auth`, then `HELLO` -> `AUTH` -> `RESUME`
- expired/consumed `auth_token` triggers bootstrap retry
- `resume_token` expired path handled cleanly
- fingerprint mismatch blocks `AUTH`

### Race/Failure Tests

- disconnect during active streaming
- reconnect overlaps with local user detach request
- duplicate control replies or out-of-order replies

## Acceptance Criteria

- `neosh connect` establishes interactive session end-to-end.
- `neosh detach` leaves session resumable.
- `neosh resume` restores session before timeout.
- fingerprint pin verification is always enforced before `AUTH`.
- protocol/order violations fail fast with clear diagnostics.
