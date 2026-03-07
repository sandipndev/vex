mod handler;
mod session;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixListener;
use tracing::{error, info};

use session::SessionManager;

pub async fn run(socket_path: &Path) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Remove stale socket
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    info!("vex daemon listening on {}", socket_path.display());

    let manager = Arc::new(SessionManager::new());

    // Signal handler for graceful shutdown
    let manager_signal = Arc::clone(&manager);
    let socket_path_signal = socket_path.to_owned();
    tokio::spawn(async move {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        let mut sigint =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();

        tokio::select! {
            _ = sigterm.recv() => {
                info!("received SIGTERM");
            }
            _ = sigint.recv() => {
                info!("received SIGINT");
            }
        }

        info!("shutting down...");
        manager_signal.kill_all().await;
        let _ = std::fs::remove_file(&socket_path_signal);
        std::process::exit(0);
    });

    // Accept loop
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                info!("new client connection");
                let manager = Arc::clone(&manager);
                tokio::spawn(async move {
                    handler::handle_connection(stream, manager).await;
                });
            }
            Err(e) => {
                error!("accept error: {}", e);
            }
        }
    }
}
