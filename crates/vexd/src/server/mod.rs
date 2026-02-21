use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use vex_proto::{Command, Response, Transport, VexProtoError};

use crate::state::AppState;

pub mod tcp;
pub mod unix;

pub async fn handle_connection<S>(
    mut stream: S,
    state: Arc<AppState>,
    transport: Transport,
    token_id: Option<String>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let cmd: Command = match vex_proto::framing::recv(&mut stream).await {
            Ok(c) => c,
            Err(_) => break, // connection closed or broken
        };

        let response = dispatch(cmd, &state, &transport, &token_id).await;
        vex_proto::framing::send(&mut stream, &response).await?;
    }
    Ok(())
}

async fn dispatch(
    cmd: Command,
    state: &Arc<AppState>,
    transport: &Transport,
    token_id: &Option<String>,
) -> Response {
    match cmd {
        Command::Status => Response::DaemonStatus(vex_proto::DaemonStatus {
            uptime_secs: state.uptime_secs(),
            connected_clients: state.connected_clients(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),

        Command::Whoami => Response::ClientInfo(vex_proto::ClientInfo {
            token_id: token_id.clone(),
            is_local: matches!(transport, Transport::Unix),
        }),

        Command::PairCreate { label, expire_secs } => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            let mut store = state.token_store.lock().await;
            match store.generate(label, expire_secs) {
                Ok((token, secret)) => Response::Pair(vex_proto::PairPayload {
                    token_id: token.token_id,
                    token_secret: secret,
                    host: None,
                }),
                Err(e) => Response::Error(VexProtoError::Internal(e.to_string())),
            }
        }

        Command::PairList => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            let store = state.token_store.lock().await;
            let clients = store
                .list()
                .iter()
                .map(|t| vex_proto::PairedClient {
                    token_id: t.token_id.clone(),
                    label: t.label.clone(),
                    created_at: t.created_at.to_rfc3339(),
                    expires_at: t.expires_at.map(|dt| dt.to_rfc3339()),
                    last_seen: t.last_seen.map(|dt| dt.to_rfc3339()),
                })
                .collect();
            Response::PairedClients(clients)
        }

        Command::PairRevoke { id } => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            let mut store = state.token_store.lock().await;
            if store.revoke(&id) {
                Response::Ok
            } else {
                Response::Error(VexProtoError::NotFound)
            }
        }

        Command::PairRevokeAll => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            let mut store = state.token_store.lock().await;
            let count = store.revoke_all();
            Response::Revoked(count as u32)
        }
    }
}
