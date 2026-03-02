# neosh

`neosh` is a QUIC-based remote terminal system with SSH bootstrap and session resume.

- Client: `neosh`
- Server: `neoshd`
- Protocol: [`neosh_protocol_v0.1.0.md`](./neosh_protocol_v0.1.0.md)
- ALPN: `neosh/1`

## neosh vs SSH

| Topic | SSH | neosh |
| --- | --- | --- |
| Transport | TCP | QUIC (UDP) |
| Session continuity on network switch / transient loss | Usually reconnect manually | Built-in detach/resume workflow |
| Bootstrap | Native SSH | Uses SSH for bootstrap and trust handoff |
| Long-running remote tasks | Usually use `tmux`/`screen` manually | Session lifecycle is first-class (`detach`, `resume`) |
| Trust verification | Host key model | Verifies `neoshd` TLS fingerprint from SSH bootstrap before `AUTH` |
| Interactive latency under unstable networks | Good on stable TCP | Usually better resilience under lossy/mobile links |

## Why neosh

- QUIC-based interactive shell with better tolerance to transient network jitter/loss
- SSH bootstrap keeps deployment practical in existing SSH environments
- Built-in resumable detached sessions (no extra multiplexer required for basic workflow)
- Tokenized auth/resume flow (`auth_token` + `resume_token`) with explicit lifecycle controls
- Simple operator-facing CLI (`connect`, `detach`, `resume`)

## Best Use Cases

- Mobile or roaming networks where temporary disconnections are common
- Remote development / ops sessions that should survive client reconnects
- Multi-hop environments where SSH is already accepted as bootstrap entry point
- Teams that want a structured resumable session model without relying only on `tmux` conventions

## Quick Start

### Install Latest `neoshd` (Linux/macOS)

Install the latest GitHub Release binary to `/usr/local/bin/neoshd`:

```bash
curl -fsSL https://raw.githubusercontent.com/plucury/neosh/main/scripts/install_neoshd.sh | bash
```

Optional install directory override:

```bash
INSTALL_DIR="$HOME/.local/bin" curl -fsSL https://raw.githubusercontent.com/plucury/neosh/main/scripts/install_neoshd.sh | bash
```

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
