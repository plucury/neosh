use std::ffi::{CStr, CString, c_char};
use std::os::raw::c_int;
use std::ptr;
use std::slice;
use std::sync::{Mutex, OnceLock};

use neoshd::client::quic_client::connect_and_verify;
use neoshd::protocol::framing::encode_frame;
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use tokio::runtime::Runtime;
use tokio::time::{Duration, timeout};

const CLIENT_VERSION_CSTR: &[u8] = b"neosh/0.1.0\0";
const PROTOCOL_VERSION_CSTR: &[u8] = b"0.1.0\0";
const MAX_CONTROL_FRAME: usize = 64 * 1024;
const DEFAULT_QUIC_IDLE_TIMEOUT_SECS: u64 = 60;

const NEOSH_CLIENT_OK: c_int = 0;
const NEOSH_CLIENT_ERR_INVALID_ARG: c_int = -1;
const NEOSH_CLIENT_ERR_BUFFER_TOO_SMALL: c_int = -2;
const NEOSH_CLIENT_ERR_NOT_READY: c_int = -3;
const NEOSH_CLIENT_ERR_INTERNAL: c_int = -4;

#[repr(C)]
pub struct neosh_client_t {
    _private: [u8; 0],
}

struct NeoshClient {
    endpoint: Endpoint,
    connection: Connection,
    control_send: SendStream,
    control_recv: RecvStream,
    stdin_send: Option<SendStream>,
    stdout_recv: Option<RecvStream>,
    last_error: CString,
}

fn cstring_lossy(input: &str) -> CString {
    let mut bytes = input.as_bytes().to_vec();
    for b in &mut bytes {
        if *b == 0 {
            *b = b'?';
        }
    }
    CString::new(bytes).unwrap_or_else(|_| CString::new("unknown error").expect("valid literal"))
}

fn global_last_error() -> &'static Mutex<CString> {
    static LAST: OnceLock<Mutex<CString>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(CString::new("ok").expect("valid literal")))
}

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().expect("failed to init tokio runtime"))
}

fn ensure_rustls_crypto_provider() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    let _ = INSTALLED.get_or_init(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn set_global_error(msg: &str) {
    if let Ok(mut guard) = global_last_error().lock() {
        *guard = cstring_lossy(msg);
    }
}

fn set_client_error(client: &mut NeoshClient, msg: &str, code: c_int) -> c_int {
    client.last_error = cstring_lossy(msg);
    code
}

unsafe fn client_mut<'a>(ptr_client: *mut neosh_client_t) -> Option<&'a mut NeoshClient> {
    if ptr_client.is_null() {
        return None;
    }
    let raw = ptr_client.cast::<NeoshClient>();
    // SAFETY: caller guarantees ptr_client came from neosh_client_connect.
    Some(unsafe { &mut *raw })
}

unsafe fn client_ref<'a>(ptr_client: *const neosh_client_t) -> Option<&'a NeoshClient> {
    if ptr_client.is_null() {
        return None;
    }
    let raw = ptr_client.cast::<NeoshClient>();
    // SAFETY: caller guarantees ptr_client came from neosh_client_connect.
    Some(unsafe { &*raw })
}

fn cstr_to_string(arg: *const c_char, name: &str) -> Result<String, String> {
    if arg.is_null() {
        return Err(format!("{name} is null"));
    }
    // SAFETY: checked for null above and pointer must be valid C string.
    let c = unsafe { CStr::from_ptr(arg) };
    let s = c
        .to_str()
        .map_err(|_| format!("{name} is not valid UTF-8"))?
        .trim()
        .to_string();
    if s.is_empty() {
        return Err(format!("{name} is empty"));
    }
    Ok(s)
}

fn read_control_payload(recv: &mut RecvStream) -> Result<Vec<u8>, String> {
    runtime().block_on(async {
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| format!("read control length failed: {e}"))?;
        let payload_len = u32::from_be_bytes(len_buf) as usize;
        if payload_len > MAX_CONTROL_FRAME {
            return Err(format!("control frame too large: {payload_len}"));
        }
        let mut payload = vec![0u8; payload_len];
        recv.read_exact(&mut payload)
            .await
            .map_err(|e| format!("read control payload failed: {e}"))?;
        Ok(payload)
    })
}

