use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::Utc;
use clap::{Parser, Subcommand};
use neoshd::protocol::dispatcher::Dispatcher;
use neoshd::protocol::framing::{decode_frame, encode_frame};
use neoshd::protocol::messages::{ErrorMessage, MessageKind, parse_message_kind};
use neoshd::session::manager::{SessionError, SessionManager, SessionState};
use neoshd::terminal::pty::LivePty;
use neoshd::token::service::{TokenError, TokenService};
use neoshd::SERVER_VERSION;
use quinn::{Connection, Endpoint, ServerConfig, VarInt};
use quinn::crypto::rustls::QuicServerConfig;
use rcgen::generate_simple_self_signed;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

const ALPN: &[u8] = b"neosh/1";
const MAX_CONTROL_FRAME: usize = 64 * 1024;

#[derive(Debug, Parser)]
#[command(name = "neoshd")]
#[command(about = "Start a remote shell backend and issue bootstrap/renew tokens.")]
#[command(after_help = "Examples:\n  neoshd new --user \"$USER\"\n  neoshd new --user \"$USER\" --bind-server 0.0.0.0 --port-range 30000:39999\n  neoshd renew-auth --session-id 550e8400-e29b-41d4-a716-446655440000 --user \"$USER\"\n  neoshd version")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Create or reuse a session and print bootstrap JSON.")]
    New(NewArgs),
    #[command(about = "Issue a fresh single-use auth token for an existing session.")]
    #[command(name = "renew-auth")]
    #[command(after_help = "Example:\n  neoshd renew-auth --session-id 550e8400-e29b-41d4-a716-446655440000 --user \"$USER\"")]
    RenewAuth {
        #[arg(long, help = "Session ID to renew auth token for")]
        session_id: Uuid,
        #[arg(long, help = "Session owner user name")]
        user: String,
    },
    #[command(about = "Print server version.")]
    Version,
}

#[derive(Debug, Parser, Clone)]
struct NewArgs {
    #[arg(long, help = "Session owner user name")]
    user: String,
    #[arg(long, default_value = "30000:39999", help = "UDP port range, format start:end")]
    port_range: String,
    #[arg(long, default_value = "ssh", help = "Host/IP advertised to clients in quic_addr")]
    bind_server: String,
    #[arg(long, default_value = "", help = "Path to TLS certificate PEM (optional)")]
    tls_cert: String,
    #[arg(long, default_value = "", help = "Path to TLS private key PEM (optional)")]
    tls_key: String,
    #[arg(long, default_value_t = 600, help = "Detached session timeout in seconds")]
    session_timeout: u64,
    #[arg(long, default_value_t = 60, help = "Auth token TTL in seconds")]
    auth_token_ttl: u64,
    #[arg(long, default_value_t = 86400, help = "Resume token TTL in seconds")]
    resume_token_ttl: u64,
    #[arg(long, default_value_t = 1_048_576, help = "Replay buffer cap in bytes")]
    replay_buffer_bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
struct BootstrapOutput {
    session_id: Uuid,
    auth_token: String,
    auth_token_expires_in_seconds: u64,
    quic_addr: String,
    cert_fingerprint: String,
}

#[derive(Debug, Deserialize)]
struct AuthReq {
    token: String,
}

#[derive(Debug, Deserialize)]
struct AttachReq {
    session_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct ResumeReq {
    session_id: Uuid,
    resume_token: String,
}

#[derive(Debug, Deserialize)]
struct ResizeReq {
    rows: u16,
    cols: u16,
}

#[derive(Debug, Deserialize)]
struct RenewIpcReq {
    session_id: Uuid,
    user: String,
}

struct ServerState {
    session_id: Uuid,
    owner_user: String,
    owner_uid: u32,
    quic_addr: String,
    cert_fingerprint: String,
    session_timeout: Duration,
    auth_token_ttl: Duration,
    resume_token_ttl: Duration,
    replay_cap: usize,
    sessions: SessionManager,
    tokens: TokenService,
    pty: Option<LivePty>,
    pty_writer: Option<Arc<std::sync::Mutex<Box<dyn Write + Send>>>>,
    output_tx: Option<broadcast::Sender<Vec<u8>>>,
    replay: Vec<u8>,
    detached_at: Option<SystemTime>,
    pty_exited: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupReason {
    ConnectionEnd,
    ExplicitDetach,
}

impl ServerState {
    fn new(
        session_id: Uuid,
        owner_user: String,
        owner_uid: u32,
        quic_addr: String,
        cert_fingerprint: String,
        cfg: &NewArgs,
    ) -> Self {
        let mut sessions = SessionManager::default();
        sessions.create_session_with_id(
            session_id,
            owner_user.clone(),
            quic_addr.clone(),
            cert_fingerprint.clone(),
        );

        Self {
            session_id,
            owner_user,
            owner_uid,
            quic_addr,
            cert_fingerprint,
            session_timeout: Duration::from_secs(cfg.session_timeout),
            auth_token_ttl: Duration::from_secs(cfg.auth_token_ttl),
            resume_token_ttl: Duration::from_secs(cfg.resume_token_ttl),
            replay_cap: cfg.replay_buffer_bytes,
            sessions,
            tokens: TokenService::default(),
            pty: None,
            pty_writer: None,
            output_tx: None,
            replay: Vec::new(),
            detached_at: None,
            pty_exited: Arc::new(AtomicBool::new(false)),
        }
    }

