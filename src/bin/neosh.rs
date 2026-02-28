use std::env;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use neoshd::client::bootstrap::{BootstrapPayload, run_ssh_bootstrap};
use neoshd::client::cache::{SessionCache, SessionCacheEntry, now_epoch_seconds};
use neoshd::client::control::{encode_control_json, message_type};
use neoshd::client::quic_client::{QuicClientError, connect_and_verify};
use neoshd::CLIENT_VERSION;
use quinn::{RecvStream, SendStream};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::sync::Notify;
use uuid::Uuid;

const MAX_CONTROL_FRAME: usize = 64 * 1024;

#[derive(Debug, Parser)]
#[command(name = "neosh")]
#[command(about = "neosh client v0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Connect { target: String },
    Resume {
        #[arg(long)]
        session_id: Uuid,
        #[arg(long)]
        target: Option<String>,
    },
    Detach {
        #[arg(long)]
        session_id: Option<Uuid>,
    },
    Version,
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("bootstrap failed: {0}")]
    Bootstrap(String),
    #[error("quic failed: {0}")]
    Quic(String),
    #[error("protocol failed: {0}")]
    Protocol(String),
    #[error("cache failed: {0}")]
    Cache(String),
    #[error("io failed: {0}")]
    Io(String),
    #[error("auth failed")]
    AuthFailed,
    #[error("connection lost while attached")]
    Disconnected(Uuid),
}

#[derive(Debug, Deserialize)]
struct AuthOk {
    session_id: Uuid,
    resume_token: String,
    resume_token_expires_in_seconds: u64,
}

#[derive(Debug, Deserialize)]
struct ResumeOk {
    _session_id: Uuid,
    _replay_bytes: usize,
}

#[derive(Clone, Copy)]
enum BootstrapMode {
    New,
    Renew { session_id: Uuid },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Version => {
            println!("{}", CLIENT_VERSION);
            Ok(())
        }
        Commands::Connect { target } => connect_cmd(&target).await,
        Commands::Resume { session_id, target } => resume_cmd(session_id, target).await,
        Commands::Detach { session_id } => detach_cmd(session_id).await,
    };

    if let Err(err) = result {
        eprintln!("{}", json!({"error": err.to_string()}));
        std::process::exit(1);
    }
}

