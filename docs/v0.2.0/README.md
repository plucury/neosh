# neosh v0.2.0 Docs

- [`neoshd_implementation.md`](./neoshd_implementation.md): Server implementation guide for `client_id` and same-client takeover.
- [`neosh_implementation.md`](./neosh_implementation.md): Client implementation guide for persistent `client_id` and protocol integration.
- [`neosh-client-c_implementation.md`](./neosh-client-c_implementation.md): FFI implementation guide for `client_id` and XCFramework delivery.
- [`../../neosh_protocol_v0.1.0.md`](../../neosh_protocol_v0.1.0.md): Wire protocol spec with `v0.2.0` extensions documented inline.
- [`REQ-v0.2.0-001.md`](./REQ-v0.2.0-001.md): Protocol support for `client_id` (`client-id-v1`).
- [`REQ-v0.2.0-002.md`](./REQ-v0.2.0-002.md): Same-client fast reconnect takeover.
- [`REQ-v0.2.0-003.md`](./REQ-v0.2.0-003.md): Resume failure observability and diagnostics.
- [`REQ-v0.2.0-004.md`](./REQ-v0.2.0-004.md): Compatibility and rollout requirements.

Notes:

- Current binaries advertise `neosh/0.2.0` and `neoshd/0.2.0`.
- The on-wire `protocol_version` value is still `0.1.0`; `0.2.0` behavior is
  activated through capabilities and optional control-message fields.
