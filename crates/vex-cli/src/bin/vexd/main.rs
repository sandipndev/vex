use vex_cli as vex_proto;

mod auth;
mod local;
mod repo_store;
mod server;
mod state;

use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use daemonize::Daemonize;
use qrcode::QrCode;

use repo_store::RepoStore;
use vex_cli::user_config::UserConfig;
use vex_cli::vex_home::vex_home;

// ── CLI definition ─────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "vexd", about = "Vex daemon — manages agent work streams")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the daemon in the background
    Start,
    /// Stop the running daemon
    Stop,
    /// Restart the daemon
    Restart,
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
    /// Repository management
    Repo {
        #[command(subcommand)]
        action: RepoCmd,
    },
    /// Run daemon in the foreground (for use by service managers)
    #[command(hide = true)]
    StartForeground,
    /// Print shell completion script
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
    /// Manage system service integration
    Svc {
        #[command(subcommand)]
        action: SvcCmd,
    },
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

#[derive(Subcommand)]
enum RepoCmd {
    /// Register a git repository (daemon must be running)
    Register {
        /// Path to the repository (can be relative; `.` works)
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum SvcCmd {
    /// Install and start the system service
    Enable,
    /// Stop and remove the system service
    Disable,
    /// Show system service status
    Status,
}

// ── Path helpers ─────────────────────────────────────────────────────────────

fn daemon_dir() -> Result<PathBuf> {
    Ok(vex_home().join("daemon"))
}

fn admin_socket_path() -> Result<PathBuf> {
    Ok(daemon_dir()?.join("vexd.sock"))
}

fn pid_file() -> Result<PathBuf> {
    Ok(daemon_dir()?.join("vexd.pid"))
}

// ── PID helpers ───────────────────────────────────────────────────────────────

/// Read the stored PID, or None if the file is absent / unreadable.
fn read_pid() -> Option<u32> {
    pid_file()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse().ok())
}

/// Return true if a process with `pid` is alive (kill -0).
fn pid_is_alive(pid: u32) -> bool {
    // Safety: we are only sending signal 0, which probes existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

// ── Entry point ─────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Start => do_start(),

        Commands::StartForeground => run_foreground(),

        Commands::Restart => {
            if let Err(e) = do_stop() {
                eprintln!("stop: {e}");
            }
            do_start()
        }

        Commands::Stop => do_stop(),

        Commands::Status => do_status(),

        Commands::Logs => {
            let log_path = daemon_dir()?.join("vexd.log");
            if !log_path.exists() {
                anyhow::bail!("Log file not found: {}", log_path.display());
            }
            // exec into tail — replaces this process
            let err = std::os::unix::process::CommandExt::exec(
                std::process::Command::new("tail").arg("-f").arg(&log_path),
            );
            Err(err.into())
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

        Commands::Repo { action } => tokio::runtime::Runtime::new()?.block_on(async {
            let sock = admin_socket_path()?;
            match action {
                RepoCmd::Register { path } => {
                    let abs = path
                        .canonicalize()
                        .with_context(|| format!("cannot resolve '{}'", path.display()))?;
                    let path_str = abs.to_string_lossy().to_string();
                    let repo = match local::send_command(
                        &sock,
                        &vex_proto::Command::RepoRegister {
                            path: path_str.clone(),
                        },
                    )
                    .await?
                    {
                        vex_proto::Response::RepoRegistered(repo) => repo,
                        vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                        other => anyhow::bail!("Unexpected: {other:?}"),
                    };

                    println!(
                        "Registered {} ({}) [default branch: {}]",
                        repo.id, repo.name, repo.default_branch
                    );
                }
            }
            Ok(())
        }),

        Commands::Svc { action } => cmd_svc(action),
    }
}

// ── Start / stop / status ────────────────────────────────────────────────────

fn do_start() -> Result<()> {
    // Check if already running
    if let Some(pid) = read_pid()
        && pid_is_alive(pid)
    {
        println!("vexd already running (pid {pid})");
        return Ok(());
    }

    let daemon_dir = daemon_dir()?;
    std::fs::create_dir_all(&daemon_dir)?;
    // Restrict daemon dir so other local users cannot traverse into it
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&daemon_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::create_dir_all(daemon_dir.join("tls"))?;

    let log = open_log(&daemon_dir)?;
    Daemonize::new()
        .pid_file(daemon_dir.join("vexd.pid"))
        .stdout(log.try_clone()?)
        .stderr(log)
        .start()
        .context("daemonize failed")?;
    // We are now in the daemon child process.

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("Failed to install rustls crypto provider"))?;