async fn connect_cmd(target: &str) -> Result<(), CliError> {
    let mut backoff = Duration::from_millis(100);
    for attempt in 0..2 {
        let payload = ssh_bootstrap_with_retry(target, BootstrapMode::New)?;
        match run_session(target, payload, SessionMode::Attach).await {
            Ok(()) => return Ok(()),
            Err(CliError::AuthFailed) if attempt == 0 => {
                tokio::time::sleep(backoff).await;
                backoff = backoff.saturating_mul(2);
                continue;
            }
            Err(CliError::Disconnected(session_id)) => {
                return resume_with_backoff(session_id, target.to_string()).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(CliError::AuthFailed)
}

async fn resume_cmd(session_id: Uuid, target_override: Option<String>) -> Result<(), CliError> {
    let cache = SessionCache::new(SessionCache::default_root());
    let cached = cache.get(session_id).map_err(|e| CliError::Cache(e.to_string()))?;
    if resume_entry_expired(&cached, now_epoch_seconds()) {
        let _ = cache.delete(session_id);
        return Err(CliError::Cache("resume token expired; start a new connect".to_string()));
    }

    let target = target_override.unwrap_or(cached.ssh_target.clone());
    if target.is_empty() {
        return Err(CliError::Cache(
            "missing ssh_target in cache; pass --target explicitly".to_string(),
        ));
    }
    resume_with_backoff(session_id, target).await
}

async fn resume_with_backoff(session_id: Uuid, target: String) -> Result<(), CliError> {
    let cache = SessionCache::new(SessionCache::default_root());
    let mut backoff = Duration::from_millis(100);
    for attempt in 0..3 {
        let cached = cache.get(session_id).map_err(|e| CliError::Cache(e.to_string()))?;
        if resume_entry_expired(&cached, now_epoch_seconds()) {
            let _ = cache.delete(session_id);
            return Err(CliError::Cache("resume token expired; start a new connect".to_string()));
        }
        let payload = ssh_bootstrap_with_retry(&target, BootstrapMode::Renew { session_id })?;
        validate_renew_payload(session_id, &payload)?;
        if payload.cert_fingerprint != cached.cert_fingerprint {
            return Err(CliError::Protocol(
                "cert_fingerprint changed for existing session_id".to_string(),
            ));
        }
        if payload.quic_addr != cached.quic_addr {
            let mut updated = cached.clone();
            updated.quic_addr = payload.quic_addr.clone();
            updated.updated_at = now_epoch_seconds();
            cache.put(&updated).map_err(|e| CliError::Cache(e.to_string()))?;
        }

        match run_session(
            &target,
            payload,
            SessionMode::Resume {
                session_id,
                resume_token: cached.resume_token.clone(),
            },
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(CliError::AuthFailed) | Err(CliError::Disconnected(_)) if attempt < 2 => {
                tokio::time::sleep(backoff).await;
                backoff = backoff.saturating_mul(2);
            }
            Err(e) => return Err(e),
        }
    }
    Err(CliError::Protocol(
        "resume attempts exhausted after connection/auth failures".to_string(),
    ))
}

async fn run_session(target: &str, payload: BootstrapPayload, mode: SessionMode) -> Result<(), CliError> {
    log_event("quic_connect_start", json!({"quic_addr": payload.quic_addr}));
    let (endpoint, conn) = connect_and_verify(&payload.quic_addr, &payload.cert_fingerprint)
        .await
        .map_err(|e| {
            if matches!(e, QuicClientError::FingerprintMismatch) {
                log_event("fingerprint_verify_fail", json!({"error": e.to_string()}));
            }
            log_event("quic_connect_fail", json!({"error": e.to_string()}));
            CliError::Quic(e.to_string())
        })?;
    log_event("quic_connect_ok", json!({}));
    log_event("fingerprint_verify_ok", json!({}));

    let (control_send, mut control_recv) = conn
        .open_bi()
        .await
        .map_err(|e| CliError::Protocol(format!("open control stream failed: {e}")))?;
    let control_send = std::sync::Arc::new(tokio::sync::Mutex::new(control_send));

    send_json(
        &control_send,
        &json!({
            "type":"HELLO",
            "protocol_version":"0.1.0",
            "client_version": CLIENT_VERSION,
            "capabilities":["stdin-bytes","resume-v1"]
        }),
    )
    .await?;
    expect_type(&mut control_recv, "HELLO_ACK").await?;

    send_json(
        &control_send,
        &json!({
            "type":"AUTH",
            "method":"ssh-token",
            "token": payload.auth_token,
        }),
    )
    .await?;

    let auth_value = read_json(&mut control_recv).await?;
    if message_type(&auth_value) == Some("ERROR") {
        log_event("auth_fail", json!({"frame": auth_value}));
        if is_error_code(&auth_value, "AUTH_FAILED") {
            return Err(CliError::AuthFailed);
        }
        return Err(CliError::Protocol(format!("AUTH failed: {auth_value}")));
    }
    if message_type(&auth_value) != Some("AUTH_OK") {
        log_event("auth_fail", json!({"frame": auth_value}));
        return Err(CliError::Protocol(format!("expected AUTH_OK, got {auth_value}")));
    }
    log_event("auth_ok", json!({}));
    let auth_ok: AuthOk =
        serde_json::from_value(auth_value).map_err(|e| CliError::Protocol(format!("bad AUTH_OK: {e}")))?;

    let cache = SessionCache::new(SessionCache::default_root());
    let cache_entry = SessionCacheEntry {
        session_id: auth_ok.session_id,
        ssh_target: target.to_string(),
        resume_token: auth_ok.resume_token.clone(),
        resume_token_expires_at: now_epoch_seconds() + auth_ok.resume_token_expires_in_seconds,
        quic_addr: payload.quic_addr.clone(),
        cert_fingerprint: payload.cert_fingerprint.clone(),
        updated_at: now_epoch_seconds(),
    };
    cache.put(&cache_entry).map_err(|e| CliError::Cache(e.to_string()))?;

    match mode {
        SessionMode::Attach => {
            send_json(
                &control_send,
                &json!({"type":"ATTACH","session_id":auth_ok.session_id,"attach_mode":"exclusive"}),
            )
            .await?;
            expect_type(&mut control_recv, "ATTACH_OK").await?;
            log_event("attach_ok", json!({"session_id": auth_ok.session_id}));
        }
        SessionMode::Resume {
            session_id,
            resume_token,
        } => {
            send_json(
                &control_send,
                &json!({"type":"RESUME","session_id":auth_ok.session_id,"resume_token":resume_token}),
            )
            .await?;
            let v = read_json(&mut control_recv).await?;
            if message_type(&v) == Some("ERROR") {
                if is_error_code(&v, "SESSION_EXPIRED") {
                    let _ = cache.delete(session_id);
                }
                log_event("resume_fail", json!({"frame": v}));
                return Err(CliError::Protocol(format!("RESUME failed: {v}")));
            }
            let _resume_ok: ResumeOk =
                serde_json::from_value(v).map_err(|e| CliError::Protocol(format!("bad RESUME_OK: {e}")))?;
            log_event("resume_ok", json!({"session_id": auth_ok.session_id}));
        }
    }

    let _raw_mode = RawModeGuard::activate();
    let notify_detach = std::sync::Arc::new(Notify::new());
    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<ControlSignal>();
    let ipc_path = ipc_socket_path(auth_ok.session_id);
    if let Some(parent) = ipc_path.parent() {
        fs::create_dir_all(parent).map_err(|e| CliError::Io(e.to_string()))?;
    }
    let _ = fs::remove_file(&ipc_path);

    let listener = UnixListener::bind(&ipc_path).map_err(|e| CliError::Io(e.to_string()))?;
    fs::set_permissions(&ipc_path, fs::Permissions::from_mode(0o600))
        .map_err(|e| CliError::Io(e.to_string()))?;

    let detach_signal = notify_detach.clone();
    let owner_uid = current_euid();
    let expected_session_id = auth_ok.session_id;
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let peer_uid_ok = peer_uid_matches_owner(&stream, owner_uid);
            if !peer_uid_ok {
                let _ = stream
                    .write_all(br#"{"ok":false,"error":"peer uid mismatch"}"#)
                    .await;
                continue;
            }
            let mut buf = Vec::new();
            if stream.read_to_end(&mut buf).await.is_err() {
                let _ = stream
                    .write_all(br#"{"ok":false,"error":"invalid request body"}"#)
                    .await;
                continue;
            }
            let req: Value = match serde_json::from_slice(&buf) {
                Ok(v) => v,
                Err(_) => {
                    let _ = stream
                        .write_all(br#"{"ok":false,"error":"invalid request json"}"#)
                        .await;
                    continue;
                }
            };
            if is_valid_detach_request(&req, expected_session_id) {
                let _ = stream.write_all(br#"{"ok":true}"#).await;
                detach_signal.notify_waiters();
                break;
            } else {
                let _ = stream
                    .write_all(br#"{"ok":false,"error":"invalid detach request"}"#)
                    .await;
            }
        }
    });

    let stdin_stream = conn
        .open_uni()
        .await
        .map_err(|e| CliError::Protocol(format!("open stdin stream failed: {e}")))?;
    let stdout_stream = conn
        .accept_uni()
        .await
        .map_err(|e| CliError::Protocol(format!("accept stdout stream failed: {e}")))?;

    let bridge_task = tokio::spawn(bridge_terminal(
        stdin_stream,
        stdout_stream,
        notify_detach.clone(),
        control_tx.clone(),
    ));
    let resize_task = tokio::spawn(resize_watch_task(control_send.clone(), notify_detach.clone()));
    let keepalive_task = tokio::spawn(keepalive_task(control_send.clone(), notify_detach.clone()));
    let control_watch_task = tokio::spawn(control_watch_loop(control_recv, control_tx));

    let mut should_send_detach = true;
    let mut disconnected = false;
    tokio::select! {
        _ = notify_detach.notified() => {}
        Some(signal) = control_rx.recv() => {
            match signal {
                ControlSignal::Close => {
                    should_send_detach = false;
                    log_event("close_received", json!({"session_id": auth_ok.session_id}));
                    let _ = cache.delete(auth_ok.session_id);
                }
                ControlSignal::Error(value) => {
                    let _ = bridge_task.abort();
                    let _ = resize_task.abort();
                    let _ = control_watch_task.abort();
                    let _ = fs::remove_file(&ipc_path);
                    endpoint.close(0u32.into(), b"client close");
                    return Err(CliError::Protocol(format!("control error: {value}")));
                }
                ControlSignal::Disconnected => {
                    should_send_detach = false;
                    disconnected = true;
                }
            }
        }
    }
    if should_send_detach {
        log_event("detach_sent", json!({"session_id": auth_ok.session_id}));
        send_json(&control_send, &json!({"type":"DETACH"})).await?;
    }

    let _ = bridge_task.abort();
    let _ = resize_task.abort();
    let _ = keepalive_task.abort();
    let _ = control_watch_task.abort();
    let _ = fs::remove_file(&ipc_path);

    endpoint.close(0u32.into(), b"client close");
    if disconnected {
        return Err(CliError::Disconnected(auth_ok.session_id));
    }
    Ok(())
}

async fn bridge_terminal(
    mut stdin_stream: quinn::SendStream,
    mut stdout_stream: quinn::RecvStream,
    notify_detach: std::sync::Arc<Notify>,
    control_tx: mpsc::UnboundedSender<ControlSignal>,
) {
    let control_tx_write = control_tx.clone();
    let write_task = tokio::spawn(async move {
        let mut input = tokio::io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            let n = match input.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };

            if n == 2 && buf[0] == 0x1d && (buf[1] == b'd' || buf[1] == b'D') {
                notify_detach.notify_waiters();
                break;
            }

            if stdin_stream.write_all(&buf[..n]).await.is_err() {
                let _ = control_tx_write.send(ControlSignal::Disconnected);
                break;
            }
        }
        let _ = stdin_stream.finish();
    });

    let read_task = tokio::spawn(async move {
        let mut output = tokio::io::stdout();
        let mut buf = [0u8; 1024];
        loop {
            match stdout_stream.read(&mut buf).await {
                Ok(Some(n)) => {
                    if output.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    let _ = output.flush().await;
                }
                Ok(None) => {
                    let _ = control_tx.send(ControlSignal::Disconnected);
                    break;
                }
                Err(_) => {
                    let _ = control_tx.send(ControlSignal::Disconnected);
                    break;
                }
            }
        }
    });

    let _ = tokio::join!(write_task, read_task);
}

async fn detach_cmd(session_id: Option<Uuid>) -> Result<(), CliError> {
    let path = match session_id {
        Some(sid) => ipc_socket_path(sid),
        None => discover_active_socket()?,
    };
    let resolved_session_id = session_id.unwrap_or(session_id_from_socket_path(&path)?);

    let mut stream = UnixStream::connect(&path)
        .await
        .map_err(|e| CliError::Io(format!("connect {} failed: {e}", path.display())))?;

    let req = json!({"type":"DETACH","session_id":resolved_session_id});
    stream
        .write_all(req.to_string().as_bytes())
        .await
        .map_err(|e| CliError::Io(e.to_string()))?;
    stream
        .shutdown()
        .await
        .map_err(|e| CliError::Io(e.to_string()))?;

    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .await
        .map_err(|e| CliError::Io(e.to_string()))?;
    let value: Value =
        serde_json::from_slice(&buf).map_err(|e| CliError::Protocol(e.to_string()))?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return Err(CliError::Protocol(format!("detach failed: {value}")));
    }

    println!("{}", json!({"ok":true}));
    Ok(())
}

async fn send_json(
    send: &std::sync::Arc<tokio::sync::Mutex<SendStream>>,
    value: &Value,
) -> Result<(), CliError> {
    let frame = encode_control_json(value).map_err(|e| CliError::Protocol(e.to_string()))?;
    let mut guard = send.lock().await;
    guard
        .write_all(&frame)
        .await
        .map_err(|e| CliError::Protocol(format!("write control failed: {e}")))
}

async fn read_json(recv: &mut RecvStream) -> Result<Value, CliError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| CliError::Protocol(format!("read control len failed: {e}")))?;
    let payload_len = u32::from_be_bytes(len_buf) as usize;
    if payload_len > MAX_CONTROL_FRAME {
        return Err(CliError::Protocol("control frame too large".to_string()));
    }
    let mut payload = vec![0u8; payload_len];
    recv.read_exact(&mut payload)
        .await
        .map_err(|e| CliError::Protocol(format!("read control payload failed: {e}")))?;

    serde_json::from_slice(&payload).map_err(|e| CliError::Protocol(format!("invalid json: {e}")))
}

