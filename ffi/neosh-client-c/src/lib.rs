use std::ffi::c_char;

const CLIENT_VERSION_CSTR: &[u8] = b"neosh/0.1.0\0";
const PROTOCOL_VERSION_CSTR: &[u8] = b"0.1.0\0";

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
