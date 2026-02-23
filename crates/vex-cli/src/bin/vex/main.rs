use vex_cli as vex_proto;

mod config;
mod connect;

use anyhow::Result;
use clap::{Args, CommandFactory, Parser, Subcommand};

use config::{Config, ConnectionEntry};
use connect::Connection;

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "vex",
    about = "Vex client — connects to one or more vexd daemons"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Save a connection (local Unix socket or remote TLS)
    Connect(ConnectArgs),
    /// Remove a saved connection
    Disconnect(ConnectionFlag),
    /// Set the default connection
    Use {
        /// Connection name to make the default
        name: String,
    },
    /// List all saved connections
    List,
    /// Print daemon status
    Status(ConnectionFlag),
    /// Show who you are connected as
    Whoami(ConnectionFlag),
    /// List registered projects
    Projects(ConnectionFlag),
    /// Manage workstreams under a project
    Workstream {
        #[command(subcommand)]
        action: WorkstreamCmd,
    },
    /// Manage shells in a workstream
    Shell {
        #[command(subcommand)]
        action: ShellCmd,
    },
    /// Print shell completion script
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
}

#[derive(Args)]
struct ConnectArgs {
    /// Name for this connection (default: "default")
    #[arg(long, short = 'n', default_value = "default")]
    name: String,
    /// TCP host:port of a remote daemon (omit to use local Unix socket)
    #[arg(long)]
    host: Option<String>,
    /// Make this the default connection
    #[arg(long)]
    set_default: bool,
}

#[derive(Args)]
struct ConnectionFlag {
    /// Connection name to use (uses the default when omitted)
    #[arg(long, short = 'c')]
    connection: Option<String>,
    /// Run against all saved connections
    #[arg(long)]
    all: bool,
}

#[derive(Subcommand)]
enum WorkstreamCmd {
    /// Create a new workstream under a project
    Create {
        /// Project name
        project_name: String,
        /// Workstream name
        name: String,
        #[command(flatten)]
        conn: ConnectionFlag,
    },
    /// List workstreams for a project
    List {
        /// Project name
        project_name: String,
        #[command(flatten)]
        conn: ConnectionFlag,
    },
    /// Delete a workstream by name
    Delete {
        /// Project name
        project_name: String,
        /// Workstream name
        name: String,
        #[command(flatten)]
        conn: ConnectionFlag,
    },
}

#[derive(Subcommand)]
enum ShellCmd {
    /// Create a new shell in a workstream
    Create {
        /// Project name
        project_name: String,
        /// Workstream name
        workstream_name: String,
        #[command(flatten)]
        conn: ConnectionFlag,
    },
    /// List shells in a workstream
    List {
        /// Project name
        project_name: String,
        /// Workstream name
        workstream_name: String,
        #[command(flatten)]
        conn: ConnectionFlag,
    },
    /// Delete a shell by ID
    Delete {
        /// Project name
        project_name: String,
        /// Workstream name
        workstream_name: String,
        /// Shell ID (e.g. shell_1)
        shell_id: String,
        #[command(flatten)]
        conn: ConnectionFlag,
    },
    /// Attach to an interactive shell
    Attach {
        /// Project name
        project_name: String,
        /// Workstream name
        workstream_name: String,
        /// Shell ID (omit to attach to the first shell)
        shell_id: Option<String>,
        #[command(flatten)]
        conn: ConnectionFlag,
    },
}