    tokio::runtime::Runtime::new()?.block_on(run_daemon(vex_home()))
}

/// Run in the foreground — used by systemd/launchd service files.
fn run_foreground() -> Result<()> {
    let daemon_dir = daemon_dir()?;
    std::fs::create_dir_all(&daemon_dir)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&daemon_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::create_dir_all(daemon_dir.join("tls"))?;
    std::fs::write(daemon_dir.join("vexd.pid"), std::process::id().to_string())
        .context("writing PID file")?;

    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("Failed to install rustls crypto provider"))?;

    tokio::runtime::Runtime::new()?.block_on(run_daemon(vex_home()))
}

fn do_stop() -> Result<()> {
    let pid_path = pid_file()?;
    let Some(pid) = read_pid() else {
        println!("vexd not running");
        return Ok(());
    };

    if !pid_is_alive(pid) {
        println!("vexd not running (stale pid file)");
        let _ = std::fs::remove_file(&pid_path);
        return Ok(());
    }

    // Send SIGTERM
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };

    // Wait up to 5 seconds
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !pid_is_alive(pid) {
            let _ = std::fs::remove_file(&pid_path);
            println!("vexd stopped");
            return Ok(());
        }
    }

    // Still alive — send SIGKILL
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(200));
    let _ = std::fs::remove_file(&pid_path);
    println!("vexd killed");
    Ok(())
}

fn do_status() -> Result<()> {
    let Some(pid) = read_pid() else {
        println!("vexd not running");
        return Ok(());
    };

    if !pid_is_alive(pid) {
        println!("vexd dead (stale pid file)");
        return Ok(());
    }

    // Process is alive — try to get details from the socket
    let sock = admin_socket_path()?;
    match tokio::runtime::Runtime::new()?
        .block_on(local::send_command(&sock, &vex_proto::Command::Status))
    {
        Ok(vex_proto::Response::DaemonStatus(s)) => {
            println!(
                "vexd running (pid {pid}) | uptime {}s | {} clients",
                s.uptime_secs, s.connected_clients
            );
        }
        _ => {
            // Socket not ready yet (starting up), but process is alive
            println!("vexd running (pid {pid}) | starting up...");
        }
    }
    Ok(())
}

// ── Daemon server loop ──────────────────────────────────────────────────────

