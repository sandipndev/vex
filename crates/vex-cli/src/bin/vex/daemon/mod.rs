mod handler;
mod session;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::{TcpListener, UnixListener};
use tracing::{error, info};
use uuid::Uuid;

use session::SessionManager;

pub async fn run(socket_path: &Path, listen_addr: Option<SocketAddr>) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Remove stale socket
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }

    // Generate auth token and write to file
    let token = Arc::new(Uuid::new_v4().to_string());
    let token_path = socket_path.with_extension("token");
    std::fs::write(&token_path, token.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600))?;
    }
    info!("auth token written to {}", token_path.display());

    let unix_listener = UnixListener::bind(socket_path)?;
    info!("vex daemon listening on {}", socket_path.display());

    let tcp_listener = match listen_addr {
        Some(addr) => {
            let listener = TcpListener::bind(addr).await?;
            info!("vex daemon listening on tcp://{}", addr);
            Some(listener)
        }
        None => None,
    };

    let manager = Arc::new(SessionManager::new());

    // Signal handler for graceful shutdown
    let manager_signal = Arc::clone(&manager);
    let socket_path_signal = socket_path.to_owned();
    let token_path_signal = token_path.clone();
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
        let _ = std::fs::remove_file(&token_path_signal);
        std::process::exit(0);
    });

    // Accept loop
    loop {
        tokio::select! {
            result = unix_listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        info!("new unix client connection");
                        let manager = Arc::clone(&manager);
                        let token = Arc::clone(&token);
                        tokio::spawn(async move {
                            handler::handle_connection(stream, manager, token).await;
                        });
                    }
                    Err(e) => {
                        error!("unix accept error: {}", e);
                    }
                }
            }
            result = async {
                match &tcp_listener {
                    Some(l) => l.accept().await,
                    None => std::future::pending().await,
                }
            } => {
                match result {
                    Ok((stream, addr)) => {
                        info!("new tcp client connection from {}", addr);
                        let manager = Arc::clone(&manager);
                        let token = Arc::clone(&token);
                        tokio::spawn(async move {
                            handler::handle_connection(stream, manager, token).await;
                        });
                    }
                    Err(e) => {
                        error!("tcp accept error: {}", e);
                    }
                }
            }
        }
    }
}
