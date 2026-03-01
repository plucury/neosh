#ifndef NEOSH_CLIENT_H
#define NEOSH_CLIENT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Opaque client handle.
typedef struct neosh_client_t neosh_client_t;

// Return codes.
// 0: success
// -1: invalid argument
// -2: output buffer too small
// -3: stream not ready / wrong state
// -4: internal or transport error
#define NEOSH_CLIENT_OK 0
#define NEOSH_CLIENT_ERR_INVALID_ARG -1
#define NEOSH_CLIENT_ERR_BUFFER_TOO_SMALL -2
#define NEOSH_CLIENT_ERR_NOT_READY -3
#define NEOSH_CLIENT_ERR_INTERNAL -4

// Returns UTF-8 C string "neosh/0.1.0".
const char *neosh_client_version(void);

// Returns UTF-8 C string protocol version "0.1.0".
const char *neosh_client_protocol_version(void);

// Returns core version and validates linkage against Rust core constant.
const char *neosh_client_version_from_core(void);

// Connects QUIC and opens the control bidirectional stream.
// Returns null on failure; call neosh_client_last_error_global() for details.
neosh_client_t *neosh_client_connect(const char *quic_addr, const char *expected_fingerprint);

// Closes transport and frees client handle. Safe with null.
void neosh_client_free(neosh_client_t *client);

// Returns last error (UTF-8) associated with this client handle.
// Returned pointer is owned by the client; do not free.
const char *neosh_client_last_error(const neosh_client_t *client);

// Returns last global error (UTF-8), used when connect returns null.
// Returned pointer is static; do not free.
const char *neosh_client_last_error_global(void);

// Sends one control JSON payload (raw JSON bytes, framing is added internally).
int32_t neosh_client_send_control_json(
    neosh_client_t *client,
    const uint8_t *json_bytes,
    size_t json_len
);

// Receives one control JSON payload (without frame header).
// If out_cap is insufficient, returns BUFFER_TOO_SMALL and writes required size to out_len.
int32_t neosh_client_recv_control_json(
    neosh_client_t *client,
    uint8_t *out_buf,
    size_t out_cap,
    size_t *out_len
);

// Opens data streams:
// - one unidirectional client->server stream for stdin
// - one accepted unidirectional server->client stream for stdout
int32_t neosh_client_open_data_streams(neosh_client_t *client);

// Sends bytes to remote stdin stream.
int32_t neosh_client_send_stdin(
    neosh_client_t *client,
    const uint8_t *data,
    size_t len,
    size_t *written_len
);

// Finishes stdin stream.
int32_t neosh_client_finish_stdin(neosh_client_t *client);

// Receives bytes from remote stdout stream.
// On EOF: out_len=0 and eof=1.
int32_t neosh_client_recv_stdout(
    neosh_client_t *client,
    uint8_t *out_buf,
    size_t out_cap,
    size_t *out_len,
    int32_t *eof
);

#ifdef __cplusplus
}
#endif

#endif  // NEOSH_CLIENT_H