    fn issue_auth_token(&mut self) -> String {
        self.tokens
            .issue_auth_token(self.session_id, &self.owner_user, self.auth_token_ttl)
    }

    fn issue_resume_token(&mut self) -> String {
        self.tokens
            .issue_resume_token(self.session_id, &self.owner_user, self.resume_token_ttl)
    }

    fn bootstrap_output(&mut self) -> BootstrapOutput {
        BootstrapOutput {
            session_id: self.session_id,
            auth_token: self.issue_auth_token(),
            auth_token_expires_in_seconds: self.auth_token_ttl.as_secs(),
            quic_addr: self.quic_addr.clone(),
            cert_fingerprint: self.cert_fingerprint.clone(),
        }
    }

    fn append_replay(&mut self, chunk: &[u8]) {
        self.replay.extend_from_slice(chunk);
        if self.replay.len() > self.replay_cap {
            let overflow = self.replay.len() - self.replay_cap;
            self.replay.drain(0..overflow);
        }
    }

    fn expire_if_detached_timeout(&mut self, now: SystemTime) -> bool {
        if let Some(detached_at) = self.detached_at {
            if let Ok(elapsed) = now.duration_since(detached_at) {
                if elapsed >= self.session_timeout {
                    let _ = self.sessions.expire(self.session_id);
                    if let Some(s) = self.sessions.session(self.session_id) {
                        if s.state == SessionState::Expired {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

#[derive(Debug, thiserror::Error)]
enum NeoshdError {
    #[error("config error: {0}")]
    Config(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("quic error: {0}")]
    Quinn(#[from] quinn::ConnectionError),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("auth failed")]
    AuthFailed,
    #[error("session not found")]
    SessionNotFound,
    #[error("session expired")]
    SessionExpired,
    #[error("attach denied")]
    AttachDenied,
    #[error("internal error: {0}")]
    Internal(String),
}

#[tokio::main]
async fn main() {
    install_rustls_crypto_provider();
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Version => {
            println!("{}", SERVER_VERSION);
            Ok(())
        }
        Commands::RenewAuth { session_id, user } => run_renew_auth(session_id, &user).await,
        Commands::New(args) => run_new(args).await,
    };

    if let Err(err) = result {
        eprintln!("{}", json!({"error": err.to_string()}));
        std::process::exit(1);
    }
}

fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

async fn run_renew_auth(session_id: Uuid, user: &str) -> Result<(), NeoshdError> {
    let sock = ipc_socket_path(session_id);
    let mut stream = UnixStream::connect(&sock)
        .await
        .map_err(|e| NeoshdError::Config(format!("cannot reach session control socket {}: {e}", sock.display())))?;

    let req = json!({
        "session_id": session_id,
        "user": user,
    });
    stream.write_all(req.to_string().as_bytes()).await?;
    stream.shutdown().await?;

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let value: Value = serde_json::from_slice(&buf)
        .map_err(|e| NeoshdError::Internal(format!("invalid IPC response: {e}")))?;

    if value.get("error").is_some() {
        return Err(NeoshdError::Internal(value.to_string()));
    }

    println!("{}", value);
    Ok(())
}

async fn run_new(args: NewArgs) -> Result<(), NeoshdError> {
    validate_new_args(&args)?;
    let (server_config, cert_fingerprint) = build_server_tls(&args)?;
    let bind_addrs = resolve_bind_addrs(&args)?;
    let (endpoint, bound_addr) = bind_endpoint(server_config, &bind_addrs, &args.port_range)?;

    let session_id = Uuid::new_v4();
    let quic_addr = format!("{}:{}", bound_addr.ip(), bound_addr.port());

    let state = Arc::new(Mutex::new(ServerState::new(
        session_id,
        args.user.clone(),
        unsafe { libc::geteuid() as u32 },
        quic_addr.clone(),
        cert_fingerprint,
        &args,
    )));

    let bootstrap = {
        let mut locked = state.lock().await;
        locked.bootstrap_output()
    };
    println!("{}", serde_json::to_string(&bootstrap).map_err(|e| NeoshdError::Internal(e.to_string()))?);
    log_event(
        "token_issued",
        json!({"token_type":"auth_token","session_id":bootstrap.session_id}),
    );

    let ipc_path = ipc_socket_path(session_id);
    if let Some(parent) = ipc_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _ = fs::remove_file(&ipc_path);

    let ipc_task = {
        let state = Arc::clone(&state);
        let ipc_path = ipc_path.clone();
        tokio::spawn(async move { run_ipc_server(&ipc_path, state).await })
    };
    let sweep_task = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                let mut st = state.lock().await;
                sweep_tokens(&mut st, SystemTime::now(), Duration::from_secs(300));
            }
        })
    };

    log_event("server_start", json!({"session_id": session_id, "quic_addr": quic_addr}));

    let accept_res = run_quic_accept_loop(endpoint, Arc::clone(&state)).await;
    let _ = ipc_task.abort();
    let _ = sweep_task.abort();
    let _ = fs::remove_file(&ipc_path);

    accept_res
}

async fn run_ipc_server(path: &Path, state: Arc<Mutex<ServerState>>) -> Result<(), NeoshdError> {
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;

    loop {
        let (mut stream, _) = listener.accept().await?;
        let peer_uid = peer_uid(&stream)?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;

        let response = match serde_json::from_slice::<RenewIpcReq>(&buf) {
            Ok(req) => {
                let mut st = state.lock().await;
                if peer_uid != st.owner_uid {
                    log_event("renew_auth_denied", json!({"reason":"peer_uid_mismatch","peer_uid":peer_uid}));
                    json!({"error": "permission denied"})
                } else if req.session_id != st.session_id {
                    json!({"error": "session_id mismatch"})
                } else if req.user != st.owner_user {
                    log_event("renew_auth_denied", json!({"reason":"user_mismatch"}));
                    json!({"error": "permission denied"})
                } else {
                    log_event("token_issued", json!({"token_type":"auth_token","session_id":st.session_id}));
                    json!(st.bootstrap_output())
                }
            }
            Err(err) => json!({"error": format!("bad request: {err}")}),
        };

        stream.write_all(response.to_string().as_bytes()).await?;
        stream.shutdown().await?;
    }
}

fn peer_uid(stream: &UnixStream) -> Result<u32, NeoshdError> {
    let cred = stream
        .peer_cred()
        .map_err(|e| NeoshdError::Internal(format!("peer credential read failed: {e}")))?;
    Ok(cred.uid())
}

async fn run_quic_accept_loop(endpoint: Endpoint, state: Arc<Mutex<ServerState>>) -> Result<(), NeoshdError> {
    loop {
        {
            let mut st = state.lock().await;
            if st.expire_if_detached_timeout(SystemTime::now()) {
                log_event("session_expired", json!({"session_id": st.session_id}));
                break;
            }
            if should_server_stop(&st) {
                break;
            }
        }

        let incoming = match tokio::time::timeout(Duration::from_secs(1), endpoint.accept()).await {
            Ok(Some(x)) => x,
            Ok(None) => break,
            Err(_) => continue,
        };

        let state_for_conn = Arc::clone(&state);
        tokio::spawn(async move {
            let connecting = match incoming.await {
                Ok(c) => c,
                Err(err) => {
                    log_event("conn_accept_error", json!({"error": err.to_string()}));
                    return;
                }
            };

            if let Err(err) = handle_connection(connecting, state_for_conn).await {
                log_event("conn_error", json!({"error": err.to_string()}));
            }
        });
    }

    Ok(())
}

fn should_server_stop(st: &ServerState) -> bool {
    matches!(
        st.sessions.session(st.session_id).map(|s| s.state),
        Some(SessionState::Terminated | SessionState::Expired)
    )
}

async fn handle_connection(connection: Connection, state: Arc<Mutex<ServerState>>) -> Result<(), NeoshdError> {
    let conn_id = Uuid::new_v4();
    log_event("conn_open", json!({"conn_id": conn_id.to_string(), "remote": connection.remote_address().to_string()}));

    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|e| NeoshdError::Protocol(format!("control stream missing: {e}")))?;

    let mut dispatcher = Dispatcher::default();
    let mut attached_epoch: Option<u64> = None;
    let mut authed = false;
    let mut cleanup_reason = CleanupReason::ConnectionEnd;

    loop {
        let payload = match read_control_frame(&mut recv).await {
            Ok(p) => p,
            Err(_) => break,
        };

        let kind = match parse_message_kind(&payload) {
            Some(k) => k,
            None => {
                write_error_frame(&mut send, &NeoshdError::Protocol("unknown control message".to_string()))
                    .await?;
                break;
            }
        };

        if dispatcher.on_message(kind).is_err() {
            write_error_frame(&mut send, &NeoshdError::Protocol("out-of-order control message".to_string()))
                .await?;
            break;
        }

        match kind {
            MessageKind::Hello => {
                let timeout = { state.lock().await.session_timeout.as_secs() };
                write_control_json(
                    &mut send,
                    &json!({
                        "type": "HELLO_ACK",
                        "protocol_version": "0.1.0",
                        "server_version": SERVER_VERSION,
                        "capabilities": ["stdin-bytes", "resume-v1"],
                        "session_timeout_seconds": timeout,
                    }),
                )
                .await?;
            }
            MessageKind::Auth => {
                let req: AuthReq = match serde_json::from_slice(&payload) {
                    Ok(v) => v,
                    Err(e) => {
                        write_error_frame(&mut send, &NeoshdError::Protocol(format!("bad AUTH payload: {e}")))
                            .await?;
                        break;
                    }
                };
                let (session_id, resume_token, resume_ttl) = {
                    let mut st = state.lock().await;
                    let sid = st.session_id;
                    let owner = st.owner_user.clone();
                    if let Err(err) = st
                        .tokens
                        .validate_and_consume_auth(&req.token, sid, &owner, SystemTime::now())
                    {
                        write_error_frame(&mut send, &map_token_auth_error(err)).await?;
                        log_event("auth_failed", json!({"conn_id": conn_id.to_string()}));
                        break;
                    }
                    log_event("token_consumed", json!({"token_type":"auth_token","session_id":sid}));
                    let resume = st.issue_resume_token();
                    log_event("token_issued", json!({"token_type":"resume_token","session_id":sid}));
                    (sid, resume, st.resume_token_ttl.as_secs())
                };
                authed = true;

                write_control_json(
                    &mut send,
                    &json!({
                        "type": "AUTH_OK",
                        "session_id": session_id,
                        "resume_token": resume_token,
                        "resume_token_expires_in_seconds": resume_ttl,
                    }),
                )
                .await?;
                log_event("auth_ok", json!({"conn_id": conn_id.to_string()}));
            }
            MessageKind::Attach => {
                if !authed {
                    write_error_frame(&mut send, &NeoshdError::Protocol("ATTACH before AUTH".to_string()))
                        .await?;
                    break;
                }
                let req: AttachReq = match serde_json::from_slice(&payload) {
                    Ok(v) => v,
                    Err(e) => {
                        write_error_frame(&mut send, &NeoshdError::Protocol(format!("bad ATTACH payload: {e}")))
                            .await?;
                        break;
                    }
                };
                let epoch = {
                    let mut st = state.lock().await;
                    let sid = st.session_id;
                    if req.session_id != st.session_id {
                        write_error_frame(&mut send, &NeoshdError::SessionNotFound).await?;
                        break;
                    }
                    let epoch = match st
                        .sessions
                        .attach_exclusive(sid, conn_id)
                    {
                        Ok(v) => v,
                        Err(err) => {
                            write_error_frame(&mut send, &map_session_error(err)).await?;
                            break;
                        }
                    };
                    st.detached_at = None;
                    ensure_pty_runtime(&mut st)?;
                    epoch
                };
                attached_epoch = Some(epoch);

                write_control_json(
                    &mut send,
                    &json!({"type":"ATTACH_OK", "session_id": req.session_id}),
                )
                .await?;
                start_data_plane(connection.clone(), Arc::clone(&state), false).await?;
                log_event("attach_ok", json!({"conn_id": conn_id.to_string()}));
            }
            MessageKind::Resume => {
                if !authed {
                    write_error_frame(&mut send, &NeoshdError::Protocol("RESUME before AUTH".to_string()))
                        .await?;
                    break;
                }
                let req: ResumeReq = match serde_json::from_slice(&payload) {
                    Ok(v) => v,
                    Err(e) => {
                        write_error_frame(&mut send, &NeoshdError::Protocol(format!("bad RESUME payload: {e}")))
                            .await?;
                        break;
                    }
                };
                let (epoch, replay_len) = {
                    let mut st = state.lock().await;
                    let sid = st.session_id;
                    let owner = st.owner_user.clone();
                    if req.session_id != st.session_id {
                        log_event("resume_failed", json!({"conn_id": conn_id.to_string(), "reason":"session_not_found"}));
                        write_error_frame(&mut send, &NeoshdError::SessionNotFound).await?;
                        break;
                    }
                    if let Err(err) = st
                        .tokens
                        .validate_resume(&req.resume_token, sid, &owner, SystemTime::now())
                    {
                        log_event("resume_failed", json!({"conn_id": conn_id.to_string(), "reason":"token_invalid"}));
                        write_error_frame(&mut send, &map_token_resume_error(err)).await?;
                        break;
                    }
                    let epoch = match st
                        .sessions
                        .attach_exclusive(sid, conn_id)
                    {
                        Ok(v) => v,
                        Err(err) => {
                            log_event("resume_failed", json!({"conn_id": conn_id.to_string(), "reason":"attach_denied"}));
                            write_error_frame(&mut send, &map_session_error(err)).await?;
                            break;
                        }
                    };
                    st.detached_at = None;
                    ensure_pty_runtime(&mut st)?;
                    (epoch, st.replay.len())
                };
                attached_epoch = Some(epoch);

                write_control_json(
                    &mut send,
                    &json!({"type":"RESUME_OK", "session_id": req.session_id, "replay_bytes": replay_len}),
                )
                .await?;
                start_data_plane(connection.clone(), Arc::clone(&state), true).await?;
                log_event("resume_ok", json!({"conn_id": conn_id.to_string(), "replay_bytes": replay_len}));
            }
            MessageKind::Resize => {
                let req: ResizeReq = match serde_json::from_slice(&payload) {
                    Ok(v) => v,
                    Err(e) => {
                        write_error_frame(&mut send, &NeoshdError::Protocol(format!("bad RESIZE payload: {e}")))
                            .await?;
                        break;
                    }
                };
                let st = state.lock().await;
                if let Some(pty) = st.pty.as_ref() {
                    pty.resize(req.rows, req.cols)
                        .map_err(|e| NeoshdError::Internal(e.to_string()))?;
                }
            }
            MessageKind::Detach => {
                let mut st = state.lock().await;
                let sid = st.session_id;
                st.sessions
                    .detach(sid, conn_id)
                    .map_err(map_session_error)?;
                st.detached_at = Some(SystemTime::now());
                cleanup_reason = CleanupReason::ExplicitDetach;
                break;
            }
            MessageKind::Close => {
                let mut st = state.lock().await;
                let sid = st.session_id;
                st.sessions
                    .terminate(sid)
                    .map_err(map_session_error)?;
                log_event("session_terminated", json!({"session_id": st.session_id}));
                break;
            }
            MessageKind::Ping => {
                let nonce = serde_json::from_slice::<Value>(&payload)
                    .ok()
                    .and_then(|v| v.get("nonce").cloned())
                    .unwrap_or(json!(""));
                write_control_json(&mut send, &json!({"type":"PONG", "nonce": nonce})).await?;
            }
            MessageKind::Pong | MessageKind::Error => {}
        }
    }

    if let Some(epoch) = attached_epoch {
        let mut st = state.lock().await;
        finalize_connection_cleanup(&mut st, conn_id, epoch, cleanup_reason);
    }

    connection.close(VarInt::from_u32(0), b"connection closed");
    log_event("conn_close", json!({"conn_id": conn_id.to_string()}));
    Ok(())
}

fn ensure_pty_runtime(st: &mut ServerState) -> Result<(), NeoshdError> {
    if st.pty.is_some() {
        return Ok(());
    }

    let shell = resolve_login_shell();
    let mut pty =
        LivePty::spawn(24, 80, &shell).map_err(|e| NeoshdError::Internal(e.to_string()))?;
    let reader = pty.take_reader().map_err(|e| NeoshdError::Internal(e.to_string()))?;
    let writer = pty.writer();
    log_event("pty_spawn", json!({"shell": shell}));

    let (tx, _rx) = broadcast::channel::<Vec<u8>>(64);
    let tx_for_thread = tx.clone();
    let pty_exited = Arc::new(AtomicBool::new(false));
    let pty_exited_for_thread = Arc::clone(&pty_exited);

    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = tx_for_thread.send(buf[..n].to_vec());
                }
                Err(_) => break,
            }
        }
        pty_exited_for_thread.store(true, Ordering::SeqCst);
        let _ = tx_for_thread.send(Vec::new());
    });