// ── Entry point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let cli = Cli::parse();

    match cli.command {
        Commands::Connect(args) => cmd_connect(args).await,
        Commands::Disconnect(flag) => cmd_disconnect(flag),
        Commands::Use { name } => cmd_use(&name),
        Commands::List => cmd_list(),
        Commands::Status(flag) => cmd_with_connections(flag, DaemonCmd::Status).await,
        Commands::Whoami(flag) => cmd_with_connections(flag, DaemonCmd::Whoami).await,
        Commands::Projects(flag) => cmd_with_connections(flag, DaemonCmd::Projects).await,
        Commands::Workstream { action } => match action {
            WorkstreamCmd::Create {
                project_name,
                name,
                conn,
            } => {
                cmd_with_connections(conn, DaemonCmd::WorkstreamCreate { project_name, name }).await
            }
            WorkstreamCmd::List { project_name, conn } => {
                cmd_with_connections(conn, DaemonCmd::WorkstreamList { project_name }).await
            }
            WorkstreamCmd::Delete {
                project_name,
                name,
                conn,
            } => {
                cmd_with_connections(conn, DaemonCmd::WorkstreamDelete { project_name, name }).await
            }
        },
        Commands::Shell { action } => match action {
            ShellCmd::Create {
                project_name,
                workstream_name,
                conn,
            } => {
                cmd_with_connections(
                    conn,
                    DaemonCmd::ShellCreate {
                        project_name,
                        workstream_name,
                    },
                )
                .await
            }
            ShellCmd::List {
                project_name,
                workstream_name,
                conn,
            } => {
                cmd_with_connections(
                    conn,
                    DaemonCmd::ShellList {
                        project_name,
                        workstream_name,
                    },
                )
                .await
            }
            ShellCmd::Delete {
                project_name,
                workstream_name,
                shell_id,
                conn,
            } => {
                cmd_with_connections(
                    conn,
                    DaemonCmd::ShellDelete {
                        project_name,
                        workstream_name,
                        shell_id,
                    },
                )
                .await
            }
            ShellCmd::Attach {
                project_name,
                workstream_name,
                shell_id,
                conn,
            } => cmd_shell_attach(conn, project_name, workstream_name, shell_id).await,
        },
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "vex", &mut std::io::stdout());
            Ok(())
        }
    }
}

// ── Command implementations ──────────────────────────────────────────────────

async fn cmd_connect(args: ConnectArgs) -> Result<()> {
    match args.host {
        None => {
            // Local Unix socket
            let socket_path = default_socket_path();
            // Verify daemon is reachable
            tokio::net::UnixStream::connect(&socket_path)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Cannot reach vexd at {socket_path} — is it running? ({e})")
                })?;

            let entry = ConnectionEntry {
                transport: "unix".to_string(),
                unix_socket: Some(socket_path.clone()),
                ..Default::default()
            };

            let mut cfg = Config::load()?;
            cfg.upsert(args.name.clone(), entry, args.set_default);
            cfg.save()?;
            println!(
                "Connection '{}' saved (Unix socket: {socket_path})",
                args.name
            );
            if cfg.default_connection.as_deref() == Some(&args.name) {
                println!("Set as default connection.");
            }
        }
        Some(host) => {
            // Remote TLS
            eprint!("Enter pairing token (from 'vexd pair'): ");
            let pairing = read_line()?;
            let pairing = pairing.trim();

            let (token_id, token_secret) = pairing.split_once(':').ok_or_else(|| {
                anyhow::anyhow!("Invalid pairing token — expected <token_id>:<secret>")
            })?;

            let (_, new_fp) = Connection::tcp_connect(
                &host,
                token_id,
                token_secret,
                None, // no existing fingerprint — TOFU
            )
            .await?;

            if let Some(ref fp) = new_fp {
                println!("TLS fingerprint pinned: {fp}");
            }

            let entry = ConnectionEntry {
                transport: "tcp".to_string(),
                tcp_host: Some(host.clone()),
                token_id: Some(token_id.to_string()),
                token_secret: Some(token_secret.to_string()),
                tls_fingerprint: new_fp,
                ..Default::default()
            };

            let mut cfg = Config::load()?;
            cfg.upsert(args.name.clone(), entry, args.set_default);
            cfg.save()?;
            println!("Connection '{}' saved (TCP: {host})", args.name);
            if cfg.default_connection.as_deref() == Some(&args.name) {
                println!("Set as default connection.");
            }
        }
    }
    Ok(())
}

