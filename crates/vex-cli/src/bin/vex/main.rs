mod daemon;
mod session;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

fn default_socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("VEX_SOCKET") {
        return PathBuf::from(path);
    }
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

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the daemon
    Daemon,
    /// Manage sessions
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// Create a new session
    Create {
        /// Shell to use (defaults to $SHELL or /bin/sh)
        #[arg(long)]
        shell: Option<String>,
    },
    /// List active sessions
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let socket_path = cli.socket.unwrap_or_else(default_socket_path);

    match cli.command {
        Command::Daemon => {
            daemon::run(&socket_path).await?;
        }
        Command::Session { action } => {
            let token = match cli.token {
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
            match action {
                SessionAction::Create { shell } => {
                    session::session_create(&socket_path, &token, shell).await?;
                }
                SessionAction::List => {
                    session::session_list(&socket_path, &token).await?;
                }
                SessionAction::Attach { id } => {
                    session::session_attach(&socket_path, &token, &id).await?;
                }
                SessionAction::Kill { id } => {
                    session::session_kill(&socket_path, &token, &id).await?;
                }
            }
        }
    }

    Ok(())
}
