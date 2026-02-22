use vex_cli as vex_proto;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use tower_http::cors::CorsLayer;
use vex_proto::{Command, Response, Transport, VexProtoError};

use crate::state::AppState;

#[derive(serde::Deserialize)]
struct CommandRequest {
    command: Command,
}

pub async fn serve_http(
    state: Arc<AppState>,
    addr: SocketAddr,
    tls_dir: PathBuf,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/command", post(handle_command))
        .layer(CorsLayer::very_permissive())
        .with_state(state);

    let cert_path = tls_dir.join("cert.pem");
    let key_path = tls_dir.join("key.pem");

    let tls_config =
        axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path).await?;

    tracing::info!("HTTPS listener on {addr}");

    axum_server::bind_rustls(addr, tls_config)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

async fn handle_command(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<CommandRequest>,
) -> (StatusCode, axum::Json<Response>) {
    // Parse Authorization: Bearer tok_id:secret
    let Some(token_id) = authenticate(&headers, &state).await else {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(Response::Error(VexProtoError::Unauthorized)),
        );
    };

    let response = super::dispatch(body.command, &state, &Transport::Tcp, &Some(token_id)).await;

    let status = match &response {
        Response::Error(VexProtoError::Unauthorized) => StatusCode::UNAUTHORIZED,
        Response::Error(VexProtoError::LocalOnly) => StatusCode::FORBIDDEN,
        Response::Error(VexProtoError::NotFound) => StatusCode::NOT_FOUND,
        Response::Error(VexProtoError::Internal(_)) => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::OK,
    };

    (status, axum::Json(response))
}

async fn authenticate(headers: &HeaderMap, state: &Arc<AppState>) -> Option<String> {
    let auth_header = headers.get("authorization")?.to_str().ok()?;
    let bearer = auth_header.strip_prefix("Bearer ")?;

    let (token_id, token_secret) = bearer.split_once(':')?;
    if token_id.is_empty() || token_secret.is_empty() {
        return None;
    }

    let mut store = state.token_store.lock().await;
    if store.validate(token_id, token_secret) {
        Some(token_id.to_string())
    } else {
        None
    }
}