fn cmd_disconnect(flag: ConnectionFlag) -> Result<()> {
    let mut cfg = Config::load()?;
    if flag.all {
        cfg.clear_all();
        cfg.save()?;
        println!("All connections removed.");
    } else {
        let name = flag
            .connection
            .as_deref()
            .or(cfg.default_connection.as_deref())
            .ok_or_else(|| anyhow::anyhow!("No connection specified and no default is set."))?
            .to_string();
        if cfg.remove(&name) {
            cfg.save()?;
            println!("Removed connection '{name}'.");
        } else {
            println!("Connection '{name}' not found.");
        }
    }
    Ok(())
}

fn cmd_use(name: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    if !cfg.connections.contains_key(name) {
        anyhow::bail!("Unknown connection '{name}'. Run 'vex list' to see available connections.");
    }
    cfg.default_connection = Some(name.to_string());
    cfg.save()?;
    println!("Default connection set to '{name}'.");
    Ok(())
}

fn cmd_list() -> Result<()> {
    let cfg = Config::load()?;
    if cfg.connections.is_empty() {
        println!("No saved connections. Run 'vex connect' to add one.");
        return Ok(());
    }
    let default = cfg.default_connection.as_deref().unwrap_or("");
    for (name, entry) in &cfg.connections {
        let marker = if name == default { "*" } else { " " };
        let target = match entry.transport.as_str() {
            "tcp" => entry.tcp_host.as_deref().unwrap_or("?").to_string(),
            _ => entry
                .unix_socket
                .as_deref()
                .unwrap_or("~/.vex/vexd.sock")
                .to_string(),
        };
        println!("{marker} {name:<20} {} → {target}", entry.transport);
    }
    Ok(())
}

// ── Generic command runner ────────────────────────────────────────────────────

#[derive(Clone)]
enum DaemonCmd {
    Status,
    Whoami,
    Projects,
    WorkstreamCreate {
        project_name: String,
        name: String,
    },
    WorkstreamList {
        project_name: String,
    },
    WorkstreamDelete {
        project_name: String,
        name: String,
    },
    ShellCreate {
        project_name: String,
        workstream_name: String,
    },
    ShellList {
        project_name: String,
        workstream_name: String,
    },
    ShellDelete {
        project_name: String,
        workstream_name: String,
        shell_id: String,
    },
}

async fn run_cmd(conn: &mut Connection, cmd: DaemonCmd) -> Result<()> {
    match cmd {
        DaemonCmd::Status => run_status(conn).await,
        DaemonCmd::Whoami => run_whoami(conn).await,
        DaemonCmd::Projects => run_projects(conn).await,
        DaemonCmd::WorkstreamCreate { project_name, name } => {
            run_workstream_create(conn, project_name, name).await
        }
        DaemonCmd::WorkstreamList { project_name } => run_workstream_list(conn, project_name).await,
        DaemonCmd::WorkstreamDelete { project_name, name } => {
            run_workstream_delete(conn, project_name, name).await
        }
        DaemonCmd::ShellCreate {
            project_name,
            workstream_name,
        } => run_shell_create(conn, project_name, workstream_name).await,
        DaemonCmd::ShellList {
            project_name,
            workstream_name,
        } => run_shell_list(conn, project_name, workstream_name).await,
        DaemonCmd::ShellDelete {
            project_name,
            workstream_name,
            shell_id,
        } => run_shell_delete(conn, project_name, workstream_name, shell_id).await,
    }
}

