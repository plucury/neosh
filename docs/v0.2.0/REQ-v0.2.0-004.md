# REQ-v0.2.0-004: Compatibility and Rollout

## Background

`client_id` and same-client takeover must not break existing deployments.

## Requirement

Define staged rollout and compatibility guarantees.

1. Feature gate: behavior requiring `client_id` is only active when
   `client-id-v1` is negotiated.
2. Default policy: deny cross-client takeover.
3. Existing v0.1.0 clients must continue to attach/resume as before.

## Validation

1. Mixed fleet test: v0.1.0 and v0.2.0 clients run against v0.2.0 server.
2. No protocol-level incompatibility introduced in control framing.
