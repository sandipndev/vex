mod daemon;
mod session;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

fn default_socket_path() -> PathBuf {
    let home = dirs::home_dir().expect("could not determine home directory");
    home.join(".vex").join("vexd.sock")
}

#[derive(Serialize, Deserialize)]
struct SavedConnection {
    addr: SocketAddr,
    token: String,
}

fn connect_file_for(socket_path: &Path) -> PathBuf {
    socket_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("connect.json")
}

fn load_saved_connection(socket_path: &Path) -> Option<SavedConnection> {
    let path = connect_file_for(socket_path);
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_connection(socket_path: &Path, conn: &SavedConnection) -> Result<()> {
    let path = connect_file_for(socket_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string(conn)?;
    std::fs::write(&path, &data)?;
    // Restrict permissions since this file contains a token
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn remove_connection(socket_path: &Path) -> Result<()> {
    let path = connect_file_for(socket_path);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[derive(Parser)]
#[command(name = "vex", about = "Vex terminal multiplexer")]
struct Cli {
    /// Path to the vexd Unix socket
    #[arg(long, env = "VEX_SOCKET")]
    socket: Option<PathBuf>,

    /// Auth token (defaults to reading <socket_dir>/vexd.token)
    #[arg(long, env = "VEX_TOKEN")]
    token: Option<String>,

    /// Connect to a remote daemon over TCP (e.g. 192.168.1.5:9090)
    #[arg(long, env = "VEX_CONNECT")]
    connect: Option<SocketAddr>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the daemon
    Daemon {
        /// Also listen on a TCP address for remote clients (e.g. 0.0.0.0:9090)
        #[arg(long)]
        listen: Option<SocketAddr>,
    },
    /// Create a new session
    Create {
        /// Shell to use (defaults to $SHELL or /bin/sh)
        #[arg(long)]
        shell: Option<String>,
        /// Attach to the session immediately after creating it
        #[arg(short, long)]
        attach: bool,
    },
    /// List active sessions
    #[command(alias = "ls")]
    List,
    /// Attach to a session
    Attach {
        /// Session ID or unique prefix
        id: String,
    },
    /// Kill a session
    Kill {
        /// Session ID or unique prefix
        id: String,
    },
    /// Save a remote connection as the default target
    Connect {
        /// Remote daemon address (e.g. 192.168.1.5:9090)
        addr: SocketAddr,
        /// Auth token for the remote daemon
        #[arg(long)]
        token: String,
    },
    /// Remove the saved remote connection, reverting to local
    Disconnect,
}

fn resolve_target_and_token(
    connect: Option<SocketAddr>,
    token: Option<String>,
    socket_path: &Path,
) -> Result<(session::Target, String)> {
    // 1. Explicit --connect flag takes highest priority
    if let Some(addr) = connect {
        let token = token.ok_or_else(|| {
            anyhow::anyhow!("--token or VEX_TOKEN is required when using --connect")
        })?;
        return Ok((session::Target::Tcp(addr), token));
    }

    // 2. Saved connection from connect.json
    if let Some(saved) = load_saved_connection(socket_path) {
        let token = token.unwrap_or(saved.token);
        return Ok((session::Target::Tcp(saved.addr), token));
    }

    // 3. Local Unix socket
    let token = match token {
        Some(t) => t,
        None => {
            let token_path = socket_path.with_extension("token");
            std::fs::read_to_string(&token_path).map_err(|e| {
                anyhow::anyhow!(
                    "could not read token from {}: {} (is the daemon running?)",
                    token_path.display(),
                    e
                )
            })?
        }
    };
    Ok((session::Target::Unix(socket_path.to_path_buf()), token))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let socket_path = cli.socket.unwrap_or_else(default_socket_path);

    match cli.command {
        Some(Command::Daemon { listen }) => {
            daemon::run(&socket_path, listen).await?;
        }
        Some(Command::Connect { addr, token }) => {
            let conn = SavedConnection {
                addr,
                token: token.clone(),
            };
            // Try to verify the connection
            let target = session::Target::Tcp(addr);
            match session::session_list(&target, &token).await {
                Ok(()) => {
                    save_connection(&socket_path, &conn)?;
                    eprintln!("connected to {} (verified)", addr);
                }
                Err(e) => {
                    save_connection(&socket_path, &conn)?;
                    eprintln!("warning: could not verify connection: {}", e);
                    eprintln!("saved connection to {} (unverified)", addr);
                }
            }
        }
        Some(Command::Disconnect) => {
            remove_connection(&socket_path)?;
            eprintln!("disconnected; using local daemon");
        }
        Some(Command::Create { shell, attach }) => {
            let (target, token) = resolve_target_and_token(cli.connect, cli.token, &socket_path)?;
            let id = session::session_create(&target, &token, shell).await?;
            if attach {
                session::session_attach(&target, &token, &id).await?;
            }
        }
        Some(Command::Attach { id }) => {
            let (target, token) = resolve_target_and_token(cli.connect, cli.token, &socket_path)?;
            session::session_attach(&target, &token, &id).await?;
        }
        Some(Command::Kill { id }) => {
            let (target, token) = resolve_target_and_token(cli.connect, cli.token, &socket_path)?;
            session::session_kill(&target, &token, &id).await?;
        }
        Some(Command::List) | None => {
            let (target, token) = resolve_target_and_token(cli.connect, cli.token, &socket_path)?;
            session::session_list(&target, &token).await?;
        }
    }

    Ok(())
}
