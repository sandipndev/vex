mod auth;
mod local;
mod server;
mod state;

use std::{
    fs::{File, OpenOptions},
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use daemonize::Daemonize;
use qrcode::QrCode;

// ── CLI definition ─────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "vexd", about = "Vex daemon — manages agent work streams")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the daemon
    Start(StartArgs),
    /// Stop a running daemon
    Stop,
    /// Restart the daemon (stop then start)
    Restart(StartArgs),
    /// Print daemon status
    Status,
    /// Tail the daemon log
    Logs,
    /// Create a new pairing token and display a QR code
    Pair(PairArgs),
    /// Manage pairing tokens
    Tokens {
        #[command(subcommand)]
        action: TokensCmd,
    },
    /// Print shell completion script
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
}

#[derive(Args, Clone)]
struct StartArgs {
    /// Detach and run in the background
    #[arg(long)]
    daemon: bool,
}

#[derive(Args)]
struct PairArgs {
    /// Human-readable label for this token
    #[arg(long)]
    label: Option<String>,
    /// Token lifetime in seconds (omit = never expires)
    #[arg(long)]
    expire: Option<u64>,
}

#[derive(Subcommand)]
enum TokensCmd {
    /// List all paired tokens
    List,
    /// Revoke a token by ID, or all tokens with --all
    Revoke {
        /// Token ID to revoke
        id: Option<String>,
        /// Revoke every token
        #[arg(long)]
        all: bool,
    },
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn vexd_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".vexd"))
}

fn admin_socket_path() -> Result<PathBuf> {
    Ok(vexd_dir()?.join("vexd.sock"))
}

// ── Entry point ─────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Start(args) => setup_and_launch(args),

        Commands::Restart(args) => {
            if let Err(e) = do_stop() {
                eprintln!("stop: {e}");
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            setup_and_launch(args)
        }

        Commands::Stop => do_stop(),

        Commands::Status => tokio::runtime::Runtime::new()?.block_on(async {
            let sock = admin_socket_path()?;
            match local::send_command(&sock, &vex_proto::Command::Status).await? {
                vex_proto::Response::DaemonStatus(s) => {
                    println!("vexd v{}", s.version);
                    println!("uptime:  {}s", s.uptime_secs);
                    println!("clients: {}", s.connected_clients);
                }
                other => println!("{other:?}"),
            }
            Ok(())
        }),

        Commands::Logs => {
            let log_path = vexd_dir()?.join("vexd.log");
            if !log_path.exists() {
                anyhow::bail!("Log file not found: {}", log_path.display());
            }
            std::process::Command::new("tail")
                .arg("-f")
                .arg(&log_path)
                .status()
                .context("failed to run tail")?;
            Ok(())
        }

        Commands::Pair(args) => tokio::runtime::Runtime::new()?.block_on(async {
            let sock = admin_socket_path()?;
            let cmd = vex_proto::Command::PairCreate {
                label: args.label.clone(),
                expire_secs: args.expire,
            };
            match local::send_command(&sock, &cmd).await? {
                vex_proto::Response::Pair(p) => {
                    println!("Token ID    : {}", p.token_id);
                    println!("Token secret: {}", p.token_secret);
                    let pairing = p.pairing_string();
                    println!("\nPairing string:\n  {pairing}");
                    println!("\nQR code:");
                    print_qr(&pairing);
                    Ok(())
                }
                vex_proto::Response::Error(e) => anyhow::bail!("Error: {e:?}"),
                other => anyhow::bail!("Unexpected response: {other:?}"),
            }
        }),

        Commands::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "vexd", &mut std::io::stdout());
            Ok(())
        }

        Commands::Tokens { action } => tokio::runtime::Runtime::new()?.block_on(async {
            let sock = admin_socket_path()?;
            match action {
                TokensCmd::List => {
                    match local::send_command(&sock, &vex_proto::Command::PairList).await? {
                        vex_proto::Response::PairedClients(clients) => {
                            if clients.is_empty() {
                                println!("No paired tokens.");
                            }
                            for c in &clients {
                                let label = c.label.as_deref().unwrap_or("(no label)");
                                let expires = c.expires_at.as_deref().unwrap_or("never");
                                let seen = c.last_seen.as_deref().unwrap_or("never");
                                println!(
                                    "{:20} {:<20} expires={} last_seen={}",
                                    c.token_id, label, expires, seen
                                );
                            }
                        }
                        other => println!("{other:?}"),
                    }
                }
                TokensCmd::Revoke { id, all } => {
                    if *all {
                        match local::send_command(&sock, &vex_proto::Command::PairRevokeAll).await?
                        {
                            vex_proto::Response::Revoked(n) => {
                                println!("Revoked {n} token(s).")
                            }
                            other => println!("{other:?}"),
                        }
                    } else if let Some(token_id) = id {
                        match local::send_command(
                            &sock,
                            &vex_proto::Command::PairRevoke {
                                id: token_id.clone(),
                            },
                        )
                        .await?
                        {
                            vex_proto::Response::Ok => println!("Revoked {token_id}."),
                            vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                            other => println!("{other:?}"),
                        }
                    } else {
                        anyhow::bail!("Provide a token ID or --all");
                    }
                }
            }
            Ok(())
        }),
    }
}