    st.pty_writer = Some(writer);
    st.output_tx = Some(tx);
    st.pty = Some(pty);
    st.pty_exited = pty_exited;
    Ok(())
}

fn resolve_login_shell() -> String {
    env::var("SHELL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "sh".to_string())
}

async fn start_data_plane(connection: Connection, state: Arc<Mutex<ServerState>>, resume: bool) -> Result<(), NeoshdError> {
    let mut stdout_stream = connection
        .open_uni()
        .await
        .map_err(|e| NeoshdError::Protocol(format!("open STDOUT stream failed: {e}")))?;

    let (writer, mut rx, replay) = {
        let st = state.lock().await;
        let writer = st
            .pty_writer
            .as_ref()
            .ok_or_else(|| NeoshdError::Internal("PTY writer missing".to_string()))?
            .clone();
        let tx = st
            .output_tx
            .as_ref()
            .ok_or_else(|| NeoshdError::Internal("PTY output channel missing".to_string()))?
            .clone();
        let replay = if resume { st.replay.clone() } else { Vec::new() };
        (writer, tx.subscribe(), replay)
    };

    if !replay.is_empty() {
        stdout_stream
            .write_all(&replay)
            .await
            .map_err(|e| NeoshdError::Internal(format!("write replay failed: {e}")))?;
    }

    let state_for_out = Arc::clone(&state);
    tokio::spawn(async move {
        while let Ok(chunk) = rx.recv().await {
            if chunk.is_empty() {
                break;
            }
            {
                let mut st = state_for_out.lock().await;
                st.append_replay(&chunk);
            }
            if stdout_stream.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let _ = stdout_stream.finish();
    });

    tokio::spawn(async move {
        let mut stdin_stream = match connection.accept_uni().await {
            Ok(stream) => stream,
            Err(err) => {
                log_event("data_plane_stdin_missing", json!({"error": err.to_string()}));
                return;
            }
        };
        let mut buf = [0u8; 4096];
        loop {
            let n = match stdin_stream.read(&mut buf).await {
                Ok(Some(n)) => n,
                Ok(None) => break,
                Err(_) => break,
            };

            if let Ok(mut w) = writer.lock() {
                if w.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = w.flush();
            } else {
                break;
            }
        }
    });

    Ok(())
}

fn finalize_connection_cleanup(
    st: &mut ServerState,
    conn_id: Uuid,
    epoch: u64,
    reason: CleanupReason,
) {
    let sid = st.session_id;
    if reason == CleanupReason::ExplicitDetach {
        if st.sessions.conditional_stale_cleanup(sid, conn_id, epoch) {
            st.detached_at = Some(SystemTime::now());
        }
        return;
    }

    if st.pty_exited.load(Ordering::SeqCst) {
        let _ = st.sessions.terminate(sid);
        st.detached_at = None;
        log_event(
            "session_terminated",
            json!({"session_id": sid, "reason":"pty_exit"}),
        );
        return;
    }

    if st.sessions.conditional_stale_cleanup(sid, conn_id, epoch) {
        st.detached_at = Some(SystemTime::now());
    }
}

async fn read_control_frame(recv: &mut quinn::RecvStream) -> Result<Vec<u8>, NeoshdError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| NeoshdError::Protocol(format!("read frame length failed: {e}")))?;
    let payload_len = u32::from_be_bytes(len_buf) as usize;
    if payload_len > MAX_CONTROL_FRAME {
        return Err(NeoshdError::Protocol("control frame too large".to_string()));
    }
    let mut payload = vec![0u8; payload_len];
    recv.read_exact(&mut payload)
        .await
        .map_err(|e| NeoshdError::Protocol(format!("read frame payload failed: {e}")))?;

    let frame = [&len_buf[..], &payload[..]].concat();
    let decoded = decode_frame(&frame, MAX_CONTROL_FRAME)
        .map_err(|e| NeoshdError::Protocol(format!("decode frame failed: {e}")))?;
    Ok(decoded.to_vec())
}