async fn cmd_with_connections(flag: ConnectionFlag, cmd: DaemonCmd) -> Result<()> {
    let mut cfg = Config::load()?;
    let mut cfg_changed = false;

    if flag.all {
        let names: Vec<String> = cfg.connections.keys().cloned().collect();
        let mut any_error = false;
        for name in &names {
            let entry = cfg.connections.get_mut(name).expect("key from keys()");
            let fp_before = entry.tls_fingerprint.clone();
            print!("[{name}] ");
            match Connection::from_entry(entry).await {
                Ok(mut conn) => {
                    if entry.tls_fingerprint != fp_before {
                        cfg_changed = true;
                    }
                    if let Err(e) = run_cmd(&mut conn, cmd.clone()).await {
                        eprintln!("error: {e}");
                        any_error = true;
                    }
                }
                Err(e) => {
                    eprintln!("connect error: {e}");
                    any_error = true;
                }
            }
        }
        if cfg_changed {
            cfg.save()?;
        }
        if any_error {
            anyhow::bail!("One or more connections failed");
        }
    } else {
        // Resolve to get the name (drops the immutable borrow on cfg immediately)
        let name: String = {
            let conn_name = flag.connection.as_deref();
            let (n, _) = cfg.resolve(conn_name)?;
            n
        };
        let entry = cfg
            .connections
            .get_mut(&name)
            .expect("resolve verified key exists");
        let fp_before = entry.tls_fingerprint.clone();
        let mut conn = Connection::from_entry(entry).await?;
        if entry.tls_fingerprint != fp_before {
            cfg.save()?;
        }
        run_cmd(&mut conn, cmd).await?;
    }

    Ok(())
}

async fn run_status(conn: &mut Connection) -> Result<()> {
    conn.send(&vex_proto::Command::Status).await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::DaemonStatus(s) => {
            println!(
                "vexd v{} | uptime: {}s | clients: {}",
                s.version, s.uptime_secs, s.connected_clients
            );
        }
        other => println!("{other:?}"),
    }
    Ok(())
}

async fn run_whoami(conn: &mut Connection) -> Result<()> {
    conn.send(&vex_proto::Command::Whoami).await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::ClientInfo(info) => {
            if info.is_local {
                println!("local (admin via Unix socket)");
            } else if let Some(id) = &info.token_id {
                println!("authenticated as token: {id}");
            } else {
                println!("unauthenticated TCP connection");
            }
        }
        other => println!("{other:?}"),
    }
    Ok(())
}

async fn run_projects(conn: &mut Connection) -> Result<()> {
    conn.send(&vex_proto::Command::ProjectList).await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::Projects(projects) => {
            if projects.is_empty() {
                println!("No registered projects.");
            }
            for p in &projects {
                println!("{:<20} {:<30} {}", p.name, p.repo, p.path);
            }
        }
        other => println!("{other:?}"),
    }
    Ok(())
}

async fn run_workstream_create(
    conn: &mut Connection,
    project_name: String,
    name: String,
) -> Result<()> {
    conn.send(&vex_proto::Command::WorkstreamCreate { project_name, name })
        .await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::Workstream(ws) => {
            println!(
                "Created workstream '{}' in project '{}'",
                ws.name, ws.project_name
            );
        }
        vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
    Ok(())
}

async fn run_workstream_list(conn: &mut Connection, project_name: String) -> Result<()> {
    conn.send(&vex_proto::Command::WorkstreamList { project_name })
        .await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::Workstreams(ws) => {
            if ws.is_empty() {
                println!("No workstreams.");
            }
            for w in &ws {
                println!("{}", w.name);
            }
        }
        vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
    Ok(())
}

async fn run_workstream_delete(
    conn: &mut Connection,
    project_name: String,
    name: String,
) -> Result<()> {
    conn.send(&vex_proto::Command::WorkstreamDelete {
        project_name,
        name: name.clone(),
    })
    .await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::Ok => println!("Deleted workstream '{name}'."),
        vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
    Ok(())
}

async fn run_shell_create(
    conn: &mut Connection,
    project_name: String,
    workstream_name: String,
) -> Result<()> {
    conn.send(&vex_proto::Command::ShellCreate {
        project_name,
        workstream_name,
    })
    .await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::Shell(s) => {
            println!(
                "Created shell '{}' in workstream '{}' (project '{}')",
                s.id, s.workstream_name, s.project_name
            );
        }
        vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
    Ok(())
}

async fn run_shell_list(
    conn: &mut Connection,
    project_name: String,
    workstream_name: String,
) -> Result<()> {
    conn.send(&vex_proto::Command::ShellList {
        project_name,
        workstream_name,
    })
    .await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::Shells(shells) => {
            if shells.is_empty() {
                println!("No shells.");
            }
            for s in &shells {
                println!("{}", s.id);
            }
        }
        vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
    Ok(())
}

