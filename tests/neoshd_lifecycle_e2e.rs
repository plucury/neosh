use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use quinn::{ClientConfig, Endpoint, RecvStream, SendStream};
use rcgen::generate_simple_self_signed;
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use serde_json::{Value, json};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, Command};

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_initial_attach_timeout_triggers_auto_shutdown_log() -> TestResult<()> {
    let tmp = unique_temp_dir("e2e-timeout").await?;
    let (cert_path, key_path) = write_self_signed_cert_pair(&tmp).await?;
    let xdg_runtime_dir = unique_short_runtime_dir("to").await?;
    let port_range = unique_port_range("to")?;
    fs::create_dir_all(&xdg_runtime_dir).await?;

    let mut child = spawn_neoshd(
        &cert_path,
        &key_path,
        &xdg_runtime_dir,
        2,
        600,
        &port_range,
    )?;
    let stdout = child.stdout.take().ok_or("missing child stdout")?;
    let stderr = child.stderr.take().ok_or("missing child stderr")?;
    let stderr_task = tokio::spawn(collect_stderr(stderr));

    let mut stdout_lines = BufReader::new(stdout).lines();
    let bootstrap_line = match tokio::time::timeout(Duration::from_secs(5), stdout_lines.next_line()).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => {
            let _ = child.kill().await;
            let stderr_log = stderr_task.await??;
            return Err(format!("missing bootstrap line; stderr={stderr_log}").into());
        }
        Ok(Err(e)) => {
            let _ = child.kill().await;
            let stderr_log = stderr_task.await??;
            return Err(format!("bootstrap stdout read error: {e}; stderr={stderr_log}").into());
        }
        Err(_) => {
            let _ = child.kill().await;
            let stderr_log = stderr_task.await??;
            return Err(format!("bootstrap stdout timeout; stderr={stderr_log}").into());
        }
    };
    let _bootstrap: Value = serde_json::from_str(&bootstrap_line)?;

    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .map_err(|_| "neoshd did not auto-stop in time")??;
    let stderr_log = stderr_task.await??;

    assert!(status.success(), "neoshd exited with non-zero status: {status}");
    assert!(
        stderr_log.contains("\"event\":\"initial_attach_timeout\""),
        "missing initial_attach_timeout log: {stderr_log}"
    );
    assert!(
        stderr_log.contains("\"event\":\"server_auto_shutdown\"")
            && stderr_log.contains("\"reason\":\"initial_attach_timeout\""),
        "missing server_auto_shutdown(reason=initial_attach_timeout) log: {stderr_log}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_normal_attach_does_not_trigger_initial_timeout_shutdown() -> TestResult<()> {
    let tmp = unique_temp_dir("e2e-connect").await?;
    let (cert_path, key_path) = write_self_signed_cert_pair(&tmp).await?;
    let xdg_runtime_dir = unique_short_runtime_dir("co").await?;
    let port_range = unique_port_range("co")?;
    fs::create_dir_all(&xdg_runtime_dir).await?;

    let mut child = spawn_neoshd(
        &cert_path,
        &key_path,
        &xdg_runtime_dir,
        300,
        600,
        &port_range,
    )?;
    let stdout = child.stdout.take().ok_or("missing child stdout")?;
    let stderr = child.stderr.take().ok_or("missing child stderr")?;
    let stderr_task = tokio::spawn(collect_stderr(stderr));

    let mut stdout_lines = BufReader::new(stdout).lines();
    let bootstrap_line = match tokio::time::timeout(Duration::from_secs(5), stdout_lines.next_line()).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => {
            let _ = child.kill().await;
            let stderr_log = stderr_task.await??;
            return Err(format!("missing bootstrap line; stderr={stderr_log}").into());
        }
        Ok(Err(e)) => {
            let _ = child.kill().await;
            let stderr_log = stderr_task.await??;
            return Err(format!("bootstrap stdout read error: {e}; stderr={stderr_log}").into());
        }
        Err(_) => {
            let _ = child.kill().await;
            let stderr_log = stderr_task.await??;
            return Err(format!("bootstrap stdout timeout; stderr={stderr_log}").into());
        }
    };
    let bootstrap: Value = serde_json::from_str(&bootstrap_line)?;
    let session_id = bootstrap["session_id"]
        .as_str()
        .ok_or("bootstrap.session_id missing")?;
    let auth_token = bootstrap["auth_token"]
        .as_str()
        .ok_or("bootstrap.auth_token missing")?;
    let quic_addr = bootstrap["quic_addr"]
        .as_str()
        .ok_or("bootstrap.quic_addr missing")?;

    let endpoint = build_client_endpoint(&cert_path).await?;
    let server_addr = quic_addr.parse()?;
    let connection = endpoint
        .connect(server_addr, "localhost")?
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let (mut send, mut recv) = connection.open_bi().await?;

    write_control(&mut send, json!({"type":"HELLO"})).await?;
    let hello_ack = read_control(&mut recv).await?;
    assert_eq!(hello_ack["type"], "HELLO_ACK");

    write_control(&mut send, json!({"type":"AUTH", "token": auth_token})).await?;
    let auth_ok = read_control(&mut recv).await?;
    assert_eq!(auth_ok["type"], "AUTH_OK");

    write_control(&mut send, json!({"type":"ATTACH", "session_id": session_id})).await?;
    let attach_ok = read_control(&mut recv).await?;
    assert_eq!(attach_ok["type"], "ATTACH_OK");

    write_control(&mut send, json!({"type":"CLOSE"})).await?;
    send.flush().await?;
    let _ = send.finish();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(recv);
    drop(send);
    drop(connection);
    drop(endpoint);

    let status = match tokio::time::timeout(Duration::from_secs(8), child.wait()).await {
        Ok(res) => res?,
        Err(_) => {
            let _ = child.kill().await;
            let stderr_log = stderr_task.await??;
            return Err(format!("neoshd did not stop after CLOSE; stderr={stderr_log}").into());
        }
    };
    let stderr_log = stderr_task.await??;

    assert!(status.success(), "neoshd exited with non-zero status: {status}");
    assert!(
        !stderr_log.contains("\"event\":\"initial_attach_timeout\""),
        "unexpected initial_attach_timeout log: {stderr_log}"
    );
    assert!(
        !stderr_log.contains("\"event\":\"server_auto_shutdown\"")
            || !stderr_log.contains("\"reason\":\"initial_attach_timeout\""),
        "unexpected server_auto_shutdown(initial_attach_timeout): {stderr_log}"
    );
    assert!(
        stderr_log.contains("\"event\":\"server_stop\"")
            && stderr_log.contains("\"reason\":\"session_terminated\""),
        "missing server_stop(reason=session_terminated): {stderr_log}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_full_session_lifecycle_attach_detach_resume_close() -> TestResult<()> {
    let tmp = unique_temp_dir("e2e-lifecycle").await?;
    let (cert_path, key_path) = write_self_signed_cert_pair(&tmp).await?;
    let xdg_runtime_dir = unique_short_runtime_dir("lf").await?;
    let port_range = unique_port_range("lf")?;
    fs::create_dir_all(&xdg_runtime_dir).await?;

    let mut child = spawn_neoshd(
        &cert_path,
        &key_path,
        &xdg_runtime_dir,
        300,
        600,
        &port_range,
    )?;
    let stdout = child.stdout.take().ok_or("missing child stdout")?;
    let stderr = child.stderr.take().ok_or("missing child stderr")?;
    let stderr_task = tokio::spawn(collect_stderr(stderr));

    let mut stdout_lines = BufReader::new(stdout).lines();
    let bootstrap_line =
        match tokio::time::timeout(Duration::from_secs(5), stdout_lines.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => {
                let _ = child.kill().await;
                let stderr_log = stderr_task.await??;
                return Err(format!("missing bootstrap line; stderr={stderr_log}").into());
            }
            Ok(Err(e)) => {
                let _ = child.kill().await;
                let stderr_log = stderr_task.await??;
                return Err(format!("bootstrap stdout read error: {e}; stderr={stderr_log}").into());
            }
            Err(_) => {
                let _ = child.kill().await;
                let stderr_log = stderr_task.await??;
                return Err(format!("bootstrap stdout timeout; stderr={stderr_log}").into());
            }
        };
    let bootstrap: Value = serde_json::from_str(&bootstrap_line)?;
    let session_id = bootstrap["session_id"]
        .as_str()
        .ok_or("bootstrap.session_id missing")?
        .to_string();
    let first_auth_token = bootstrap["auth_token"]
        .as_str()
        .ok_or("bootstrap.auth_token missing")?
        .to_string();
    let first_quic_addr = bootstrap["quic_addr"]
        .as_str()
        .ok_or("bootstrap.quic_addr missing")?
        .to_string();

    let endpoint1 = build_client_endpoint(&cert_path).await?;
    let server_addr1 = first_quic_addr.parse()?;
    let conn1 = endpoint1
        .connect(server_addr1, "localhost")?
        .await
        .map_err(|e| format!("connect #1 failed: {e}"))?;
    let (mut send1, mut recv1) = conn1.open_bi().await?;

    write_control(&mut send1, json!({"type":"HELLO"})).await?;
    let hello_ack1 = read_control(&mut recv1).await?;
    assert_eq!(hello_ack1["type"], "HELLO_ACK");

    write_control(&mut send1, json!({"type":"AUTH", "token": first_auth_token})).await?;
    let auth_ok1 = read_control(&mut recv1).await?;
    assert_eq!(auth_ok1["type"], "AUTH_OK");

    write_control(&mut send1, json!({"type":"ATTACH", "session_id": session_id})).await?;
    let attach_ok = read_control(&mut recv1).await?;
    assert_eq!(attach_ok["type"], "ATTACH_OK");

    write_control(&mut send1, json!({"type":"DETACH"})).await?;
    send1.flush().await?;
    let _ = send1.finish();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(recv1);
    drop(send1);
    drop(conn1);
    endpoint1.close(0u32.into(), b"detach done");

    let renewed = run_renew_auth(&session_id, &xdg_runtime_dir).await?;
    let second_auth_token = renewed["auth_token"]
        .as_str()
        .ok_or("renewed.auth_token missing")?;
    let second_quic_addr = renewed["quic_addr"]
        .as_str()
        .ok_or("renewed.quic_addr missing")?;
    assert_eq!(second_quic_addr, first_quic_addr);

    let endpoint2 = build_client_endpoint(&cert_path).await?;
    let server_addr2 = second_quic_addr.parse()?;
    let conn2 = endpoint2
        .connect(server_addr2, "localhost")?
        .await
        .map_err(|e| format!("connect #2 failed: {e}"))?;
    let (mut send2, mut recv2) = conn2.open_bi().await?;

    write_control(&mut send2, json!({"type":"HELLO"})).await?;
    let hello_ack2 = read_control(&mut recv2).await?;
    assert_eq!(hello_ack2["type"], "HELLO_ACK");

    write_control(&mut send2, json!({"type":"AUTH", "token": second_auth_token})).await?;
    let auth_ok2 = read_control(&mut recv2).await?;
    assert_eq!(auth_ok2["type"], "AUTH_OK");
    let resume_token = auth_ok2["resume_token"]
        .as_str()
        .ok_or("AUTH_OK.resume_token missing")?;

    write_control(
        &mut send2,
        json!({"type":"RESUME", "session_id": session_id, "resume_token": resume_token}),
    )
    .await?;
    let resume_ok = read_control(&mut recv2).await?;
    assert_eq!(resume_ok["type"], "RESUME_OK");

    write_control(&mut send2, json!({"type":"CLOSE"})).await?;
    send2.flush().await?;
    let _ = send2.finish();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(recv2);
    drop(send2);
    drop(conn2);
    endpoint2.close(0u32.into(), b"close done");

    let status = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(res) => res?,
        Err(_) => {
            let _ = child.kill().await;
            let stderr_log = stderr_task.await??;
            return Err(
                format!("neoshd did not stop after full lifecycle; stderr={stderr_log}").into(),
            );
        }
    };
    let stderr_log = stderr_task.await??;

    assert!(status.success(), "neoshd exited with non-zero status: {status}");
    assert!(
        stderr_log.contains("\"event\":\"attach_ok\""),
        "missing attach_ok log: {stderr_log}"
    );
    assert!(
        stderr_log.contains("\"event\":\"resume_ok\""),
        "missing resume_ok log: {stderr_log}"
    );
    assert!(
        stderr_log.contains("\"event\":\"server_stop\"")
            && stderr_log.contains("\"reason\":\"session_terminated\""),
        "missing server_stop(reason=session_terminated): {stderr_log}"
    );
    assert!(
        !stderr_log.contains("\"event\":\"initial_attach_timeout\""),
        "unexpected initial_attach_timeout in lifecycle path: {stderr_log}"
    );
    Ok(())
}

