# REQ-v0.1.1-001: neoshd Initial Attach Timeout

## Background

In v0.1.0, a `neoshd new` process can remain alive indefinitely when no client
ever performs `ATTACH` or `RESUME`. This can leave idle bootstrap-created
sessions running forever.

## Requirement

`neoshd new` MUST support a configurable maximum wait time for the first client
attach/resume.

- Flag: `--initial-attach-timeout`
- Unit: seconds
- Default: `300` (5 minutes)
- `0` means disabled

## Behavior

1. Timer starts when `neoshd new` creates the session.
2. Timeout check applies only while session state is `CREATED` (before first
   successful `ATTACH` or `RESUME`).
3. If timeout is reached:
   - session state MUST transition to `EXPIRED`
   - `neoshd` process MUST stop serving and exit its accept loop
   - server SHOULD emit an `initial_attach_timeout` event for observability
   - server SHOULD emit a shutdown event that includes the stop reason
4. On startup, server SHOULD log whether initial-attach timeout is armed or
   disabled, including configured timeout seconds when armed.
5. After first successful attach/resume, this timeout MUST no longer apply.
   Existing detached timeout policy (`--session-timeout`) continues to control
   post-detach expiry (use `0` to disable).

## Compatibility

- Backward compatible for existing clients; change is server-side lifecycle
  behavior only.
- Operators can set `--initial-attach-timeout 0` to keep legacy indefinite-wait
  behavior.
