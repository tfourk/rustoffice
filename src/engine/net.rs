//! TCP / TLS plumbing shared by IMAP and the auto-discovery probes.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + std::fmt::Debug> AsyncStream for T {}

pub type BoxStream = Box<dyn AsyncStream>;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn roots() -> Arc<RootCertStore> {
    static ROOTS: OnceLock<Arc<RootCertStore>> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut store = RootCertStore::empty();
            store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let native = rustls_native_certs::load_native_certs();
            let mut added = 0;
            for cert in native.certs {
                if store.add(cert).is_ok() {
                    added += 1;
                }
            }
            log::debug!("loaded {added} native root certificates");
            Arc::new(store)
        })
        .clone()
}

pub fn tls_config(accept_invalid: bool) -> Arc<ClientConfig> {
    let builder = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("default TLS versions are supported");
    let cfg = if accept_invalid {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider())))
            .with_no_client_auth()
    } else {
        builder.with_root_certificates(roots()).with_no_client_auth()
    };
    Arc::new(cfg)
}

pub async fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .map_err(|_| anyhow!("Timed out connecting to {host}:{port}"))?
        .with_context(|| format!("Could not connect to {host}:{port}"))?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

pub async fn tls_wrap<S>(
    stream: S,
    host: &str,
    accept_invalid: bool,
) -> Result<tokio_rustls::client::TlsStream<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let connector = tokio_rustls::TlsConnector::from(tls_config(accept_invalid));
    let name = ServerName::try_from(host.to_string())
        .map_err(|_| anyhow!("Invalid server name \"{host}\""))?;
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(name, stream))
        .await
        .map_err(|_| anyhow!("TLS handshake with {host} timed out"))?
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("certificate") || msg.contains("UnknownIssuer") {
                anyhow!(
                    "The server's security certificate is not trusted ({msg}). \
                     If this is your own server, enable \"Allow untrusted certificates\"."
                )
            } else {
                anyhow!("TLS error: {msg}")
            }
        })?;
    Ok(tls)
}

/// Accepts any certificate. Only used when the user explicitly allows
/// untrusted certificates for an account (self-hosted servers, local bridges).
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Quick reachability probe used by auto-discovery.
pub async fn probe(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(4), TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}
