# neoshd v0.2.0 Implementation Guide

## Scope

This document describes the current `neoshd/0.2.0` behavior as implemented in
`src/bin/neoshd.rs`.

Wire compatibility:

- `protocol_version` remains `0.1.0`
- capability negotiation enables `client-id-v1`
- `AUTH` and `RESUME` accept optional `client_id`

Reference protocol: `../../neosh_protocol_v0.1.0.md`

## CLI Surface

Implemented subcommands:

- `neoshd new`
- `neoshd renew-auth`
- `neoshd version`

`neoshd new` options and defaults:

- `--user <name>` required
- `--port-range 30000:39999`
- `--bind-server ssh`
- `--tls-cert <pem>`
- `--tls-key <pem>`
- `--working-directory <dir>`
- `--command <shell-command>`
- `--session-timeout 600`
- `--initial-attach-timeout 300`
- `--auth-token-ttl 60`
- `--resume-token-ttl 86400`
- `--quic-idle-timeout-seconds 60`
- `--replay-buffer-bytes 1048576`

`neoshd renew-auth` issues a new single-use `auth_token` for an existing
session through a local Unix socket owned by the session process.

## Bootstrap Output

`neoshd new` prints one JSON object to stdout:

- `session_id`
- `auth_token`
- `auth_token_expires_in_seconds`
- `quic_addr`
- `cert_fingerprint`

The client treats this JSON as the SSH bootstrap handoff.

## Network Binding and TLS

### `--bind-server`

Implemented behaviors:

- `ssh`: use the server-side address from `SSH_CONNECTION`, otherwise
  fall back to `127.0.0.1`
- `any`: bind `0.0.0.0`
- explicit IP or hostname: resolve and try each resulting address

The server scans the requested `--port-range` until it finds a free UDP port.

### TLS

- If both `--tls-cert` and `--tls-key` are provided, `neoshd` loads that pair.
- Otherwise it generates a self-signed certificate for the process.
- The published `cert_fingerprint` is `sha256:<hex>` over the DER certificate.
- QUIC ALPN is always `neosh/1`.

## Runtime Model

Current runtime is single-process and single-session:

- one `neoshd new` process owns exactly one session
- one interactive PTY is spawned lazily on first `ATTACH` or `RESUME`
- PTY shell defaults to `$SHELL`, falling back to `sh`

Optional startup controls:

- `--working-directory` sets the initial PTY directory
- `--command` runs once before entering the interactive shell

The server maintains:

- `SessionManager` state (`CREATED`, `ATTACHED`, `DETACHED`, `EXPIRED`, `TERMINATED`)
- `active_client_id: Option<Uuid>`
- `conn_registry: HashMap<Uuid, Connection>`
- replay buffer capped by `--replay-buffer-bytes`

Local `renew-auth` IPC socket path:

- `$XDG_RUNTIME_DIR/neoshd/<session-id>.sock`
- fallback: `/tmp/neoshd/<session-id>.sock`

## Control Plane Behavior

### HELLO

The server always advertises:

- `stdin-bytes`
- `resume-v1`

It adds `client-id-v1` to `HELLO_ACK.capabilities` only when the client
advertised it in `HELLO`.

If `client-id-v1` is not negotiated, the server ignores `client_id` semantics
and keeps legacy exclusive attach behavior.

### AUTH

Behavior:

1. Validate and consume the single-use `auth_token`.
2. Issue a new `resume_token`.
3. Record `AUTH.client_id` only if `client-id-v1` was negotiated.
4. Return `AUTH_OK`.

Missing `client_id` does not fail `AUTH`.

### ATTACH

`ATTACH` is always exclusive:

- success requires no current attached connection
- on success, `active_client_id` is set from the authenticated connection when
  capability `client-id-v1` was negotiated
- PTY runtime is started if necessary

### RESUME

The implemented decision logic is:

1. Validate `session_id`.
2. Validate `resume_token`.
3. If `client-id-v1` was negotiated and:
   - session is `ATTACHED`
   - incoming `client_id` is present
   - incoming `client_id == active_client_id`
   then the server performs same-client takeover.
4. Else if negotiated and both client IDs are present but differ, return
   `ATTACH_DENIED`.
5. Otherwise fall back to normal exclusive attach behavior.

Same-client takeover details:

- session owner switches to the new connection first
- previous attached connection is removed from `conn_registry`
- old QUIC connection is then closed with reason `session takeover`
- `attach_epoch` is used so stale cleanup from the old connection does not
  detach the new owner

After successful `RESUME`, `RESUME_OK.replay_bytes` reports the number of bytes
currently present in the replay buffer.

## Timeouts and Cleanup

Implemented timeout rules:

- `initial_attach_timeout`: expires untouched `CREATED` sessions
- `session_timeout`: expires detached sessions
- `auth_token_ttl`: validity of bootstrap `AUTH`
- `resume_token_ttl`: validity of `RESUME`
- `quic_idle_timeout_seconds`: QUIC transport idle timeout

When a session expires or terminates, `active_client_id` is cleared.

## Observability

The server writes structured JSON logs to stderr. Important events include:

- `server_start`, `server_stop`, `server_auto_shutdown`
- `token_issued`, `token_consumed`
- `conn_open`, `conn_close`, `conn_accept_error`, `conn_error`
- `auth_ok`, `auth_failed`
- `attach_ok`
- `resume_ok`, `resume_failed`
- `session_takeover`
- `pty_spawn`
- `session_expired`, `initial_attach_timeout`

`resume_failed` logs currently include:

- `reason`
- `detail`
- `session_state` when available
- `attached_conn_id` when available
- `incoming_client_id`
- `active_client_id`

## Compatibility Notes

- Older clients without `client_id` continue to work.
- New clients can send `client_id` immediately; the server only acts on it when
  capability negotiation succeeds.
- Error code mapping is stable:
  - expired/revoked `resume_token` -> `SESSION_EXPIRED`
  - binding mismatch / consumed / missing / wrong-type resume token -> `AUTH_FAILED`
  - client ID mismatch on attached session -> `ATTACH_DENIED`
