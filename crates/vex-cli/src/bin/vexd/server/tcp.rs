use vex_cli as vex_proto;

use std::{net::SocketAddr, path::Path, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use vex_proto::Transport;

use crate::state::AppState;

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn ensure_tls_certs(
    tls_dir: &Path,
) -> anyhow::Result<(
    Vec<rustls::pki_types::CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
)> {
    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");

    if cert_path.exists() && key_path.exists() {
        let cert_pem = std::fs::read(&cert_path)?;
        let key_pem = std::fs::read(&key_path)?;
        let certs =
            rustls_pemfile::certs(&mut cert_pem.as_slice()).collect::<Result<Vec<_>, _>>()?;
        let key = rustls_pemfile::private_key(&mut key_pem.as_slice())?
            .ok_or_else(|| anyhow::anyhow!("No private key found in {}", key_path.display()))?;
        Ok((certs, key))
    } else {
        std::fs::create_dir_all(tls_dir)?;
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();
        std::fs::write(&cert_path, &cert_pem)?;
        std::fs::write(&key_path, &key_pem)?;

        let certs =
            rustls_pemfile::certs(&mut cert_pem.as_bytes()).collect::<Result<Vec<_>, _>>()?;
        let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())?
            .ok_or_else(|| anyhow::anyhow!("Failed to parse generated private key"))?;
        Ok((certs, key))
    }
}

pub async fn serve_tcp(
    state: Arc<AppState>,
    addr: SocketAddr,
    tls_dir: PathBuf,
) -> anyhow::Result<()> {
    let (certs, key) = ensure_tls_certs(&tls_dir)?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("TCP TLS listener on {addr}");

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let state = state.clone();

        tokio::spawn(async move {
            match acceptor.accept(stream).await {
                Ok(mut tls_stream) => {
                    // Pre-command auth
                    let auth: vex_proto::AuthToken =
                        match vex_proto::framing::recv(&mut tls_stream).await {
                            Ok(a) => a,
                            Err(e) => {
                                tracing::warn!("Auth read error from {peer_addr}: {e}");
                                return;
                            }
                        };

                    // Log fingerprint of the connecting cert (none for mutual TLS, but we log peer)
                    let token_id = {
                        let mut store = state.token_store.lock().await;
                        if store.validate(&auth.token_id, &auth.token_secret) {
                            Some(auth.token_id.clone())
                        } else {
                            None
                        }
                    };

                    if token_id.is_none() {
                        let _ = vex_proto::framing::send(
                            &mut tls_stream,
                            &vex_proto::Response::Error(vex_proto::VexProtoError::Unauthorized),
                        )
                        .await;
                        tracing::warn!("Rejected auth from {peer_addr} (token: {})", auth.token_id);
                        return;
                    }

                    // Auth OK
                    if let Err(e) =
                        vex_proto::framing::send(&mut tls_stream, &vex_proto::Response::Pong).await
                    {
                        tracing::warn!("Failed to send auth pong to {peer_addr}: {e}");
                        return;
                    }

                    tracing::info!(
                        "Authenticated TCP connection from {peer_addr} (token: {})",
                        token_id.as_deref().unwrap_or("?")
                    );

                    state.increment_clients();
                    if let Err(e) = crate::server::handle_connection(
                        tls_stream,
                        state.clone(),
                        Transport::Tcp,
                        token_id,
                    )
                    .await
                    {
                        tracing::warn!("TCP connection error from {peer_addr}: {e}");
                    }
                    state.decrement_clients();
                }
                Err(e) => {
                    tracing::warn!("TLS accept error from {peer_addr}: {e}");
                }
            }
        });
    }
}

/// Compute blake3 fingerprint of a PEM cert file (for display/TOFU).
pub fn cert_fingerprint(tls_dir: &Path) -> anyhow::Result<String> {
    let cert_pem = std::fs::read(tls_dir.join("cert.pem"))?;
    let certs: Vec<_> =
        rustls_pemfile::certs(&mut cert_pem.as_slice()).collect::<Result<Vec<_>, _>>()?;
    let first = certs
        .first()
        .ok_or_else(|| anyhow::anyhow!("No cert found"))?;
    let fp = blake3::hash(first.as_ref());
    Ok(bytes_to_hex(fp.as_bytes()))
}