async fn write_control_json(send: &mut quinn::SendStream, value: &Value) -> Result<(), NeoshdError> {
    let payload = serde_json::to_vec(value).map_err(|e| NeoshdError::Internal(e.to_string()))?;
    let frame = encode_frame(&payload);
    send.write_all(&frame)
        .await
        .map_err(|e| NeoshdError::Protocol(format!("write frame failed: {e}")))
}

async fn write_error_frame(send: &mut quinn::SendStream, err: &NeoshdError) -> Result<(), NeoshdError> {
    let msg = error_frame_for(err);
    write_control_json(send, &json!(msg)).await
}

fn error_frame_for(err: &NeoshdError) -> ErrorMessage {
    let (code, retryable) = match err {
        NeoshdError::AuthFailed => ("AUTH_FAILED", false),
        NeoshdError::SessionNotFound => ("SESSION_NOT_FOUND", false),
        NeoshdError::SessionExpired => ("SESSION_EXPIRED", false),
        NeoshdError::AttachDenied => ("ATTACH_DENIED", false),
        NeoshdError::Protocol(_) => ("PROTOCOL_ERROR", false),
        NeoshdError::Internal(_) | NeoshdError::Io(_) | NeoshdError::Quinn(_) | NeoshdError::Config(_) => {
            ("INTERNAL_ERROR", true)
        }
    };
    ErrorMessage {
        msg_type: "ERROR",
        code,
        message: err.to_string(),
        retryable,
    }
}

