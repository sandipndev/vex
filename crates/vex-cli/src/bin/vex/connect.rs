use vex_cli as vex_proto;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, client::danger::HandshakeSignatureValid};
use tokio::net::{TcpStream, UnixStream};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use vex_proto::framing;

use crate::config::ConnectionEntry;

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── TOFU certificate verifier ────────────────────────────────────────────────

#[derive(Debug)]
struct TofuVerifier {
    /// Fingerprint we expect (from saved config), None on first connect.
    expected_fingerprint: Option<String>,
    /// Captured during handshake so the caller can persist it.
    seen_fingerprint: Mutex<Option<String>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl TofuVerifier {
    fn new(expected: Option<String>) -> Self {
        Self {
            expected_fingerprint: expected,
            seen_fingerprint: Mutex::new(None),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }
}

impl rustls::client::danger::ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let fp = bytes_to_hex(blake3::hash(end_entity.as_ref()).as_bytes());

        *self.seen_fingerprint.lock().unwrap() = Some(fp.clone());

        if let Some(expected) = &self.expected_fingerprint
            && *expected != fp
        {
            return Err(rustls::Error::General(format!(
                "TLS certificate fingerprint mismatch!\n  expected: {expected}\n  got:      {fp}\n\
                 If you trust the new certificate, delete the connection entry and reconnect."
            )));
        }

        Ok(rustls::client::danger::ServerCertVerified::assertion())
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
            &self.provider.signature_verification_algorithms,
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
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ── Connection enum ──────────────────────────────────────────────────────────

pub enum Connection {
    Unix(UnixStream),
    Tcp(Box<TlsStream<TcpStream>>),
}

impl Connection {
    /// Connect using a `ConnectionEntry` from config.
    ///
    /// If this is a TCP connection and no fingerprint is pinned yet (first TOFU
    /// connect), `entry.tls_fingerprint` is updated in-place.  The caller is
    /// responsible for persisting the config if it changed.
    pub async fn from_entry(entry: &mut ConnectionEntry) -> Result<Self> {
        match entry.transport.as_str() {
            "tcp" => {
                let host = entry
                    .tcp_host
                    .clone()
                    .context("connection entry missing tcp_host")?;
                let token_id = entry
                    .token_id
                    .clone()
                    .context("connection entry missing token_id")?;
                let token_secret = entry
                    .token_secret
                    .clone()
                    .context("connection entry missing token_secret")?;
                let existing_fp = entry.tls_fingerprint.clone();
                let (conn, new_fp) =
                    Self::tcp_connect(&host, &token_id, &token_secret, existing_fp).await?;
                if let Some(fp) = new_fp {
                    tracing_print(&format!("TLS fingerprint pinned: {fp}"));
                    entry.tls_fingerprint = Some(fp);
                }
                Ok(conn)
            }
            _ => {
                // unix (default)
                let path = entry.unix_socket.as_deref().unwrap_or("/tmp/vexd.sock");
                let stream = UnixStream::connect(path).await.with_context(|| {
                    format!("Could not connect to vexd at {path} — is vexd running?")
                })?;
                Ok(Connection::Unix(stream))
            }
        }
    }

    /// Perform a TCP TLS connection with auth.
    ///
    /// Returns `(Connection, Option<new_fingerprint>)` — the fingerprint is
    /// `Some` only when it was learned for the first time (TOFU first connect).
    pub async fn tcp_connect(
        host: &str,
        token_id: &str,
        token_secret: &str,
        existing_fingerprint: Option<String>,
    ) -> Result<(Self, Option<String>)> {
        let (hostname, port) = parse_host_port(host)?;

        let verifier = Arc::new(TofuVerifier::new(existing_fingerprint.clone()));
        let verifier_ref = verifier.clone();

        let client_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();

        let connector = TlsConnector::from(Arc::new(client_config));
        let stream = TcpStream::connect((hostname.as_str(), port))
            .await
            .with_context(|| format!("TCP connect to {hostname}:{port} failed"))?;

        let server_name: ServerName<'static> = ServerName::try_from(hostname.clone())
            .map_err(|e| anyhow::anyhow!("Invalid server name '{hostname}': {e}"))?;

        let mut tls_stream = connector
            .connect(server_name, stream)
            .await
            .context("TLS handshake failed")?;

        // Capture new fingerprint if we didn't have one
        let new_fingerprint = if existing_fingerprint.is_none() {
            verifier_ref.seen_fingerprint.lock().unwrap().clone()
        } else {
            None
        };

        // Pre-command auth
        let auth = vex_proto::AuthToken {
            token_id: token_id.to_string(),
            token_secret: token_secret.to_string(),
        };
        framing::send(&mut tls_stream, &auth).await?;

        let response: vex_proto::Response = framing::recv(&mut tls_stream).await?;
        match response {
            vex_proto::Response::Pong => {}
            vex_proto::Response::Error(e) => {
                anyhow::bail!("Authentication failed: {e:?}")
            }
            other => anyhow::bail!("Unexpected auth response: {other:?}"),
        }

        Ok((Connection::Tcp(Box::new(tls_stream)), new_fingerprint))
    }

    pub fn is_unix(&self) -> bool {
        matches!(self, Connection::Unix(_))
    }

    pub async fn send<T: serde::Serialize>(&mut self, msg: &T) -> Result<()> {
        match self {
            Connection::Unix(s) => framing::send(s, msg).await.map_err(Into::into),
            Connection::Tcp(s) => framing::send(s, msg).await.map_err(Into::into),
        }
    }

    pub async fn recv<T: for<'de> serde::Deserialize<'de>>(&mut self) -> Result<T> {
        match self {
            Connection::Unix(s) => framing::recv(s).await.map_err(Into::into),
            Connection::Tcp(s) => framing::recv(s).await.map_err(Into::into),
        }
    }
}

// ── Utilities ────────────────────────────────────────────────────────────────

/// eprintln wrapper used for TOFU messages so they appear even when stdout is piped.
fn tracing_print(msg: &str) {
    eprintln!("{msg}");
}

fn parse_host_port(host: &str) -> Result<(String, u16)> {
    if let Some((h, p)) = host.rsplit_once(':') {
        let port: u16 = p
            .parse()
            .with_context(|| format!("invalid port in '{host}'"))?;
        Ok((h.to_string(), port))
    } else {
        Ok((host.to_string(), 7422))
    }
}