// ── Daemon launch (shared by Start and Restart) ──────────────────────────────

fn setup_and_launch(args: &StartArgs) -> Result<()> {
    let vexd_dir = vexd_dir()?;
    std::fs::create_dir_all(&vexd_dir)?;
    std::fs::create_dir_all(vexd_dir.join("tls"))?;

    if args.daemon {
        let log = open_log(&vexd_dir)?;
        Daemonize::new()
            .pid_file(vexd_dir.join("vexd.pid"))
            .stdout(log.try_clone()?)
            .stderr(log)
            .start()
            .context("daemonize failed")?;
        // We are now in the daemon child process.
    } else {
        std::fs::write(vexd_dir.join("vexd.pid"), std::process::id().to_string())
            .context("writing PID file")?;
    }

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("Failed to install rustls crypto provider"))?;

    tokio::runtime::Runtime::new()?.block_on(run_daemon(vexd_dir))
}

// ── Daemon server loop ──────────────────────────────────────────────────────

async fn run_daemon(vexd_dir: PathBuf) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let token_store = auth::TokenStore::load(vexd_dir.join("tokens.json"))?;
    let state = state::AppState::new(vexd_dir.clone(), token_store);

    let socket_path = state.socket_path();
    let tcp_port: u16 = std::env::var("VEXD_TCP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(vex_proto::DEFAULT_TCP_PORT);
    let tcp_addr: SocketAddr = format!("0.0.0.0:{tcp_port}")
        .parse()
        .context("invalid TCP address")?;
    let tls_dir = vexd_dir.join("tls");

    match server::tcp::cert_fingerprint(&tls_dir) {
        Ok(fp) => tracing::info!("TLS cert fingerprint (blake3): {fp}"),
        Err(e) => tracing::warn!("Could not read cert fingerprint: {e}"),
    }

    let unix_handle = tokio::spawn(server::unix::serve_unix(state.clone(), socket_path));
    let tcp_handle = tokio::spawn(server::tcp::serve_tcp(state.clone(), tcp_addr, tls_dir));

    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate())?;

    tracing::info!("vexd started (pid {})", std::process::id());

    tokio::select! {
        res = unix_handle => {
            match res {
                Ok(Ok(())) => tracing::info!("Unix server stopped"),
                Ok(Err(e)) => tracing::error!("Unix server error: {e}"),
                Err(e) => tracing::error!("Unix server panicked: {e}"),
            }
        }
        res = tcp_handle => {
            match res {
                Ok(Ok(())) => tracing::info!("TCP server stopped"),
                Ok(Err(e)) => tracing::error!("TCP server error: {e}"),
                Err(e) => tracing::error!("TCP server panicked: {e}"),
            }
        }
        _ = sigterm.recv() => tracing::info!("SIGTERM received, shutting down"),
        _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT received, shutting down"),
    }

    Ok(())
}

// ── Stop helper ─────────────────────────────────────────────────────────────

fn do_stop() -> Result<()> {
    let pid_file = vexd_dir()?.join("vexd.pid");
    if !pid_file.exists() {
        println!("vexd does not appear to be running (no PID file).");
        return Ok(());
    }
    let pid_str = std::fs::read_to_string(&pid_file)?;
    let pid = pid_str.trim().to_string();

    let status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(&pid)
        .status()
        .context("failed to run kill")?;

    // Remove PID file regardless; if kill succeeded, process is gone
    if let Err(e) = std::fs::remove_file(&pid_file) {
        tracing::warn!("Could not remove PID file: {e}");
    }

    if status.success() {
        println!("Sent SIGTERM to vexd (PID {pid}).");
    } else {
        eprintln!("kill returned non-zero for PID {pid}; process may already be gone.");
    }
    Ok(())
}

// ── Misc helpers ────────────────────────────────────────────────────────────

fn open_log(vexd_dir: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(vexd_dir.join("vexd.log"))
        .context("opening log file")
}

fn print_qr(data: &str) {
    match QrCode::new(data.as_bytes()) {
        Ok(code) => {
            let image = code
                .render::<qrcode::render::unicode::Dense1x2>()
                .quiet_zone(true)
                .build();
            println!("{image}");
        }
        Err(e) => eprintln!("Could not generate QR code: {e}"),
    }
}