async fn expect_type(recv: &mut RecvStream, expected: &str) -> Result<Value, CliError> {
    let value = read_json(recv).await?;
    if message_type(&value) != Some(expected) {
        return Err(CliError::Protocol(format!("expected {expected}, got {value}")));
    }
    Ok(value)
}

fn ipc_socket_path(session_id: Uuid) -> PathBuf {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir)
        .join("neosh")
        .join(format!("{}.sock", session_id))
}

enum SessionMode {
    Attach,
    Resume { session_id: Uuid, resume_token: String },
}

fn log_event(event: &str, payload: Value) {
    eprintln!("{}", json!({"event": event, "payload": payload}));
}

fn ssh_bootstrap_with_retry(target: &str, mode: BootstrapMode) -> Result<BootstrapPayload, CliError> {
    let mut last_err: Option<String> = None;
    for _ in 0..2 {
        let cmd = match mode {
            BootstrapMode::New => "neoshd new --user \"$USER\"".to_string(),
            BootstrapMode::Renew { session_id } => {
                format!("neoshd renew-auth --session-id {} --user \"$USER\"", session_id)
            }
        };
        log_event("bootstrap_start", json!({"target":target,"cmd":cmd}));
        match run_ssh_bootstrap(target, &cmd) {
            Ok(p) => {
                log_event("bootstrap_ok", json!({"session_id":p.session_id}));
                return Ok(p);
            }
            Err(e) => {
                log_event("bootstrap_fail", json!({"error":e.to_string()}));
                last_err = Some(e.to_string())
            }
        }
    }
    Err(CliError::Bootstrap(
        last_err.unwrap_or_else(|| "unknown bootstrap failure".to_string()),
    ))
}

