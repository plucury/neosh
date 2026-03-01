# neosh

`neosh` is a QUIC-based remote terminal system with SSH bootstrap and session resume.

- Client: `neosh`
- Server: `neoshd`
- Protocol: [`neosh_protocol_v0.1.0.md`](./neosh_protocol_v0.1.0.md)
- ALPN: `neosh/1`

## Why neosh

- Fast interactive shell over QUIC
- SSH-based bootstrap and identity flow
- Resumable detached session workflow
- TLS fingerprint pin verification before `AUTH`
- Simple CLI for daily use (`connect`, `detach`, `resume`)

## Quick Start

### Build

```bash
# debug
make build

# release
make build RELEASE=1
```

Binaries:

- debug: `target/debug/neosh`, `target/debug/neoshd`
- release: `target/release/neosh`, `target/release/neoshd`

### Connect

```bash
neosh connect user@host
```

If remote `neoshd` is not in `PATH`:

```bash
neosh connect user@host --neoshd-path /path/to/neoshd
```

Enable remote `neoshd` stderr logging:

```bash
# default path: /tmp/neoshd.log
neosh connect user@host --neoshd-log-file

# custom path
neosh connect user@host --neoshd-log-file /tmp/my-neoshd.log
```

## Session Workflow

Detach from attached session:

- Hotkey: press `Ctrl-a`, then `d`
- Or from another terminal:

```bash
neosh detach
```

Resume later:

```bash
neosh resume --session-id <session-id> --target user@host
```

Resume with explicit remote server path:

```bash
neosh resume --session-id <session-id> --target user@host --neoshd-path /path/to/neoshd
```

Exit semantics:

- `Ctrl-a d` / `neosh detach`: session stays resumable
- `logout` / `exit` / `Ctrl-d`: session terminates and cannot be resumed

## CLI Help

```bash
neosh --help
neosh connect --help
neosh resume --help
neosh detach --help
neoshd --help
```

## Security Notes (v0.1.0)

- `auth_token`: opaque, short-lived, single-use, only for `AUTH`
- `resume_token`: opaque, revocable, only for `RESUME`
- If `auth_token` expires before `AUTH`, client must bootstrap again
- Reconnect/resume requires `renew-auth` to get fresh `auth_token`, then `AUTH` before `RESUME`
- `neosh` verifies server certificate fingerprint from SSH bootstrap before `AUTH`
- `neoshd` default bind policy follows SSH bootstrap style (`bind-server=ssh`, port range `30000-39999`)

## Project Docs

- Protocol spec: [`neosh_protocol_v0.1.0.md`](./neosh_protocol_v0.1.0.md)
- Server docs: [`docs/v0.1.0/neoshd_implementation.md`](./docs/v0.1.0/neoshd_implementation.md)
- Client docs: [`docs/v0.1.0/neosh_implementation.md`](./docs/v0.1.0/neosh_implementation.md)
- Delivery test guide: [`docs/v0.1.0/neosh_delivery_test.md`](./docs/v0.1.0/neosh_delivery_test.md)
