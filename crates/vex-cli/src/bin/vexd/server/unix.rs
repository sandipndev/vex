use vex_cli as vex_proto;

use std::{path::PathBuf, sync::Arc};
use tokio::net::UnixListener;
use vex_proto::Transport;

use crate::state::AppState;

pub async fn serve_unix(state: Arc<AppState>, socket_path: PathBuf) -> anyhow::Result<()> {
    // Remove stale socket file
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!("Unix socket listening at {}", socket_path.display());

    loop {
        let (stream, _) = listener.accept().await?;
        let state = state.clone();

        tokio::spawn(async move {
            state.increment_clients();
            if let Err(e) =
                crate::server::handle_connection(stream, state.clone(), Transport::Unix, None).await
            {
                tracing::warn!("Unix connection error: {e}");
            }
            state.decrement_clients();
        });
    }
}