async fn run_daemon(vex_home: PathBuf) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let daemon_dir = vex_home.join("daemon");
    std::fs::create_dir_all(&daemon_dir)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&daemon_dir, std::fs::Permissions::from_mode(0o700))?;
    }

    let user_config = UserConfig::load(&vex_home);
    let token_store = auth::TokenStore::load(daemon_dir.join("tokens.json"))?;
    let mut repo_store = RepoStore::load(daemon_dir.join("repos.json"))?;

    // Reconcile: detect sessions that died while daemon was down
    let alive = list_tmux_sessions().await;
    let running_agents = repo_store.reconcile(&alive);
    repo_store::warn_missing_worktrees(&repo_store.repos);
    if let Err(e) = repo_store.save() {
        tracing::warn!("Failed to persist reconciled state: {e}");
    }

    let state = state::AppState::new(vex_home.clone(), token_store, repo_store, user_config);

    // Restart monitoring tasks for agents that are still running
    for (ws_id, agent_id, tmux_window) in running_agents {
        let handle = tokio::spawn(server::monitor_agent(
            state.clone(),
            ws_id,
            agent_id.clone(),
            tmux_window,
        ));
        state
            .monitor_handles
            .lock()
            .await
            .insert(agent_id, handle.abort_handle());
    }

    let socket_path = state.socket_path();
    let tcp_port: u16 = std::env::var("VEXD_TCP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(vex_proto::DEFAULT_TCP_PORT);
    let tcp_addr: SocketAddr = format!("0.0.0.0:{tcp_port}")
        .parse()
        .context("invalid TCP address")?;
    let tls_dir = daemon_dir.join("tls");

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

// ── Tmux session list ────────────────────────────────────────────────────────

async fn list_tmux_sessions() -> HashSet<String> {
    let Ok(out) = tokio::process::Command::new("tmux")
        .arg("list-sessions")
        .arg("-F")
        .arg("#{session_name}")
        .output()
        .await
    else {
        return HashSet::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect()
}

// ── svc subcommand ────────────────────────────────────────────────────────────

fn cmd_svc(action: &SvcCmd) -> Result<()> {
    match action {
        SvcCmd::Enable => svc_enable(),
        SvcCmd::Disable => svc_disable(),
        SvcCmd::Status => svc_status(),
    }
}

#[cfg(target_os = "linux")]
fn svc_enable() -> Result<()> {
    let exe = std::env::current_exe().context("cannot determine vexd binary path")?;
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let service_dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&service_dir)?;
    let service_path = service_dir.join("vexd.service");
    let unit = format!(
        "[Unit]\n\
         Description=Vex Daemon\n\
         After=network.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe} start-foreground\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         Environment=VEX_HOME=%h/.vex\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display()
    );
    std::fs::write(&service_path, unit)?;
    std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()?;
    std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "vexd"])
        .status()?;
    println!("vexd service enabled and started");
    Ok(())
}

#[cfg(target_os = "linux")]
fn svc_disable() -> Result<()> {
    std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "vexd"])
        .status()?;
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let _ = std::fs::remove_file(home.join(".config/systemd/user/vexd.service"));
    std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()?;
    println!("vexd service disabled");
    Ok(())
}

#[cfg(target_os = "linux")]
fn svc_status() -> Result<()> {
    let err = std::os::unix::process::CommandExt::exec(
        std::process::Command::new("systemctl").args(["--user", "status", "vexd"]),
    );
    Err(err.into())
}

#[cfg(target_os = "macos")]
fn svc_enable() -> Result<()> {
    let exe = std::env::current_exe().context("cannot determine vexd binary path")?;
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let vex_home = home.join(".vex");
    let agents_dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents_dir)?;
    let plist_path = agents_dir.join("com.vex.vexd.plist");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.vex.vexd</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>start-foreground</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>VEX_HOME</key>
    <string>{vex_home}</string>
  </dict>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        exe = exe.display(),
        vex_home = vex_home.display(),
        log = vex_home.join("daemon/vexd.log").display(),
    );
    std::fs::write(&plist_path, plist)?;
    std::process::Command::new("launchctl")
        .arg("load")
        .arg(&plist_path)
        .status()?;
    println!("vexd service enabled and started");
    Ok(())
}

#[cfg(target_os = "macos")]
fn svc_disable() -> Result<()> {
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let plist_path = home.join("Library/LaunchAgents/com.vex.vexd.plist");
    std::process::Command::new("launchctl")
        .arg("unload")
        .arg(&plist_path)
        .status()?;
    let _ = std::fs::remove_file(&plist_path);
    println!("vexd service disabled");
    Ok(())
}

#[cfg(target_os = "macos")]
fn svc_status() -> Result<()> {
    let err = std::os::unix::process::CommandExt::exec(
        std::process::Command::new("launchctl").args(["list", "com.vex.vexd"]),
    );
    Err(err.into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn svc_enable() -> Result<()> {
    anyhow::bail!("vexd svc is only supported on Linux and macOS")
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn svc_disable() -> Result<()> {
    anyhow::bail!("vexd svc is only supported on Linux and macOS")
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn svc_status() -> Result<()> {
    anyhow::bail!("vexd svc is only supported on Linux and macOS")
}

// ── Misc helpers ────────────────────────────────────────────────────────────

fn open_log(daemon_dir: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(daemon_dir.join("vexd.log"))
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