fn validate_new_args(args: &NewArgs) -> Result<(), NeoshdError> {
    if args.port_range.split(':').count() != 2 {
        return Err(NeoshdError::Config("--port-range must be START:END".to_string()));
    }
    if (args.tls_cert.is_empty() && !args.tls_key.is_empty())
        || (!args.tls_cert.is_empty() && args.tls_key.is_empty())
    {
        return Err(NeoshdError::Config(
            "--tls-cert and --tls-key must be provided together".to_string(),
        ));
    }
    Ok(())
}

fn build_server_tls(args: &NewArgs) -> Result<(ServerConfig, String), NeoshdError> {
    let (cert_der, key_der) = if args.tls_cert.is_empty() {
        let cert = generate_simple_self_signed(vec!["localhost".to_string()])
            .map_err(|e| NeoshdError::Internal(format!("generate cert failed: {e}")))?;
        (
            cert.cert.der().to_vec(),
            cert.key_pair.serialize_der(),
        )
    } else {
        load_cert_key_pair(Path::new(&args.tls_cert), Path::new(&args.tls_key))?
    };

    let mut rustls_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert_der.clone())],
            PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key_der.clone())),
        )
        .map_err(|e| NeoshdError::Config(format!("invalid TLS config: {e}")))?;
    rustls_cfg.alpn_protocols = vec![ALPN.to_vec()];

    let mut server_config = ServerConfig::with_crypto(Arc::new(
        QuicServerConfig::try_from(rustls_cfg)
            .map_err(|e| NeoshdError::Config(format!("quic rustls config failed: {e}")))?,
    ));

    server_config.transport_config(Arc::new({
        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(VarInt::from_u32(1));
        transport.max_concurrent_uni_streams(VarInt::from_u32(8));
        transport
    }));

    let fp = sha256_fingerprint(&cert_der);
    Ok((server_config, fp))
}

