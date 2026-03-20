# REQ-v0.2.0-003: Resume Failure Observability

## Background

Operators need exact failure reason for `resume_failed` events to diagnose
conflicts, token issues, and state races.

## Requirement

Server logs MUST include specific structured failure reason.

1. `resume_failed.reason` MUST be a stable subtype (for example:
   `resume_token_expired`, `resume_token_revoked`, `attach_denied`).
2. `resume_failed.detail` MUST include low-level error text.
3. For attach failures, logs SHOULD include `session_state` and
   `attached_conn_id`.
4. When negotiated, logs SHOULD include `incoming_client_id` and
   `active_client_id`.

## Validation

1. Token and attach failures are distinguishable in production logs.
2. Resume race and conflict cases can be diagnosed using a single log line.