fn spawn_neoshd(
    cert_path: &Path,
    key_path: &Path,
    xdg_runtime_dir: &Path,
    initial_attach_timeout: u64,
    session_timeout: u64,
    port_range: &str,
) -> TestResult<Child> {
    let bin = std::env::var("CARGO_BIN_EXE_neoshd")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/neoshd"));
    let mut cmd = Command::new(&bin);
    cmd.arg("new")
        .arg("--user")
        .arg("alice")
        .arg("--bind-server")
        .arg("127.0.0.1")
        .arg("--port-range")
        .arg(port_range)
        .arg("--tls-cert")
        .arg(cert_path)
        .arg("--tls-key")
        .arg(key_path)
        .arg("--initial-attach-timeout")
        .arg(initial_attach_timeout.to_string())
        .arg("--session-timeout")
        .arg(session_timeout.to_string())
        .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    Ok(cmd.spawn()?)
}

async fn run_renew_auth(session_id: &str, xdg_runtime_dir: &Path) -> TestResult<Value> {
    let bin = std::env::var("CARGO_BIN_EXE_neoshd")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/neoshd"));
    let output = Command::new(bin)
        .arg("renew-auth")
        .arg("--session-id")
        .arg(session_id)
        .arg("--user")
        .arg("alice")
        .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
        .output()
        .await?;
    if !output.status.success() {
        return Err(format!(
            "renew-auth failed: status={:?}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    Ok(value)
}

async fn build_client_endpoint(cert_path: &Path) -> TestResult<Endpoint> {
    let cert_pem = fs::read(cert_path).await?;
    let mut reader = Cursor::new(cert_pem);
    let certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()?;
    let cert = certs.first().ok_or("empty cert file")?.clone();

    let mut roots = RootCertStore::empty();
    roots.add(cert)?;
    let mut rustls_cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    rustls_cfg.alpn_protocols = vec![b"neosh/1".to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(rustls_cfg)?;
    let client_config = ClientConfig::new(Arc::new(quic_crypto));

    let mut endpoint = Endpoint::client("127.0.0.1:0".parse()?)?;
    endpoint.set_default_client_config(client_config);
    Ok(endpoint)
}

async fn write_control(send: &mut SendStream, payload: Value) -> TestResult<()> {
    let payload = serde_json::to_vec(&payload)?;
    let frame = neoshd::protocol::framing::encode_frame(&payload);
    send.write_all(&frame).await?;
    Ok(())
}

async fn read_control(recv: &mut RecvStream) -> TestResult<Value> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload).await?;
    Ok(serde_json::from_slice(&payload)?)
}

async fn collect_stderr(stderr: ChildStderr) -> TestResult<String> {
    let mut reader = BufReader::new(stderr);
    let mut out = String::new();
    reader.read_to_string(&mut out).await?;
    Ok(out)
}

async fn write_self_signed_cert_pair(base_dir: &Path) -> TestResult<(PathBuf, PathBuf)> {
    let cert = generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();

    let cert_path = base_dir.join("server.crt");
    let key_path = base_dir.join("server.key");
    fs::write(&cert_path, cert_pem).await?;
    fs::write(&key_path, key_pem).await?;
    Ok((cert_path, key_path))
}

async fn unique_temp_dir(prefix: &str) -> TestResult<PathBuf> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("neosh-{prefix}-{pid}-{now}"));
    fs::create_dir_all(&dir).await?;
    Ok(dir)
}

async fn unique_short_runtime_dir(prefix: &str) -> TestResult<PathBuf> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() % 1_000_000;
    let pid = std::process::id();
    let dir = PathBuf::from("/tmp").join(format!("neosh-{prefix}-{pid}-{now}"));
    fs::create_dir_all(&dir).await?;
    Ok(dir)
}

fn unique_port_range(tag: &str) -> TestResult<String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let mut seed = (now as u64) ^ (std::process::id() as u64);
    for b in tag.as_bytes() {
        seed = seed.wrapping_mul(131).wrapping_add(*b as u64);
    }
    let base = 30000u16 + (seed % 30000) as u16;
    let end = base.saturating_add(200).min(65535);
    Ok(format!("{base}:{end}"))
}
