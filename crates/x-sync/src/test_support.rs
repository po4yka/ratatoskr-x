//! A loopback HTTPS server whose certificate is issued by a throwaway CA. Compiled only under
//! `test-support`, which no service binary enables.
//!
//! It lets the public-capture client be tested end to end, including the success leg, on a host
//! whose trust store knows nothing about the fake (XR-021 CONTRACTS.md section R3-15).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::{ServerConfig, crypto::aws_lc_rs};

/// The failure of building or starting the fake.
pub type FakeError = Box<dyn std::error::Error + Send + Sync>;

/// How long the leaf certificate stays valid. Short on purpose: platform verifiers refuse
/// long-lived server certificates.
const LEAF_VALIDITY: Duration = Duration::from_hours(720);

/// The largest request head the fake reads.
const MAX_REQUEST_HEAD: usize = 16 * 1024;

/// One HTTPS server on `127.0.0.1` that answers every request with the same JSON body.
#[derive(Debug)]
pub struct TlsFake {
    address: SocketAddr,
    ca_pem: String,
    failed_handshakes: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl TlsFake {
    /// Starts the server. Every completed request is answered `200` with `body` as JSON.
    ///
    /// # Errors
    /// When a certificate cannot be generated or the loopback listener cannot bind.
    pub async fn start(body: impl Into<String>) -> Result<Self, FakeError> {
        let body: Arc<str> = Arc::from(body.into());
        let (ca_pem, server_config) = issue()?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let failed_handshakes = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let (failed, served) = (Arc::clone(&failed_handshakes), Arc::clone(&requests));
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_connection(
                    acceptor.clone(),
                    stream,
                    Arc::clone(&body),
                    Arc::clone(&failed),
                    Arc::clone(&served),
                ));
            }
        });
        Ok(Self {
            address,
            ca_pem,
            failed_handshakes,
            requests,
            task,
        })
    }

    /// The base URL of the server, `https://127.0.0.1:<port>`.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("https://{}", self.address)
    }

    /// The PEM of the throwaway CA that issued the server certificate.
    #[must_use]
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    /// TLS handshakes the server refused or that a client abandoned, such as an untrusted
    /// certificate.
    #[must_use]
    pub fn failed_handshakes(&self) -> usize {
        self.failed_handshakes.load(Ordering::SeqCst)
    }

    /// Requests that completed the handshake and were answered.
    #[must_use]
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for TlsFake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One accepted connection: a TLS handshake, one request head, one canned answer.
async fn serve_connection(
    acceptor: TlsAcceptor,
    stream: TcpStream,
    body: Arc<str>,
    failed: Arc<AtomicUsize>,
    served: Arc<AtomicUsize>,
) {
    let Ok(mut tls) = acceptor.accept(stream).await else {
        failed.fetch_add(1, Ordering::SeqCst);
        return;
    };
    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < MAX_REQUEST_HEAD {
        match tls.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => head.extend_from_slice(chunk.get(..read).unwrap_or_default()),
        }
    }
    served.fetch_add(1, Ordering::SeqCst);
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tls.write_all(response.as_bytes()).await;
    let _ = tls.shutdown().await;
}

/// Generates a CA, a `127.0.0.1` server certificate it issues, and the server configuration.
fn issue() -> Result<(String, ServerConfig), FakeError> {
    let ca_key = KeyPair::generate()?;
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Ratatoskr X test CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.not_before = (SystemTime::now() - Duration::from_hours(24)).into();
    ca_params.not_after = (SystemTime::now() + LEAF_VALIDITY).into();
    let ca_certificate = ca_params.self_signed(&ca_key)?;
    let issuer = Issuer::new(ca_params, ca_key);

    let leaf_key = KeyPair::generate()?;
    let mut leaf_params = CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "127.0.0.1");
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.not_before = (SystemTime::now() - Duration::from_hours(24)).into();
    leaf_params.not_after = (SystemTime::now() + LEAF_VALIDITY).into();
    let leaf = leaf_params.signed_by(&leaf_key, &issuer)?;

    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    let chain: Vec<CertificateDer<'static>> = vec![leaf.der().clone()];
    let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(chain, key)?;
    Ok((ca_certificate.pem(), config))
}