fn load_cert_key_pair(cert_path: &Path, key_path: &Path) -> Result<(Vec<u8>, Vec<u8>), NeoshdError> {
    let cert_pem = fs::read(cert_path)?;
    let key_pem = fs::read(key_path)?;

    let mut cert_reader = std::io::Cursor::new(cert_pem);
    let certs = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| NeoshdError::Config(format!("read cert failed: {e}")))?;
    let cert = certs
        .first()
        .ok_or_else(|| NeoshdError::Config("empty cert file".to_string()))?
        .to_vec();

    let mut key_reader = std::io::Cursor::new(key_pem);
    let mut keys = rustls_pemfile::pkcs8_private_keys(&mut key_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| NeoshdError::Config(format!("read key failed: {e}")))?;
    let key = keys
        .pop()
        .ok_or_else(|| NeoshdError::Config("empty key file".to_string()))?
        .secret_pkcs8_der()
        .to_vec();

    Ok((cert, key))
}

fn sha256_fingerprint(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    let mut out = String::from("sha256:");
    for b in digest {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn resolve_bind_addrs(args: &NewArgs) -> Result<Vec<IpAddr>, NeoshdError> {
    match args.bind_server.as_str() {
        "ssh" => {
            if let Ok(raw) = env::var("SSH_CONNECTION") {
                let fields: Vec<&str> = raw.split_whitespace().collect();
                if fields.len() >= 4 {
                    if let Ok(ip) = fields[2].parse::<IpAddr>() {
                        return Ok(vec![ip]);
                    }
                }
            }
            Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
        }
        "any" => Ok(vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)]),
        other => {
            if let Ok(ip) = other.parse::<IpAddr>() {
                return Ok(vec![ip]);
            }

            let addrs: Vec<IpAddr> = (other, 0)
                .to_socket_addrs()
                .map_err(|e| NeoshdError::Config(format!("resolve --bind-server failed: {e}")))?
                .map(|a| a.ip())
                .collect();
            if addrs.is_empty() {
                return Err(NeoshdError::Config("--bind-server resolved no addresses".to_string()));
            }
            Ok(addrs)
        }
    }
}

