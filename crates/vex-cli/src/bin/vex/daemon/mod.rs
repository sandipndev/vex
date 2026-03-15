mod agent;
pub mod config;
mod handler;
mod repo;
mod session;
mod workstream;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tracing::{error, info};

use agent::{new_agent_store, spawn_detection_task};
use config::VexConfig;
use repo::new_repo_store;
use session::SessionManager;
use workstream::new_workstream_store;

pub async fn run(port: u16, vex_dir: &Path) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    info!("daemon listening on 127.0.0.1:{}", port);

    let manager = Arc::new(SessionManager::new());
    let agent_store = new_agent_store();
    let repo_store = new_repo_store(vex_dir);
    let workstream_store = new_workstream_store(vex_dir);
    let config = Arc::new(VexConfig::load(vex_dir));

    // Start agent detection background task
    spawn_detection_task(Arc::clone(&manager), Arc::clone(&agent_store));

    // Signal handler for graceful shutdown
    let manager_signal = Arc::clone(&manager);
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
        manager_signal.kill_all().await;
        let _ = std::fs::remove_file(&pid_path);
        std::process::exit(0);
    });

    // Accept loop
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                info!("new connection from {}", addr);
                let manager = Arc::clone(&manager);
                let agent_store = Arc::clone(&agent_store);
                let repo_store = Arc::clone(&repo_store);
                let workstream_store = Arc::clone(&workstream_store);
                let config = Arc::clone(&config);
                tokio::spawn(async move {
                    handler::handle_connection(
                        stream,
                        manager,
                        agent_store,
                        repo_store,
                        workstream_store,
                        config,
                    )
                    .await;
                });
            }
            Err(e) => {
                error!("accept error: {}", e);
            }
        }
    }
}
