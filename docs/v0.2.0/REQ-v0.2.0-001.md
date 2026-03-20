# REQ-v0.2.0-001: Protocol `client_id` Support

## Background

Current protocol does not carry stable client identity, making it impossible to
safely distinguish same-client reconnect from different-client competition.

## Requirement

Add optional `client_id` fields and capability negotiation.

1. `HELLO.capabilities` introduces `client-id-v1`.
2. `AUTH` accepts optional `client_id`.
3. `RESUME` accepts optional `client_id`.
4. If capability is not negotiated, behavior MUST remain v0.1.0 compatible.

## Validation

1. v0.2.0 client/server negotiate `client-id-v1` successfully.
2. v0.1.0 clients interoperate without protocol failure.
3. Unknown fields remain ignored.
