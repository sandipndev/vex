mod daemon;
mod session;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
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
}

fn load_saved_connection(vex_dir: &Path) -> Option<SavedConnection> {
    let path = vex_dir.join("connect.json");
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

#[derive(Parser)]
#[command(name = "vex", about = "Vex terminal multiplexer")]
struct Cli {
    /// Daemon port
    #[arg(long, env = "VEX_PORT", default_value_t = DEFAULT_PORT)]
    port: u16,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the daemon
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
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
    /// Save a remote SSH connection as the default target
    Connect {
        /// SSH destination (e.g. user@host or an SSH config name)
        host: String,
    },
    /// Remove the saved remote connection, reverting to local
    Disconnect,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the daemon in the background
    Start,
    /// Stop the running daemon
    Stop,
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

// ── SSH forwarding ───────────────────────────────────────────────

fn ssh_args_for(cmd: &Option<Command>) -> (Vec<String>, bool) {
    let mut args = Vec::new();
    let mut needs_tty = false;
    match cmd {
        Some(Command::Create { shell, attach }) => {
            args.push("create".into());
            if let Some(s) = shell {
                args.extend(["--shell".into(), s.clone()]);
            }
            if *attach {
                args.push("--attach".into());
                needs_tty = true;
            }
        }
        Some(Command::Attach { id }) => {
            args.extend(["attach".into(), id.clone()]);
            needs_tty = true;
        }
        Some(Command::Kill { id }) => {
            args.extend(["kill".into(), id.clone()]);
        }
        Some(Command::List) | None => {
            args.push("list".into());
        }
        _ => unreachable!(),
    }
    (args, needs_tty)
}

fn exec_via_ssh(host: &str, args: &[String], needs_tty: bool) -> ! {
    let mut cmd = std::process::Command::new("ssh");
    if needs_tty {
        cmd.arg("-t");
    }
    cmd.arg(host).arg("vex").args(args);
    match cmd.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("ssh error: {}", e);
            std::process::exit(1);
        }
    }
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
            nix::unistd::setsid()
                .map_err(std::io::Error::other)?;
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

// ── Connect / Disconnect ─────────────────────────────────────────

fn connect_ssh(vex_dir: &Path, host: &str) -> Result<()> {
    std::fs::create_dir_all(vex_dir)?;

    // Try to verify
    let verified = std::process::Command::new("ssh")
        .args(["-o", "ConnectTimeout=5", "-o", "BatchMode=yes"])
        .arg(host)
        .args(["vex", "list"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    let conn = SavedConnection {
        host: host.to_string(),
    };
    let data = serde_json::to_string(&conn)?;
    std::fs::write(vex_dir.join("connect.json"), &data)?;

    if verified {
        eprintln!("connected to {} (verified)", host);
    } else {
        eprintln!(
            "warning: could not verify connection to {}\nsaved connection (unverified)",
            host
        );
    }
    Ok(())
}

fn disconnect_ssh(vex_dir: &Path) -> Result<()> {
    let path = vex_dir.join("connect.json");
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    eprintln!("disconnected; using local daemon");
    Ok(())
}

// ── Main ─────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let port = cli.port;
    let vex_dir = vex_dir();

    // Phase 1: always-local commands
    match &cli.command {
        Some(Command::Daemon { command }) => {
            return match command {
                DaemonCommand::Start => daemon_start(&vex_dir, port),
                DaemonCommand::Stop => daemon_stop(&vex_dir),
                DaemonCommand::Logs { follow } => daemon_logs(&vex_dir, *follow),
                DaemonCommand::Run => {
                    tracing_subscriber::fmt::init();
                    daemon::run(port, &vex_dir).await
                }
            };
        }
        Some(Command::Connect { host }) => return connect_ssh(&vex_dir, host),
        Some(Command::Disconnect) => {
            disconnect_ssh(&vex_dir)?;
            return Ok(());
        }
        _ => {}
    }

    // Phase 2: check for SSH forwarding
    if let Some(saved) = load_saved_connection(&vex_dir) {
        let (args, needs_tty) = ssh_args_for(&cli.command);
        exec_via_ssh(&saved.host, &args, needs_tty);
    }

    // Phase 3: local commands
    match cli.command {
        Some(Command::Create { shell, attach }) => {
            let id = session::session_create(port, shell).await?;
            if attach {
                session::session_attach(port, &id).await?;
            }
        }
        Some(Command::Attach { id }) => {
            session::session_attach(port, &id).await?;
        }
        Some(Command::Kill { id }) => {
            session::session_kill(port, &id).await?;
        }
        Some(Command::List) | None => {
            session::session_list(port).await?;
        }
        _ => unreachable!(),
    }

    Ok(())
}
