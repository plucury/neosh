# REQ-v0.2.0-002: Same-Client Fast Reconnect Takeover

## Background

With unstable networks, the same client can reconnect quickly while old
connection cleanup is still in progress, causing transient `ATTACH_DENIED`.

## Requirement

Enable same-client takeover when `client-id-v1` is negotiated.

1. Server stores `active_client_id` per session while attached.
2. If incoming `RESUME.client_id == active_client_id`, server MAY replace
   `attached_conn_id` immediately (takeover).
3. If incoming `client_id` differs, server MUST reject with `ATTACH_DENIED`
   unless policy explicitly allows cross-client takeover.
4. On takeover, previous connection MUST be moved out of attached role
   (close or suspended behavior, implementation-defined).

## Validation

1. A reconnect from same `client_id` succeeds without manual retry.
2. Different `client_id` cannot steal session by default.
3. Session state remains `ATTACHED` to exactly one connection at a time.
