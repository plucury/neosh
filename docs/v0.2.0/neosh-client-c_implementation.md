# neosh-client-c v0.2.0 Implementation Guide

## Scope

This document describes the current C ABI exported from
`ffi/neosh-client-c/src/lib.rs` and declared in
`ffi/neosh-client-c/include/neosh_client.h`.

Version strings exposed by the library:

- client version: `neosh/0.2.0`
- wire protocol version: `0.1.0`

Reference protocol: `../../neosh_protocol_v0.1.0.md`

## Exported API Surface

### Version and lifecycle

- `neosh_client_version`
- `neosh_client_protocol_version`
- `neosh_client_version_from_core`
- `neosh_client_connect`
- `neosh_client_free`
- `neosh_client_last_error`
- `neosh_client_last_error_global`

### `client_id` helpers

- `neosh_client_get_or_create_client_id`
- `neosh_client_set_client_id`

### Control payload builders

- `neosh_client_build_hello_json`
- `neosh_client_build_auth_json`
- `neosh_client_build_resume_json`

### Raw control/data transport

- `neosh_client_send_control_json`
- `neosh_client_recv_control_json`
- `neosh_client_open_data_streams`
- `neosh_client_send_stdin`
- `neosh_client_finish_stdin`
- `neosh_client_recv_stdout`

The ABI is additive: raw JSON send/recv remains available for custom protocol
composition, but the helper builders encode the current recommended v0.2.0
client behavior.

## Connection Model

`neosh_client_connect` performs:

1. QUIC connect to `quic_addr`
2. certificate fingerprint verification against `expected_fingerprint`
3. opening of the control bidirectional stream

It does not automatically send `HELLO`, `AUTH`, or `RESUME`; callers control
that sequence explicitly.

## `client_id` Handling

Each `NeoshClient` handle stores:

- `client_id: Option<Uuid>`

Resolution order:

1. if the caller previously used `neosh_client_set_client_id`, use that value
2. otherwise lazily load or create a persistent UUID from disk

Default storage path:

- `$XDG_STATE_HOME/neosh/client_id`
- fallback: `$HOME/.local/state/neosh/client_id`

Behavior:

- missing file -> create UUIDv4
- invalid file -> rotate to a new UUIDv4
- writes are atomic
- file mode is `0600` on Unix

## Builder Semantics

### HELLO builder

`neosh_client_build_hello_json` emits JSON equivalent to:

- `type = "HELLO"`
- `protocol_version = "0.1.0"`
- `client_version = "neosh/0.2.0"`
- `capabilities = ["stdin-bytes", "resume-v1", "client-id-v1"]`

### AUTH builder

`neosh_client_build_auth_json`:

- validates `auth_token`
- resolves `client_id`
- emits `AUTH` JSON including `client_id`

### RESUME builder

`neosh_client_build_resume_json`:

- validates `session_id`
- validates `resume_token`
- resolves `client_id`
- emits `RESUME` JSON including `client_id`

## Error Model

Return codes:

- `0`: success
- `-1`: invalid argument
- `-2`: output buffer too small
- `-3`: stream not ready / wrong state
- `-4`: internal or transport error

Common failure cases:

- null or empty string arguments
- invalid UUID in `session_id` or `client_id`
- failed `client_id` persistence
- buffer too small when copying builder output
- control/data stream used before initialization

When `neosh_client_connect` fails, callers must read
`neosh_client_last_error_global()`. For handle-scoped failures, use
`neosh_client_last_error(client)`.

## Stream Layout

After control-plane success, callers open data streams with
`neosh_client_open_data_streams`:

- one client -> server unidirectional STDIN stream
- one server -> client unidirectional STDOUT stream

The library follows the same framing contract as the Rust client:

- control messages are raw JSON payloads passed into/out of the API
- QUIC frame length-prefixing is handled internally
- stdout is a byte stream; EOF is reported as `out_len = 0` and `eof = 1`

## XCFramework Build

The iOS packaging flow is implemented in the repository `Makefile`:

```bash
make build-client-c-lib-ios
```

To override the minimum deployment target:

```bash
make build-client-c-lib-ios IOS_DEPLOYMENT_TARGET=17.0
```

Current build characteristics:

- builds `aarch64-apple-ios`
- builds `aarch64-apple-ios-sim`
- outputs `dist/neosh_client.xcframework`

If a zip artifact is needed for release distribution, package the generated
XCFramework after the build step.

## Compatibility Notes

- FFI helpers always emit `client_id`, but older servers can ignore the extra
  JSON field.
- Apps that already compose raw JSON can continue to do so.
- Same-client takeover still depends on the server negotiating
  `client-id-v1`.
