mod agent;
mod client;
mod daemon;
mod repo;
mod session;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

const DEFAULT_PORT: u16 = 6969;

fn vex_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("VEX_DIR") {
        PathBuf::from(dir)
    } else {
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(".vex")
    }
}

#[derive(Serialize, Deserialize)]
struct SavedConnection {
    host: String,
    tunnel_port: u16,
}

fn load_saved_connection(vex_dir: &Path) -> Option<SavedConnection> {
    let path = vex_dir.join("connect.json");
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

#[derive(Parser)]
#[command(name = "vex", about = "Vex terminal multiplexer", version)]
struct Cli {
    /// Daemon port
    #[arg(long, env = "VEX_PORT", default_value_t = DEFAULT_PORT)]
    port: u16,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Manage sessions
    #[command(alias = "s")]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Manage the daemon
    #[command(alias = "d")]
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Manage remote connections
    #[command(alias = "r")]
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    /// Manage Claude Code agents
    #[command(alias = "a")]
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Manage repositories
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Create a new session
    Create {
        /// Shell to use (defaults to $SHELL or /bin/sh)
        #[arg(long)]
        shell: Option<String>,
        /// Attach to the session immediately after creating it
        #[arg(short, long)]
        attach: bool,
        /// Create session at a named repo's working directory
        #[arg(short = 'r', long = "repo")]
        repo: Option<String>,
    },
    /// List active sessions
    #[command(alias = "ls")]
    List,
    /// Kill a session
    Kill {
        /// Session ID or unique prefix
        id: String,
    },
    /// Attach to a session
    Attach {
        /// Session ID or unique prefix
        id: String,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the daemon in the background
    Start,
    /// Stop the running daemon
    Stop,
    /// Show daemon status
    Status,
    /// Show daemon logs
    Logs {
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },
    /// Run the daemon (internal)
    #[command(hide = true)]
    Run,
}

#[derive(Subcommand)]
enum AgentCommand {
    /// List detected Claude Code agents
    #[command(alias = "ls")]
    List,
    /// Show agents that need human intervention
    #[command(alias = "notif")]
    Notifications,
    /// Watch a Claude Code agent's conversation
    Watch {
        /// Vex session ID or unique prefix
        id: String,
        /// Show thinking blocks
        #[arg(long)]
        show_thinking: bool,
    },
    /// Spawn a Claude Code agent in a repo
    Spawn {
        /// Repository name
        #[arg(short = 'r', long = "repo")]
        repo: String,
        /// Attach to the session immediately
        #[arg(short, long)]
        attach: bool,
    },
    /// Send a prompt to a Claude Code agent
    Prompt {
        /// Vex session ID or unique prefix
        id: String,
        /// Prompt text to send
        text: String,
        /// Watch the conversation after sending the prompt
        #[arg(short, long)]
        watch: bool,
        /// Show thinking blocks (requires --watch)
        #[arg(long)]
        show_thinking: bool,
    },
}

#[derive(Subcommand)]
enum RemoteCommand {
    /// Connect to a remote daemon via SSH tunnel
    Connect {
        /// SSH destination (e.g. user@host or an SSH config name)
        host: String,
    },
    /// Disconnect from the remote daemon
    Disconnect,
    /// Show current remote connection
    #[command(alias = "ls")]
    List,
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Register a named repository
    Add {
        /// Repository name
        name: String,
        /// Path to the repository root
        path: PathBuf,
    },
    /// Unregister a repository
    Remove {
        /// Repository name
        name: String,
    },
    /// List registered repositories
    #[command(alias = "ls")]
    List,
    /// Introspect a path for repository information
    IntrospectPath {
        /// Path to introspect
        path: PathBuf,
    },
}

// ── Daemon management ────────────────────────────────────────────

fn daemon_start(vex_dir: &Path, port: u16) -> Result<()> {
    std::fs::create_dir_all(vex_dir)?;

    // Check if already running
    let pid_path = vex_dir.join("daemon.pid");
    if let Ok(pid_str) = std::fs::read_to_string(&pid_path)
        && let Ok(pid) = pid_str.trim().parse::<i32>()
        && kill(Pid::from_raw(pid), None).is_ok()
    {
        eprintln!("daemon already running (pid {})", pid);
        return Ok(());
    }

    let log_path = vex_dir.join("daemon.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_err = log_file.try_clone()?;

    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--port")
        .arg(port.to_string())
        .arg("daemon")
        .arg("run")
        .stdout(log_file)
        .stderr(log_err)
        .stdin(std::process::Stdio::null())
        .env("VEX_DIR", vex_dir.as_os_str());

    // Detach from terminal session so daemon survives terminal close
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(std::io::Error::other)?;
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    let pid = child.id();

    std::fs::write(&pid_path, pid.to_string())?;

    // Wait for port to be ready
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            eprintln!("daemon started on port {} (pid {})", port, pid);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Timeout — clean up
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
    let _ = std::fs::remove_file(&pid_path);
    bail!(
        "daemon failed to start within 5s (check {})",
        log_path.display()
    );
}

fn daemon_stop(vex_dir: &Path) -> Result<()> {
    let pid_path = vex_dir.join("daemon.pid");
    let pid_str = std::fs::read_to_string(&pid_path)
        .map_err(|_| anyhow::anyhow!("no daemon running (no pid file)"))?;
    let pid = Pid::from_raw(pid_str.trim().parse()?);

    if kill(pid, None).is_err() {
        let _ = std::fs::remove_file(&pid_path);
        eprintln!("daemon not running (cleaned up stale pid file)");
        return Ok(());
    }

    kill(pid, Signal::SIGTERM).map_err(|e| anyhow::anyhow!("failed to stop daemon: {}", e))?;

    for _ in 0..50 {
        if kill(pid, None).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let _ = std::fs::remove_file(&pid_path);
    eprintln!("daemon stopped");
    Ok(())
}

fn daemon_status(vex_dir: &Path, port: u16) -> Result<()> {
    let pid_path = vex_dir.join("daemon.pid");
    if let Ok(pid_str) = std::fs::read_to_string(&pid_path)
        && let Ok(pid) = pid_str.trim().parse::<i32>()
        && kill(Pid::from_raw(pid), None).is_ok()
    {
        eprintln!("daemon running (pid {}, port {})", pid, port);
    } else {
        eprintln!("daemon not running");
    }
    Ok(())
}

fn daemon_logs(vex_dir: &Path, follow: bool) -> Result<()> {
    let log_path = vex_dir.join("daemon.log");
    if !log_path.exists() {
        bail!("no log file found (has the daemon been started?)");
    }
    if follow {
        let status = std::process::Command::new("tail")
            .arg("-f")
            .arg(&log_path)
            .status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
    let content = std::fs::read_to_string(&log_path)?;
    print!("{content}");
    Ok(())
}

// ── Connect / Disconnect (SSH tunnel) ────────────────────────────

fn find_free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn connect_ssh(vex_dir: &Path, host: &str, remote_port: u16) -> Result<()> {
    std::fs::create_dir_all(vex_dir)?;

    // Disconnect existing tunnel if any
    if load_saved_connection(vex_dir).is_some() {
        let _ = disconnect_ssh(vex_dir);
    }

    let tunnel_port = find_free_port()?;
    let ssh_sock = vex_dir.join("ssh.sock");

    // Start SSH tunnel with control socket for lifecycle management
    let status = std::process::Command::new("ssh")
        .args([
            "-f",
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=60",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ControlMaster=yes",
            "-o",
            &format!("ControlPath={}", ssh_sock.display()),
            "-L",
            &format!("{}:127.0.0.1:{}", tunnel_port, remote_port),
            host,
        ])
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run ssh: {} (is OpenSSH installed?)", e))?;

    if !status.success() {
        bail!("failed to establish SSH tunnel to {}", host);
    }

    // Brief wait for tunnel to be fully ready
    std::thread::sleep(Duration::from_millis(500));

    // Verify remote daemon is reachable through tunnel
    let verified = std::net::TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], tunnel_port)),
        Duration::from_secs(2),
    )
    .is_ok();

    // Save connection
    let conn = SavedConnection {
        host: host.to_string(),
        tunnel_port,
    };
    let data = serde_json::to_string(&conn)?;
    std::fs::write(vex_dir.join("connect.json"), &data)?;

    if verified {
        eprintln!("connected to {}", host);
    } else {
        eprintln!("tunnel to {} established", host);
        eprintln!("note: remote daemon not reachable — run `vex daemon start` on the remote");
    }

    Ok(())
}

fn disconnect_ssh(vex_dir: &Path) -> Result<()> {
    let ssh_sock = vex_dir.join("ssh.sock");

    if let Some(saved) = load_saved_connection(vex_dir) {
        // Kill SSH tunnel via control socket
        let _ = std::process::Command::new("ssh")
            .args([
                "-O",
                "exit",
                "-o",
                &format!("ControlPath={}", ssh_sock.display()),
                &saved.host,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    let _ = std::fs::remove_file(&ssh_sock);

    let path = vex_dir.join("connect.json");
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    eprintln!("disconnected; using local daemon");
    Ok(())
}

fn remote_list(vex_dir: &Path) -> Result<()> {
    if let Some(conn) = load_saved_connection(vex_dir) {
        println!("{} (tunnel port {})", conn.host, conn.tunnel_port);
    } else {
        println!("not connected to any remote");
    }
    Ok(())
}

// ── Main ─────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let port = cli.port;
    let vex_dir = vex_dir();

    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            Cli::command().print_help()?;
            return Ok(());
        }
    };

    // Phase 1: always-local commands
    match &command {
        Command::Daemon { command } => {
            return match command {
                DaemonCommand::Start => daemon_start(&vex_dir, port),
                DaemonCommand::Stop => daemon_stop(&vex_dir),
                DaemonCommand::Status => daemon_status(&vex_dir, port),
                DaemonCommand::Logs { follow } => daemon_logs(&vex_dir, *follow),
                DaemonCommand::Run => {
                    tracing_subscriber::fmt::init();
                    daemon::run(port, &vex_dir).await
                }
            };
        }
        Command::Remote { command } => {
            return match command {
                RemoteCommand::Connect { host } => connect_ssh(&vex_dir, host, port),
                RemoteCommand::Disconnect => disconnect_ssh(&vex_dir),
                RemoteCommand::List => remote_list(&vex_dir),
            };
        }
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "vex", &mut std::io::stdout());
            return Ok(());
        }
        _ => {}
    }

    // Phase 2: determine effective port (local daemon or SSH tunnel)
    let effective_port = load_saved_connection(&vex_dir)
        .map(|c| c.tunnel_port)
        .unwrap_or(port);

    // Phase 3: commands routed through effective port
    match command {
        Command::Session { command } => match command {
            SessionCommand::Create {
                shell,
                attach,
                repo,
            } => {
                let (target_port, resolved_repo) =
                    resolve_repo_for_create(repo, effective_port, port, &vex_dir).await?;
                let id = session::session_create(target_port, shell, resolved_repo).await?;
                if attach {
                    session::session_attach(target_port, &id).await?;
                }
            }
            SessionCommand::List => {
                session::session_list(effective_port).await?;
            }
            SessionCommand::Kill { id } => {
                session::session_kill(effective_port, &id).await?;
            }
            SessionCommand::Attach { id } => {
                session::session_attach(effective_port, &id).await?;
            }
        },
        Command::Agent { command } => match command {
            AgentCommand::List => {
                agent::agent_list(effective_port).await?;
            }
            AgentCommand::Notifications => {
                agent::agent_notifications(effective_port).await?;
            }
            AgentCommand::Watch { id, show_thinking } => {
                agent::agent_watch(effective_port, &id, show_thinking).await?;
            }
            AgentCommand::Prompt {
                id,
                text,
                watch,
                show_thinking,
            } => {
                agent::agent_prompt(effective_port, &id, &text, watch, show_thinking).await?;
            }
            AgentCommand::Spawn { repo, attach } => {
                let (target_port, resolved_repo) =
                    resolve_repo_for_create(Some(repo), effective_port, port, &vex_dir).await?;
                let resolved_repo = resolved_repo.expect("repo was Some");
                let id = agent::agent_spawn(target_port, &resolved_repo).await?;
                if attach {
                    session::session_attach(target_port, &id).await?;
                }
            }
        },
        Command::Repo { command } => match command {
            RepoCommand::Add { name, path } => {
                repo::repo_add(effective_port, &name, &path).await?;
            }
            RepoCommand::Remove { name } => {
                repo::repo_remove(effective_port, &name).await?;
            }
            RepoCommand::List => {
                repo::repo_list(effective_port).await?;
            }
            RepoCommand::IntrospectPath { path } => {
                repo::repo_introspect_path(effective_port, &path).await?;
            }
        },
        _ => unreachable!(),
    }

    Ok(())
}