fn resume_entry_expired(entry: &SessionCacheEntry, now: u64) -> bool {
    entry.resume_token_expires_at <= now
}

fn validate_renew_payload(expected_session_id: Uuid, payload: &BootstrapPayload) -> Result<(), CliError> {
    if payload.session_id != expected_session_id {
        return Err(CliError::Protocol(
            "renew-auth returned mismatched session_id".to_string(),
        ));
    }
    Ok(())
}

fn is_error_code(value: &Value, code: &str) -> bool {
    value.get("code").and_then(|v| v.as_str()) == Some(code)
}

fn is_valid_detach_request(value: &Value, expected_session_id: Uuid) -> bool {
    value.get("type").and_then(|v| v.as_str()) == Some("DETACH")
        && value.get("session_id").and_then(|v| v.as_str()) == Some(expected_session_id.to_string().as_str())
}

fn session_id_from_socket_path(path: &PathBuf) -> Result<Uuid, CliError> {
    let stem = path
        .file_stem()
        .and_then(|v| v.to_str())
        .ok_or_else(|| CliError::Io(format!("invalid session socket name: {}", path.display())))?;
    Uuid::parse_str(stem).map_err(|e| CliError::Io(format!("invalid session socket id: {e}")))
}

fn current_euid() -> u32 {
    #[cfg(unix)]
    {
        return unsafe { libc::geteuid() };
    }
    #[allow(unreachable_code)]
    0
}

