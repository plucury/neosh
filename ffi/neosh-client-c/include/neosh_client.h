#ifndef NEOSH_CLIENT_H
#define NEOSH_CLIENT_H

#ifdef __cplusplus
extern "C" {
#endif

// Returns UTF-8 C string "neosh/0.1.0".
const char *neosh_client_version(void);

// Returns UTF-8 C string protocol version "0.1.0".
const char *neosh_client_protocol_version(void);

// Returns core version and validates linkage against Rust core constant.
const char *neosh_client_version_from_core(void);

#ifdef __cplusplus
}
#endif

#endif  // NEOSH_CLIENT_H