async fn run_shell_delete(
    conn: &mut Connection,
    project_name: String,
    workstream_name: String,
    shell_id: String,
) -> Result<()> {
    conn.send(&vex_proto::Command::ShellDelete {
        project_name,
        workstream_name,
        shell_id: shell_id.clone(),
    })
    .await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::Ok => println!("Deleted shell '{shell_id}'."),
        vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
    Ok(())
}

// ── Shell attach ─────────────────────────────────────────────────────────────

async fn cmd_shell_attach(
    flag: ConnectionFlag,
    project_name: String,
    workstream_name: String,
    shell_id: Option<String>,
) -> Result<()> {
    let mut cfg = Config::load()?;
    let name: String = {
        let conn_name = flag.connection.as_deref();
        let (n, _) = cfg.resolve(conn_name)?;
        n
    };
    let entry = cfg
        .connections
        .get_mut(&name)
        .expect("resolve verified key exists");
    let fp_before = entry.tls_fingerprint.clone();
    let mut conn = Connection::from_entry(entry).await?;
    if entry.tls_fingerprint != fp_before {
        cfg.save()?;
    }

    // Send ShellAttach command
    conn.send(&vex_proto::Command::ShellAttach {
        project_name,
        workstream_name,
        shell_id,
    })
    .await?;
    let response: vex_proto::Response = conn.recv().await?;

    match response {
        vex_proto::Response::ShellAttachReady { tmux_target } => {
            if conn.is_local() {
                // Local: exec tmux attach directly (replaces this process)
                drop(conn);
                let err = std::os::unix::process::CommandExt::exec(
                    std::process::Command::new("tmux").args(["attach-session", "-t", &tmux_target]),
                );
                // exec() only returns on error
                anyhow::bail!("exec tmux failed: {err}");
            } else {
                // Remote: PTY passthrough over the TLS connection
                run_remote_attach(conn).await
            }
        }
        vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("Unexpected response: {other:?}"),
    }
}

async fn run_remote_attach(conn: Connection) -> Result<()> {
    crossterm::terminal::enable_raw_mode()?;

    let result = run_remote_attach_inner(conn).await;

    // Always restore terminal, even on error
    let _ = crossterm::terminal::disable_raw_mode();

    result
}

async fn run_remote_attach_inner(conn: Connection) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use vex_proto::attach_frame;

    let (mut net_read, mut net_write) = conn.into_split();

    // Send initial terminal size
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    attach_frame::send_resize(&mut net_write, cols, rows).await?;

    let mut stdin_buf = vec![0u8; 1024];
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;

    loop {
        tokio::select! {
            // stdin → network
            result = stdin.read(&mut stdin_buf) => {
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if attach_frame::send_data(&mut net_write, &stdin_buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
            // network → stdout
            frame = attach_frame::recv(&mut net_read) => {
                match frame {
                    Ok(Some(attach_frame::Frame::Data(data))) => {
                        if stdout.write_all(&data).await.is_err() {
                            break;
                        }
                        let _ = stdout.flush().await;
                    }
                    Ok(Some(attach_frame::Frame::Close)) | Ok(None) | Err(_) => break,
                    Ok(Some(attach_frame::Frame::Resize { .. })) => {
                        // Server shouldn't send resize, ignore
                    }
                }
            }
            // Terminal resize → network
            _ = sigwinch.recv() => {
                if let Ok((cols, rows)) = crossterm::terminal::size() {
                    let _ = attach_frame::send_resize(&mut net_write, cols, rows).await;
                }
            }
        }
    }

    // Best-effort close
    let _ = attach_frame::send_close(&mut net_write).await;

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn default_socket_path() -> String {
    dirs::home_dir()
        .map(|h| h.join(".vex").join("vexd.sock"))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/tmp/vexd.sock".to_string())
}

fn read_line() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_line(&mut buf)
        .map_err(|e| anyhow::anyhow!("Failed to read stdin: {e}"))?;
    Ok(buf)
}