fn peer_uid_matches_owner(stream: &UnixStream, owner_uid: u32) -> bool {
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"))]
    {
        let fd = stream.as_raw_fd();
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        return rc == 0 && uid == owner_uid;
    }
    #[cfg(target_os = "linux")]
    {
        let fd = stream.as_raw_fd();
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        return rc == 0 && cred.uid == owner_uid;
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    {
        let _ = stream;
        let _ = owner_uid;
        true
    }
}

enum ControlSignal {
    Close,
    Error(Value),
    Disconnected,
}

async fn control_watch_loop(mut control_recv: RecvStream, control_tx: mpsc::UnboundedSender<ControlSignal>) {
    while let Ok(frame) = read_json(&mut control_recv).await {
        match message_type(&frame) {
            Some("CLOSE") => {
                let _ = control_tx.send(ControlSignal::Close);
                break;
            }
            Some("ERROR") => {
                let _ = control_tx.send(ControlSignal::Error(frame));
                break;
            }
            Some("PONG") => {}
            Some(_) | None => {
                let _ = control_tx.send(ControlSignal::Error(json!({
                    "type":"ERROR",
                    "code":"PROTOCOL_ERROR",
                    "message":"unknown control message while attached",
                    "raw": frame
                })));
                break;
            }
        }
    }
}

async fn keepalive_task(
    control_send: std::sync::Arc<tokio::sync::Mutex<SendStream>>,
    notify_detach: std::sync::Arc<Notify>,
) {
    loop {
        tokio::select! {
            _ = notify_detach.notified() => break,
            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                let nonce = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis().to_string())
                    .unwrap_or_else(|_| "0".to_string());
                let _ = send_json(&control_send, &json!({"type":"PING","nonce":nonce})).await;
            }
        }
    }
}

