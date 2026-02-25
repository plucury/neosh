# neosh Protocol Specification

## Version: v0.1.0

ALPN: `neosh/1`

------------------------------------------------------------------------

# 1. Overview

neosh is a QUIC-based remote terminal protocol.

Design goals:

-   Use SSH for initial authentication (bootstrap)
-   Use QUIC as the data plane
-   Provide session resume capability
-   Provide raw terminal byte stream transport
-   Be extensible for future features

This version (v0.1.0) is a byte-stream model and does NOT include
terminal state synchronization.

------------------------------------------------------------------------

# 2. Transport Layer

-   QUIC version: IETF QUIC (implementation-defined)
-   TLS: TLS 1.3 (mandatory)
-   ALPN: `neosh/1`
-   Encryption: Required

------------------------------------------------------------------------

# 3. Connection Model

Each QUIC connection represents a client connection.

Each connection MUST:

1.  Open a bidirectional stream as the control stream
2.  Complete AUTH over the control stream
3.  Only after AUTH success may data streams be used

------------------------------------------------------------------------

# 4. Stream Layout

## 4.1 Control Stream (bidirectional)

-   Reliable
-   Ordered
-   Required

Used for:

-   HELLO
-   AUTH
-   ATTACH
-   RESUME
-   RESIZE
-   PING/PONG
-   DETACH
-   CLOSE
-   ERROR

------------------------------------------------------------------------

## 4.2 STDIN Stream (client → server, unidirectional)

-   Reliable
-   Ordered
-   Raw byte stream
-   Represents keyboard input

------------------------------------------------------------------------

## 4.3 STDOUT Stream (server → client, unidirectional)

-   Reliable
-   Ordered
-   Raw PTY output (ANSI + UTF-8)

------------------------------------------------------------------------

# 5. Framing

Control messages use length-prefixed JSON:

uint32 length (big-endian)\
bytes payload (UTF-8 JSON)

Unknown fields MUST be ignored.\
Unknown message types MUST produce ERROR.

------------------------------------------------------------------------

# 6. Control Messages

## 6.1 HELLO

Client → Server

``` json
{
  "type": "HELLO",
  "protocol_version": "0.1.0",
  "client_version": "neosh/0.1.0",
  "capabilities": ["stdin-bytes", "resume-v1"]
}
```

Server → Client

``` json
{
  "type": "HELLO_ACK",
  "server_version": "neosh-agent/0.1.0",
  "capabilities": ["stdin-bytes", "resume-v1"],
  "session_timeout_seconds": 600
}
```

------------------------------------------------------------------------

## 6.2 AUTH

Client → Server

``` json
{
  "type": "AUTH",
  "method": "ssh-token",
  "token": "base64-token"
}
```

Server → Client (success)

``` json
{
  "type": "AUTH_OK",
  "session_id": "uuid",
  "resume_token": "base64-token",
  "expires_in_seconds": 86400
}
```

------------------------------------------------------------------------

## 6.3 ATTACH

``` json
{
  "type": "ATTACH",
  "session_id": "uuid",
  "attach_mode": "exclusive"
}
```

------------------------------------------------------------------------

## 6.4 RESUME

``` json
{
  "type": "RESUME",
  "session_id": "uuid",
  "resume_token": "base64-token"
}
```

------------------------------------------------------------------------

## 6.5 RESIZE

``` json
{
  "type": "RESIZE",
  "rows": 40,
  "cols": 120
}
```

------------------------------------------------------------------------

## 6.6 DETACH

``` json
{
  "type": "DETACH"
}
```

------------------------------------------------------------------------

## 6.7 CLOSE

``` json
{
  "type": "CLOSE"
}
```

------------------------------------------------------------------------

# 7. Session Semantics

Session states:

-   CREATED
-   ATTACHED
-   DETACHED
-   EXPIRED
-   TERMINATED

Default timeout: 600 seconds.

------------------------------------------------------------------------

# 8. Output Replay

Server MUST maintain a ring buffer.

On RESUME: - Server MAY replay recent output - Replay MUST occur before
live stream resumes

------------------------------------------------------------------------

# 9. Error Codes

-   AUTH_FAILED
-   SESSION_NOT_FOUND
-   SESSION_EXPIRED
-   ATTACH_DENIED
-   PROTOCOL_ERROR
-   INTERNAL_ERROR

------------------------------------------------------------------------

# 10. Capability Negotiation

v0.1.0 defines:

-   stdin-bytes
-   resume-v1

Future capabilities must be ignored if unsupported.

------------------------------------------------------------------------

# 11. Security Model

-   Initial authentication via SSH-issued token
-   Short-lived auth tokens (≤ 60s recommended)
-   Resume tokens must be revocable
-   TLS encryption mandatory
-   Server certificate must be validated (TOFU or CA)

------------------------------------------------------------------------

# End of Specification