/// Resolve a repo name for session creation, handling local/remote disambiguation.
///
/// Returns (port_to_use, resolved_repo_name).
async fn resolve_repo_for_create(
    repo: Option<String>,
    effective_port: u16,
    local_port: u16,
    vex_dir: &Path,
) -> Result<(u16, Option<String>)> {
    let Some(repo_name) = repo else {
        return Ok((effective_port, None));
    };

    // Check for qualified name: "local/name" or "<host>/name"
    if let Some((qualifier, name)) = repo_name.split_once('/') {
        if qualifier == "local" {
            return Ok((local_port, Some(name.to_string())));
        }
        // Check if qualifier matches the remote host
        if let Some(conn) = load_saved_connection(vex_dir)
            && conn.host == qualifier
        {
            return Ok((conn.tunnel_port, Some(name.to_string())));
        }
        bail!(
            "unknown qualifier '{}' — use 'local/<name>' or '<remote-host>/<name>'",
            qualifier
        );
    }

    // Unqualified name — check if remote is connected
    let remote = load_saved_connection(vex_dir);
    if remote.is_none() {
        // No remote, just use effective port
        return Ok((effective_port, Some(repo_name)));
    }

    let conn = remote.unwrap();

    // Query both local and remote for this repo name
    let local_has = query_repo_exists(local_port, &repo_name)
        .await
        .unwrap_or(false);
    let remote_has = query_repo_exists(conn.tunnel_port, &repo_name)
        .await
        .unwrap_or(false);

    match (local_has, remote_has) {
        (true, true) => bail!(
            "ambiguous repo '{}' — exists on both local and '{}'. Use 'local/{}' or '{}/{}'",
            repo_name,
            conn.host,
            repo_name,
            conn.host,
            repo_name,
        ),
        (true, false) => Ok((local_port, Some(repo_name))),
        (false, true) => Ok((conn.tunnel_port, Some(repo_name))),
        (false, false) => bail!("repo '{}' not found on local or '{}'", repo_name, conn.host),
    }
}

async fn query_repo_exists(port: u16, name: &str) -> Result<bool> {
    use vex_cli::proto::{ClientMessage, ServerMessage};
    let resp = client::request(port, &ClientMessage::RepoList).await?;
    match resp {
        ServerMessage::Repos { repos } => Ok(repos.iter().any(|r| r.name == name)),
        _ => Ok(false),
    }
}