fn discover_active_socket() -> Result<PathBuf, CliError> {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let dir = PathBuf::from(runtime_dir).join("neosh");
    let entries = fs::read_dir(&dir)
        .map_err(|e| CliError::Io(format!("read {} failed: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|v| v.to_str()) == Some("sock") {
            return Ok(path);
        }
    }
    Err(CliError::Io("no active session socket found".to_string()))
}

async fn resize_watch_task(
    control_send: std::sync::Arc<tokio::sync::Mutex<SendStream>>,
    notify_detach: std::sync::Arc<Notify>,
) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut sig) = signal(SignalKind::window_change()) {
            loop {
                tokio::select! {
                    _ = notify_detach.notified() => break,
                    _ = sig.recv() => {
                        if let Some((rows, cols)) = read_tty_size() {
                            let _ = send_json(&control_send, &json!({"type":"RESIZE","rows":rows,"cols":cols})).await;
                        }
                    }
                }
            }
        }
    }
}

fn read_tty_size() -> Option<(u16, u16)> {
    #[cfg(unix)]
    {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
        if rc == 0 && ws.ws_row > 0 && ws.ws_col > 0 {
            return Some((ws.ws_row, ws.ws_col));
        }
    }
    None
}

struct RawModeGuard {
    fd: i32,
    orig: Option<libc::termios>,
}

impl RawModeGuard {
    fn activate() -> Self {
        #[cfg(unix)]
        {
            let fd = libc::STDIN_FILENO;
            let is_tty = unsafe { libc::isatty(fd) } == 1;
            if !is_tty {
                return Self { fd, orig: None };
            }
            let mut term = unsafe { std::mem::zeroed::<libc::termios>() };
            let rc_get = unsafe { libc::tcgetattr(fd, &mut term) };
            if rc_get != 0 {
                return Self { fd, orig: None };
            }
            let orig = term;
            unsafe { libc::cfmakeraw(&mut term) };
            let rc_set = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
            if rc_set != 0 {
                return Self { fd, orig: None };
            }
            Self { fd, orig: Some(orig) }
        }
        #[cfg(not(unix))]
        {
            Self { fd: 0, orig: None }
        }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if let Some(orig) = self.orig.take() {
            let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &orig) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_socket_path_uses_neosh_namespace() {
        let sid = Uuid::new_v4();
        let path = ipc_socket_path(sid);
        let text = path.to_string_lossy();
        assert!(text.contains("/neosh/"));
        assert!(text.ends_with(&format!("{}.sock", sid)));
    }

    #[test]
    fn parse_auth_ok() {
        let raw = r#"{"session_id":"550e8400-e29b-41d4-a716-446655440000","resume_token":"r","resume_token_expires_in_seconds":60}"#;
        let parsed: AuthOk = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.resume_token, "r");
    }

    #[test]
    fn resume_mode_generates_renew_command() {
        let mode = BootstrapMode::Renew {
            session_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
        };
        let cmd = match mode {
            BootstrapMode::New => String::new(),
            BootstrapMode::Renew { session_id } => {
                format!("neoshd renew-auth --session-id {} --user \"$USER\"", session_id)
            }
        };
        assert!(cmd.contains("renew-auth"));
    }

    #[test]
    fn detect_expired_resume_entry() {
        let now = 1_700_000_100_u64;
        let entry = SessionCacheEntry {
            session_id: Uuid::new_v4(),
            ssh_target: "user@example.com".to_string(),
            resume_token: "r".to_string(),
            resume_token_expires_at: 1_700_000_000,
            quic_addr: "127.0.0.1:30001".to_string(),
            cert_fingerprint:
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            updated_at: 1_700_000_000,
        };
        assert!(resume_entry_expired(&entry, now));
    }

    #[test]
    fn renew_payload_session_id_must_match() {
        let expected = Uuid::new_v4();
        let payload = BootstrapPayload {
            session_id: Uuid::new_v4(),
            auth_token: "t".to_string(),
            auth_token_expires_in_seconds: 60,
            quic_addr: "127.0.0.1:30001".to_string(),
            cert_fingerprint:
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
        };
        assert!(validate_renew_payload(expected, &payload).is_err());
    }

    #[test]
    fn detach_request_must_include_matching_session_id() {
        let sid = Uuid::new_v4();
        let ok = json!({"type":"DETACH","session_id":sid});
        let bad = json!({"type":"DETACH","session_id":Uuid::new_v4()});
        assert!(is_valid_detach_request(&ok, sid));
        assert!(!is_valid_detach_request(&bad, sid));
    }

    #[test]
    fn parse_session_id_from_socket_path() {
        let sid = Uuid::new_v4();
        let path = PathBuf::from(format!("/tmp/neosh/{}.sock", sid));
        let parsed = session_id_from_socket_path(&path).unwrap();
        assert_eq!(parsed, sid);
    }
}
