mod agent;
mod handler;
mod session;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tracing::{error, info};

use agent::AgentManager;
use session::SessionManager;

pub async fn run(port: u16, vex_dir: &Path) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    info!("daemon listening on 127.0.0.1:{}", port);

    let session_manager = Arc::new(SessionManager::new());
    let agent_manager = Arc::new(AgentManager::new());

    // Signal handler for graceful shutdown
    let session_manager_signal = Arc::clone(&session_manager);
    let pid_path = vex_dir.join("daemon.pid");
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
        session_manager_signal.kill_all().await;
        let _ = std::fs::remove_file(&pid_path);
        std::process::exit(0);
    });

    // Accept loop
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("new connection from {}", addr);
                let session_manager = Arc::clone(&session_manager);
                let agent_manager = Arc::clone(&agent_manager);
                tokio::spawn(async move {
                    handler::handle_connection(stream, session_manager, agent_manager).await;
                });
            }
            Err(e) => {
                error!("accept error: {}", e);
            }
        }
    }
}