fn parse_port_range(range: &str) -> Result<(u16, u16), NeoshdError> {
    let mut parts = range.split(':');
    let start: u16 = parts
        .next()
        .ok_or_else(|| NeoshdError::Config("missing range start".to_string()))?
        .parse()
        .map_err(|_| NeoshdError::Config("invalid range start".to_string()))?;
    let end: u16 = parts
        .next()
        .ok_or_else(|| NeoshdError::Config("missing range end".to_string()))?
        .parse()
        .map_err(|_| NeoshdError::Config("invalid range end".to_string()))?;
    if start == 0 || end < start {
        return Err(NeoshdError::Config("invalid --port-range".to_string()));
    }
    Ok((start, end))
}

fn sweep_tokens(st: &mut ServerState, now: SystemTime, retention: Duration) {
    st.tokens.cleanup_expired(now, retention);
}

fn bind_endpoint(
    server_config: ServerConfig,
    bind_addrs: &[IpAddr],
    range: &str,
) -> Result<(Endpoint, SocketAddr), NeoshdError> {
    let (start, end) = parse_port_range(range)?;
    for ip in bind_addrs {
        for port in start..=end {
            let addr = SocketAddr::new(*ip, port);
            if let Ok(endpoint) = Endpoint::server(server_config.clone(), addr) {
                return Ok((endpoint, addr));
            }
        }
    }
    Err(NeoshdError::Config("no available port in range".to_string()))
}

fn ipc_socket_path(session_id: Uuid) -> PathBuf {
    let runtime_dir = env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir)
        .join("neoshd")
        .join(format!("{}.sock", session_id))
}

fn map_token_auth_error(err: TokenError) -> NeoshdError {
    match err {
        TokenError::BindingMismatch | TokenError::Consumed | TokenError::Expired | TokenError::NotFound | TokenError::Revoked | TokenError::TypeMismatch => NeoshdError::AuthFailed,
    }
}

fn map_token_resume_error(err: TokenError) -> NeoshdError {
    match err {
        TokenError::Expired | TokenError::Revoked => NeoshdError::SessionExpired,
        TokenError::BindingMismatch | TokenError::Consumed | TokenError::NotFound | TokenError::TypeMismatch => {
            NeoshdError::AuthFailed
        }
    }
}

fn map_session_error(err: SessionError) -> NeoshdError {
    match err {
        SessionError::NotFound => NeoshdError::SessionNotFound,
        SessionError::AttachDenied => NeoshdError::AttachDenied,
        SessionError::InvalidState => NeoshdError::SessionExpired,
        SessionError::PermissionDenied => NeoshdError::AuthFailed,
    }
}

fn log_event(event: &str, payload: Value) {
    eprintln!(
        "{}",
        json!({"ts": now_rfc3339(), "ts_ms": now_epoch_millis(), "pid": std::process::id(), "event": event, "payload": payload})
    );
}

