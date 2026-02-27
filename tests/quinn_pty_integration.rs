use std::sync::Arc;

use neoshd::terminal::pty::PtyRuntime;
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rcgen::generate_simple_self_signed;
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quinn_loopback_handshake_skeleton() {
    let cert = generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_der = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

    let server_config = ServerConfig::with_single_cert(vec![cert_der.clone()], key_der).unwrap();

    let mut roots = RootCertStore::empty();
    roots.add(cert_der).unwrap();
    let client_config = ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();

    let server_endpoint = Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let server_addr = server_endpoint.local_addr().unwrap();

    let mut client_endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client_endpoint.set_default_client_config(client_config);

    let connecting = client_endpoint.connect(server_addr, "localhost").unwrap();
    let server_task = tokio::spawn(async move {
        let incoming = server_endpoint.accept().await.expect("incoming connection");
        incoming.await.expect("server connect ok")
    });
    let client_task = tokio::spawn(async move { connecting.await.expect("client connect ok") });

    let (server_res, client_res) = tokio::join!(server_task, client_task);
    let server_conn = server_res.expect("server task join");
    let client_conn = client_res.expect("client task join");

    assert_eq!(client_conn.remote_address(), server_addr);
    assert_eq!(server_conn.remote_address(), client_endpoint.local_addr().unwrap());

    client_endpoint.close(0u32.into(), b"test done");
}

#[test]
fn portable_pty_spawn_skeleton() {
    let mut runtime = PtyRuntime::new(24, 80).expect("create pty runtime");
    let output = runtime
        .run_shell_capture("printf neosh-pty-ok")
        .expect("run shell and capture output");

    assert!(output.contains("neosh-pty-ok"));
}