fn read_control_payload_with_timeout(
    recv: &mut RecvStream,
    timeout_duration: Duration,
) -> Result<Option<Vec<u8>>, String> {
    runtime().block_on(async {
        let mut len_buf = [0u8; 4];
        let first = timeout(timeout_duration, recv.read_exact(&mut len_buf)).await;
        match first {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("read control length failed: {e}")),
            Err(_) => return Ok(None),
        }

        let payload_len = u32::from_be_bytes(len_buf) as usize;
        if payload_len > MAX_CONTROL_FRAME {
            return Err(format!("control frame too large: {payload_len}"));
        }
        let mut payload = vec![0u8; payload_len];
        recv.read_exact(&mut payload)
            .await
            .map_err(|e| format!("read control payload failed: {e}"))?;
        Ok(Some(payload))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_version() -> *const c_char {
    CLIENT_VERSION_CSTR.as_ptr().cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_protocol_version() -> *const c_char {
    PROTOCOL_VERSION_CSTR.as_ptr().cast()
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_version_from_core() -> *const c_char {
    if neoshd::CLIENT_VERSION == "neosh/0.1.0" {
        CLIENT_VERSION_CSTR.as_ptr().cast()
    } else {
        b"mismatch\0".as_ptr().cast()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_connect(
    quic_addr: *const c_char,
    expected_fingerprint: *const c_char,
) -> *mut neosh_client_t {
    ensure_rustls_crypto_provider();

    let quic_addr = match cstr_to_string(quic_addr, "quic_addr") {
        Ok(v) => v,
        Err(e) => {
            set_global_error(&e);
            return ptr::null_mut();
        }
    };
    let expected_fingerprint = match cstr_to_string(expected_fingerprint, "expected_fingerprint") {
        Ok(v) => v,
        Err(e) => {
            set_global_error(&e);
            return ptr::null_mut();
        }
    };

    let connected = runtime().block_on(async {
        let (endpoint, conn) =
            connect_and_verify(&quic_addr, &expected_fingerprint, DEFAULT_QUIC_IDLE_TIMEOUT_SECS)
            .await
            .map_err(|e| format!("connect failed: {e}"))?;
        let (control_send, control_recv) = conn
            .open_bi()
            .await
            .map_err(|e| format!("open control stream failed: {e}"))?;
        Ok::<NeoshClient, String>(NeoshClient {
            endpoint,
            connection: conn,
            control_send,
            control_recv,
            stdin_send: None,
            stdout_recv: None,
            last_error: CString::new("ok").expect("valid literal"),
        })
    });

    match connected {
        Ok(client) => Box::into_raw(Box::new(client)).cast::<neosh_client_t>(),
        Err(e) => {
            set_global_error(&e);
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_free(client: *mut neosh_client_t) {
    if client.is_null() {
        return;
    }
    // SAFETY: client comes from Box::into_raw in neosh_client_connect.
    let owned = unsafe { Box::from_raw(client.cast::<NeoshClient>()) };
    owned.connection.close(0u32.into(), b"client close");
    owned.endpoint.close(0u32.into(), b"client close");
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_last_error(client: *const neosh_client_t) -> *const c_char {
    // SAFETY: pointer is treated as read-only and may be null.
    let Some(client) = (unsafe { client_ref(client) }) else {
        return b"null client\0".as_ptr().cast();
    };
    client.last_error.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_last_error_global() -> *const c_char {
    match global_last_error().lock() {
        Ok(guard) => guard.as_ptr(),
        Err(_) => b"global error lock poisoned\0".as_ptr().cast(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_send_control_json(
    client: *mut neosh_client_t,
    json_bytes: *const u8,
    json_len: usize,
) -> c_int {
    // SAFETY: pointer is validated before deref.
    let Some(client) = (unsafe { client_mut(client) }) else {
        return NEOSH_CLIENT_ERR_INVALID_ARG;
    };
    if json_bytes.is_null() {
        return set_client_error(client, "json_bytes is null", NEOSH_CLIENT_ERR_INVALID_ARG);
    }
    // SAFETY: caller provides a valid buffer for json_len bytes.
    let payload = unsafe { slice::from_raw_parts(json_bytes, json_len) };
    let frame = encode_frame(payload);
    match runtime().block_on(async { client.control_send.write_all(&frame).await }) {
        Ok(()) => NEOSH_CLIENT_OK,
        Err(e) => set_client_error(
            client,
            &format!("write control json failed: {e}"),
            NEOSH_CLIENT_ERR_INTERNAL,
        ),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_recv_control_json(
    client: *mut neosh_client_t,
    out_buf: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
) -> c_int {
    // SAFETY: pointer is validated before deref.
    let Some(client) = (unsafe { client_mut(client) }) else {
        return NEOSH_CLIENT_ERR_INVALID_ARG;
    };
    if out_len.is_null() {
        return set_client_error(client, "out_len is null", NEOSH_CLIENT_ERR_INVALID_ARG);
    }
    let payload = match read_control_payload_with_timeout(&mut client.control_recv, Duration::from_millis(20)) {
        Ok(Some(v)) => v,
        Ok(None) => {
            unsafe {
                *out_len = 0;
            }
            return NEOSH_CLIENT_ERR_NOT_READY;
        }
        Err(e) => return set_client_error(client, &e, NEOSH_CLIENT_ERR_INTERNAL),
    };
    // SAFETY: out_len is non-null and writable by contract.
    unsafe {
        *out_len = payload.len();
    }

    if out_buf.is_null() && !payload.is_empty() {
        return set_client_error(client, "out_buf is null", NEOSH_CLIENT_ERR_INVALID_ARG);
    }
    if payload.len() > out_cap {
        return set_client_error(
            client,
            &format!("buffer too small, need {}", payload.len()),
            NEOSH_CLIENT_ERR_BUFFER_TOO_SMALL,
        );
    }
    if payload.is_empty() {
        return NEOSH_CLIENT_OK;
    }
    // SAFETY: out_buf is non-null and has enough capacity (checked above).
    unsafe {
        ptr::copy_nonoverlapping(payload.as_ptr(), out_buf, payload.len());
    }
    NEOSH_CLIENT_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_open_data_streams(client: *mut neosh_client_t) -> c_int {
    // SAFETY: pointer is validated before deref.
    let Some(client) = (unsafe { client_mut(client) }) else {
        return NEOSH_CLIENT_ERR_INVALID_ARG;
    };

    let opened = runtime().block_on(async {
        let stdin_stream = client
            .connection
            .open_uni()
            .await
            .map_err(|e| format!("open stdin stream failed: {e}"))?;
        let stdout_stream = client
            .connection
            .accept_uni()
            .await
            .map_err(|e| format!("accept stdout stream failed: {e}"))?;
        Ok::<(SendStream, RecvStream), String>((stdin_stream, stdout_stream))
    });

    match opened {
        Ok((stdin_send, stdout_recv)) => {
            client.stdin_send = Some(stdin_send);
            client.stdout_recv = Some(stdout_recv);
            NEOSH_CLIENT_OK
        }
        Err(e) => set_client_error(client, &e, NEOSH_CLIENT_ERR_INTERNAL),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_send_stdin(
    client: *mut neosh_client_t,
    data: *const u8,
    len: usize,
    written_len: *mut usize,
) -> c_int {
    // SAFETY: pointer is validated before deref.
    let Some(client) = (unsafe { client_mut(client) }) else {
        return NEOSH_CLIENT_ERR_INVALID_ARG;
    };
    if written_len.is_null() {
        return set_client_error(client, "written_len is null", NEOSH_CLIENT_ERR_INVALID_ARG);
    }
    if data.is_null() && len > 0 {
        return set_client_error(client, "data is null", NEOSH_CLIENT_ERR_INVALID_ARG);
    }
    let Some(stdin_send) = client.stdin_send.as_mut() else {
        return set_client_error(
            client,
            "stdin stream not ready, call neosh_client_open_data_streams first",
            NEOSH_CLIENT_ERR_NOT_READY,
        );
    };

    // SAFETY: caller provides a valid buffer for len bytes if len > 0.
    let input = unsafe { slice::from_raw_parts(data, len) };
    let result = runtime().block_on(async { stdin_send.write_all(input).await });
    match result {
        Ok(()) => {
            // SAFETY: written_len is non-null and writable by contract.
            unsafe {
                *written_len = len;
            }
            NEOSH_CLIENT_OK
        }
        Err(e) => set_client_error(
            client,
            &format!("write stdin failed: {e}"),
            NEOSH_CLIENT_ERR_INTERNAL,
        ),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_finish_stdin(client: *mut neosh_client_t) -> c_int {
    // SAFETY: pointer is validated before deref.
    let Some(client) = (unsafe { client_mut(client) }) else {
        return NEOSH_CLIENT_ERR_INVALID_ARG;
    };
    let Some(stdin_send) = client.stdin_send.as_mut() else {
        return set_client_error(
            client,
            "stdin stream not ready, call neosh_client_open_data_streams first",
            NEOSH_CLIENT_ERR_NOT_READY,
        );
    };

    match runtime().block_on(async { stdin_send.finish() }) {
        Ok(()) => NEOSH_CLIENT_OK,
        Err(e) => set_client_error(
            client,
            &format!("finish stdin failed: {e}"),
            NEOSH_CLIENT_ERR_INTERNAL,
        ),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn neosh_client_recv_stdout(
    client: *mut neosh_client_t,
    out_buf: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
    eof: *mut i32,
) -> c_int {
    // SAFETY: pointer is validated before deref.
    let Some(client) = (unsafe { client_mut(client) }) else {
        return NEOSH_CLIENT_ERR_INVALID_ARG;
    };
    if out_len.is_null() || eof.is_null() {
        return set_client_error(
            client,
            "out_len or eof is null",
            NEOSH_CLIENT_ERR_INVALID_ARG,
        );
    }
    if out_cap == 0 {
        return set_client_error(client, "out_cap is zero", NEOSH_CLIENT_ERR_INVALID_ARG);
    }
    if out_buf.is_null() {
        return set_client_error(client, "out_buf is null", NEOSH_CLIENT_ERR_INVALID_ARG);
    }
    let Some(stdout_recv) = client.stdout_recv.as_mut() else {
        return set_client_error(
            client,
            "stdout stream not ready, call neosh_client_open_data_streams first",
            NEOSH_CLIENT_ERR_NOT_READY,
        );
    };
    // SAFETY: out_buf is non-null for out_cap bytes.
    let out = unsafe { slice::from_raw_parts_mut(out_buf, out_cap) };

    match runtime().block_on(async { timeout(Duration::from_millis(20), stdout_recv.read(out)).await }) {
        Ok(Ok(Some(n))) => {
            // SAFETY: out_len/eof are validated non-null above.
            unsafe {
                *out_len = n;
                *eof = 0;
            }
            NEOSH_CLIENT_OK
        }
        Ok(Ok(None)) => {
            // SAFETY: out_len/eof are validated non-null above.
            unsafe {
                *out_len = 0;
                *eof = 1;
            }
            NEOSH_CLIENT_OK
        }
        Ok(Err(e)) => set_client_error(
            client,
            &format!("read stdout failed: {e}"),
            NEOSH_CLIENT_ERR_INTERNAL,
        ),
        Err(_) => {
            unsafe {
                *out_len = 0;
                *eof = 0;
            }
            NEOSH_CLIENT_ERR_NOT_READY
        }
    }
}