fn now_epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_new_args() -> NewArgs {
        NewArgs {
            user: "alice".to_string(),
            port_range: "30000:39999".to_string(),
            bind_server: "ssh".to_string(),
            tls_cert: String::new(),
            tls_key: String::new(),
            session_timeout: 600,
            auth_token_ttl: 60,
            resume_token_ttl: 86400,
            replay_buffer_bytes: 1_048_576,
        }
    }

    #[test]
    fn parse_port_range_accepts_valid_value() {
        assert_eq!(parse_port_range("30000:39999").unwrap(), (30000, 39999));
    }

    #[test]
    fn parse_port_range_rejects_invalid_value() {
        assert!(parse_port_range("0:39999").is_err());
        assert!(parse_port_range("40000:30000").is_err());
        assert!(parse_port_range("abc:def").is_err());
    }

    #[test]
    fn validate_new_args_requires_cert_and_key_pair() {
        let mut args = base_new_args();
        args.tls_cert = "/tmp/server.crt".to_string();
        assert!(validate_new_args(&args).is_err());

        args.tls_cert.clear();
        args.tls_key = "/tmp/server.key".to_string();
        assert!(validate_new_args(&args).is_err());
    }

    #[test]
    fn resolve_bind_any_returns_unspecified() {
        let mut args = base_new_args();
        args.bind_server = "any".to_string();
        let addrs = resolve_bind_addrs(&args).unwrap();
        assert_eq!(addrs, vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)]);
    }

    #[test]
    fn token_and_session_error_mapping_matches_protocol_codes() {
        assert!(matches!(
            map_token_auth_error(TokenError::Consumed),
            NeoshdError::AuthFailed
        ));
        assert!(matches!(
            map_token_resume_error(TokenError::Expired),
            NeoshdError::SessionExpired
        ));
        assert!(matches!(
            map_session_error(SessionError::AttachDenied),
            NeoshdError::AttachDenied
        ));
    }

    #[test]
    fn detached_timeout_moves_session_to_expired() {
        let cfg = base_new_args();
        let sid = Uuid::new_v4();
        let mut state = ServerState::new(
            sid,
            "alice".to_string(),
            1000,
            "127.0.0.1:30001".to_string(),
            "sha256:test".to_string(),
            &cfg,
        );
        state.detached_at = Some(SystemTime::now() - Duration::from_secs(700));
        assert!(state.expire_if_detached_timeout(SystemTime::now()));
        assert_eq!(
            state.sessions.session(sid).unwrap().state,
            SessionState::Expired
        );
    }

    #[test]
    fn server_stop_condition_matches_terminal_states() {
        let cfg = base_new_args();
        let sid = Uuid::new_v4();
        let mut state = ServerState::new(
            sid,
            "alice".to_string(),
            1000,
            "127.0.0.1:30001".to_string(),
            "sha256:test".to_string(),
            &cfg,
        );
        assert!(!should_server_stop(&state));

        state.sessions.terminate(sid).unwrap();
        assert!(should_server_stop(&state));
    }

    #[test]
    fn error_frame_mapping_matches_protocol_codes() {
        let auth = error_frame_for(&NeoshdError::AuthFailed);
        assert_eq!(auth.code, "AUTH_FAILED");
        let sess = error_frame_for(&NeoshdError::SessionExpired);
        assert_eq!(sess.code, "SESSION_EXPIRED");
        let proto = error_frame_for(&NeoshdError::Protocol("x".into()));
        assert_eq!(proto.code, "PROTOCOL_ERROR");
    }

    #[test]
    fn token_sweeper_removes_expired_records() {
        let cfg = base_new_args();
        let sid = Uuid::new_v4();
        let mut state = ServerState::new(
            sid,
            "alice".to_string(),
            1000,
            "127.0.0.1:30001".to_string(),
            "sha256:test".to_string(),
            &cfg,
        );
        let token = state
            .tokens
            .issue_auth_token(sid, "alice", Duration::from_secs(1));
        let now = SystemTime::now() + Duration::from_secs(5);
        sweep_tokens(&mut state, now, Duration::from_secs(0));
        assert!(state
            .tokens
            .validate_and_consume_auth(&token, sid, "alice", now)
            .is_err());
    }

    #[test]
    fn cleanup_terminates_session_when_pty_exited() {
        let cfg = base_new_args();
        let sid = Uuid::new_v4();
        let conn = Uuid::new_v4();
        let mut state = ServerState::new(
            sid,
            "alice".to_string(),
            1000,
            "127.0.0.1:30001".to_string(),
            "sha256:test".to_string(),
            &cfg,
        );
        let epoch = state.sessions.attach_exclusive(sid, conn).unwrap();
        state.pty_exited.store(true, Ordering::SeqCst);

        finalize_connection_cleanup(&mut state, conn, epoch, CleanupReason::ConnectionEnd);
        assert_eq!(
            state.sessions.session(sid).unwrap().state,
            SessionState::Terminated
        );
        assert!(state.detached_at.is_none());
    }

    #[test]
    fn cleanup_marks_detached_when_connection_drops_with_live_pty() {
        let cfg = base_new_args();
        let sid = Uuid::new_v4();
        let conn = Uuid::new_v4();
        let mut state = ServerState::new(
            sid,
            "alice".to_string(),
            1000,
            "127.0.0.1:30001".to_string(),
            "sha256:test".to_string(),
            &cfg,
        );
        let epoch = state.sessions.attach_exclusive(sid, conn).unwrap();
        state.pty_exited.store(false, Ordering::SeqCst);

        finalize_connection_cleanup(&mut state, conn, epoch, CleanupReason::ConnectionEnd);
        assert_eq!(state.sessions.session(sid).unwrap().state, SessionState::Detached);
        assert!(state.detached_at.is_some());
    }

    #[test]
    fn explicit_detach_wins_over_pty_exit_race() {
        let cfg = base_new_args();
        let sid = Uuid::new_v4();
        let conn = Uuid::new_v4();
        let mut state = ServerState::new(
            sid,
            "alice".to_string(),
            1000,
            "127.0.0.1:30001".to_string(),
            "sha256:test".to_string(),
            &cfg,
        );
        let epoch = state.sessions.attach_exclusive(sid, conn).unwrap();
        state.pty_exited.store(true, Ordering::SeqCst);

        finalize_connection_cleanup(&mut state, conn, epoch, CleanupReason::ExplicitDetach);
        assert_eq!(state.sessions.session(sid).unwrap().state, SessionState::Detached);
    }
}
