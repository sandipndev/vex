mod daemon;
mod session;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

fn default_socket_path() -> PathBuf {
    let home = dirs::home_dir().expect("could not determine home directory");
    home.join(".vex").join("vexd.sock")
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
}

fn resolve_target_and_token(
    connect: Option<SocketAddr>,
    token: Option<String>,
    socket_path: &std::path::Path,
) -> Result<(session::Target, String)> {
    match connect {
        Some(addr) => {
            let token = token.ok_or_else(|| {
                anyhow::anyhow!("--token or VEX_TOKEN is required when using --connect")
            })?;
            Ok((session::Target::Tcp(addr), token))
        }
        None => {
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
    }
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
        Some(Command::Create { shell }) => {
            let (target, token) = resolve_target_and_token(cli.connect, cli.token, &socket_path)?;
            session::session_create(&target, &token, shell).await?;
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
