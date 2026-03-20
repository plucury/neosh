# neosh v0.2.0 Implementation Guide

## Scope

This document describes the current `neosh/0.2.0` client behavior as
implemented in `src/bin/neosh.rs`.

Wire compatibility:

- `protocol_version` sent on the wire is still `0.1.0`
- client capability list is `["stdin-bytes", "resume-v1", "client-id-v1"]`
- `client_id` is always included in `AUTH` and `RESUME`

Reference protocol: `../../neosh_protocol_v0.1.0.md`

## CLI Surface

Implemented subcommands:

- `neosh connect <target>`
- `neosh resume --session-id <uuid> [--target <ssh-target>]`
- `neosh detach [--session-id <uuid>]`
- `neosh version`

Important `connect` options:

- `--neoshd-path <path>`
- `--remote-working-directory <dir>`
- `--remote-command <command>`
- `--quic-idle-timeout-seconds <seconds>` default `60`
- `--neoshd-log-file[=<path>]`

Important `resume` options:

- `--target <ssh-target>` overrides cached target
- `--neoshd-path <path>`
- `--quic-idle-timeout-seconds <seconds>` default `60`

## Local State

### Persistent client identity

The client stores a stable UUID at:

- `$XDG_STATE_HOME/neosh/client_id`
- fallback: `$HOME/.local/state/neosh/client_id`

Behavior:

1. If the file contains a valid UUID, it is reused.
2. If the file is missing, a new UUIDv4 is created.
3. If the file is invalid, it is rotated to a new UUIDv4.
4. Writes are atomic and use file mode `0600`.

`client_id` is not secret. It is a reconnect hint used only when the server
negotiates capability `client-id-v1`.

### Resume cache

Successful `AUTH_OK` writes a session cache entry to:

- `$XDG_CACHE_HOME/neosh/sessions/<session-id>.json`
- fallback: `$HOME/.cache/neosh/sessions/<session-id>.json`

Each entry stores:

- `session_id`
- `ssh_target`
- `resume_token`
- `resume_token_expires_at`
- `quic_addr`
- `cert_fingerprint`
- `updated_at`

`neosh resume` reads this cache, rejects expired entries locally, and deletes
them when `SESSION_EXPIRED` is returned by the server.

### Local detach IPC

Attached interactive sessions expose a local Unix socket at:

- `$XDG_RUNTIME_DIR/neosh/<session-id>.sock`
- fallback: `/tmp/neosh/<session-id>.sock`

`neosh detach` connects to this socket and sends a local control request that
causes the attached client process to emit protocol `DETACH`.

## Bootstrap and Handshake

### `connect`

`neosh connect` performs:

1. SSH bootstrap via `neoshd new --user "$USER"` plus any requested remote
   options.
2. QUIC connection with certificate fingerprint verification.
3. `HELLO`
4. `AUTH`
5. `ATTACH`

When `--neoshd-log-file` is set, the generated SSH bootstrap command appends
remote `neoshd` stderr to the requested log file.

### `resume`

`neosh resume` performs:

1. Session cache lookup.
2. SSH bootstrap via `neoshd renew-auth --session-id <uuid> --user "$USER"`.
3. QUIC reconnect and fingerprint verification.
4. `HELLO`
5. `AUTH`
6. `RESUME`

For an existing cached session, a changed certificate fingerprint is treated as
an error. A changed `quic_addr` is accepted and the cache is updated.

## Reconnect Behavior

The client implements two bounded retry loops:

- `connect`: retries once after `AUTH_FAILED`
- `resume`: retries up to three times on connection/auth failure

If an attached interactive session loses the QUIC connection unexpectedly:

- `neosh` returns `Disconnected(session_id)` internally
- if stdin/stdout are TTYs, it automatically runs the resume flow with backoff
- if stdin/stdout are not TTYs, it exits without attempting terminal reattach

When the server also negotiated `client-id-v1`, same-client reconnect can take
over a stale attached session without requiring a prior `DETACH`.

## Terminal Behavior

- Detach hotkey: `Ctrl-a`, then `d`
- `^H` is normalized to `DEL` (`0x7f`) before sending to the server
- an initial `RESIZE` is sent immediately after `ATTACH_OK` or `RESUME_OK`
- local raw mode is enabled while the terminal bridge is active

Remote shell exit behavior:

- when the remote stdout stream reaches EOF, the client sends `CLOSE`
- the local session cache entry is deleted
- the command returns successfully

## Logging

The client emits structured JSON logs on stderr. Important events include:

- `bootstrap_start`, `bootstrap_ok`, `bootstrap_fail`
- `quic_connect_start`, `quic_connect_ok`, `quic_connect_fail`
- `fingerprint_verify_ok`, `fingerprint_verify_fail`
- `auth_ok`, `auth_fail`
- `attach_ok`
- `resume_ok`, `resume_fail`
- `detach_sent`
- `close_sent`, `close_received`

`client_id` is included in reconnect-related success/failure logs, but raw token
values are not logged.

## Compatibility Notes

- Against older servers, extra JSON fields remain ignorable and `client_id`
  falls back to best-effort behavior.
- Same-client takeover is server-controlled and only active after
  `HELLO_ACK.capabilities` includes `client-id-v1`.
- Non-interactive sessions do not auto-resume the terminal after disconnect.
