use vex_cli as vex_proto;

mod config;
mod connect;
mod tui;

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use tokio::task::JoinSet;

use config::{Config, ConnectionEntry};
use connect::Connection;
use vex_cli::vex_home::vex_home;

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "vex",
    about = "Vex client — connects to one or more vexd daemons"
)]
struct Cli {
    /// If omitted, opens the interactive TUI
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Save a connection (local Unix socket or remote TLS)
    Connect(ConnectArgs),
    /// Remove a saved connection (-c <name> or --all)
    Disconnect(DisconnectArgs),
    /// List all saved connections
    List,
    /// Print daemon status for all connections (or one with -c)
    Status(Filter),
    /// Show who you are connected as for all connections (or one with -c)
    Whoami(Filter),
    /// Repository commands
    Repo {
        #[command(subcommand)]
        action: RepoCmd,
    },
    /// Workstream commands
    Workstream {
        #[command(subcommand)]
        action: WorkstreamCmd,
    },
    /// Agent commands
    Agent {
        #[command(subcommand)]
        action: AgentCmd,
    },
    /// Print shell completion script
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
}

// ── Subcommand structs ────────────────────────────────────────────────────────

#[derive(Args)]
struct ConnectArgs {
    #[arg(long, short = 'n', default_value = "default")]
    name: String,
    #[arg(long)]
    host: Option<String>,
}

#[derive(Args)]
struct DisconnectArgs {
    #[arg(long, short = 'c')]
    connection: Option<String>,
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct Filter {
    #[arg(long, short = 'c')]
    connection: Option<String>,
}

/// Shared single-connection selector for repo/workstream/agent commands.
#[derive(Args)]
struct Single {
    /// Named connection to use (default: local Unix socket, then "default")
    #[arg(long, short = 'c')]
    connection: Option<String>,
}

// ── Repo subcommands ──────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum RepoCmd {
    /// List registered repositories
    List(Single),
    /// Unregister a repository
    Unregister {
        repo_id: String,
        #[command(flatten)]
        single: Single,
    },
}

// ── Workstream subcommands ────────────────────────────────────────────────────

#[derive(Subcommand)]
enum WorkstreamCmd {
    /// Create a new workstream (git worktree + tmux session)
    Create {
        repo_id: String,
        /// Workstream name; defaults to <branch> if omitted
        #[arg(long)]
        name: Option<String>,
        /// Branch to check out; omit to use the repo's default branch
        #[arg(long)]
        branch: Option<String>,
        /// Fetch from origin and fast-forward the branch before creating
        #[arg(long)]
        fetch: bool,
        #[command(flatten)]
        single: Single,
    },
    /// List workstreams (optionally filtered to one repo)
    List {
        repo_id: Option<String>,
        #[command(flatten)]
        single: Single,
    },
    /// Delete a workstream and its tmux session
    Delete {
        workstream_id: String,
        #[command(flatten)]
        single: Single,
    },
}

// ── Agent subcommands ─────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum AgentCmd {
    /// Spawn an agent in a workstream.
    /// If run from inside a vex tmux pane (no workstream_id given), the
    /// current pane is claimed and the agent command is exec'd in-place.
    Spawn {
        /// Workstream ID. Omit when already inside a vex tmux session.
        workstream_id: Option<String>,
        /// Task description. Optional — omit to run the agent interactively.
        #[arg(long)]
        prompt: Option<String>,
        #[command(flatten)]
        single: Single,
    },
    /// Kill a running agent
    Kill {
        agent_id: String,
        #[command(flatten)]
        single: Single,
    },
    /// List agents in a workstream
    List {
        workstream_id: String,
        #[command(flatten)]
        single: Single,
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
        None => {
            // No subcommand → open TUI
            let (conn, label) = open_single_connection(None).await?;
            tui::run(conn, label).await
        }
        Some(Commands::Connect(args)) => cmd_connect(args).await,
        Some(Commands::Disconnect(args)) => cmd_disconnect(args),
        Some(Commands::List) => cmd_list(),
        Some(Commands::Status(filter)) => cmd_with_connections(filter, DaemonCmd::Status).await,
        Some(Commands::Whoami(filter)) => cmd_with_connections(filter, DaemonCmd::Whoami).await,
        Some(Commands::Completions { shell }) => {
            clap_complete::generate(shell, &mut Cli::command(), "vex", &mut std::io::stdout());
            Ok(())
        }
        Some(Commands::Repo { action }) => cmd_repo(action).await,
        Some(Commands::Workstream { action }) => cmd_workstream(action).await,
        Some(Commands::Agent { action }) => cmd_agent(action).await,
    }
}

// ── Existing command implementations ─────────────────────────────────────────

async fn cmd_connect(args: ConnectArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    match args.host {
        None => {
            let socket_path = default_socket_path();
            tokio::net::UnixStream::connect(&socket_path)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("Cannot reach vexd at {socket_path} — is it running? ({e})")
                })?;
            cfg.upsert(
                args.name.clone(),
                ConnectionEntry {
                    transport: "unix".to_string(),
                    unix_socket: Some(socket_path.clone()),
                    ..Default::default()
                },
            );
            cfg.save()?;
            println!(
                "Connection '{}' saved (Unix socket: {socket_path})",
                args.name
            );
        }
        Some(host) => {
            eprint!("Enter pairing token (from 'vexd pair'): ");
            let pairing = read_line()?;
            let pairing = pairing.trim();

            let (token_id, token_secret) = pairing.split_once(':').ok_or_else(|| {
                anyhow::anyhow!("Invalid pairing token — expected <token_id>:<secret>")
            })?;

            let (_, new_fp) = Connection::tcp_connect(&host, token_id, token_secret, None).await?;

            if let Some(ref fp) = new_fp {
                println!("TLS fingerprint pinned: {fp}");
            }

            cfg.upsert(
                args.name.clone(),
                ConnectionEntry {
                    transport: "tcp".to_string(),
                    tcp_host: Some(host.clone()),
                    token_id: Some(token_id.to_string()),
                    token_secret: Some(token_secret.to_string()),
                    tls_fingerprint: new_fp,
                    ..Default::default()
                },
            );
            cfg.save()?;
            println!("Connection '{}' saved (TCP: {host})", args.name);
        }
    }
    Ok(())
}

fn cmd_disconnect(args: DisconnectArgs) -> Result<()> {
    let mut cfg = Config::load()?;
    if args.all {
        let count = cfg.connections.len();
        cfg.clear_all();
        cfg.save()?;
        println!("Removed {count} connection(s).");
    } else if let Some(name) = args.connection {
        if cfg.remove(&name) {
            cfg.save()?;
            println!("Removed connection '{name}'.");
        } else {
            anyhow::bail!("Unknown connection '{name}'.");
        }
    } else {
        anyhow::bail!("Specify a connection with -c <name> or use --all.");
    }
    Ok(())
}

fn cmd_list() -> Result<()> {
    let cfg = Config::load()?;
    if cfg.connections.is_empty() {
        println!("No saved connections. Run 'vex connect' to add one.");
        return Ok(());
    }
    let mut names: Vec<&String> = cfg.connections.keys().collect();
    names.sort();
    for name in names {
        let entry = &cfg.connections[name];
        let target = match entry.transport.as_str() {
            "tcp" => entry.tcp_host.as_deref().unwrap_or("?").to_string(),
            _ => entry
                .unix_socket
                .as_deref()
                .unwrap_or("~/.vex/daemon/vexd.sock")
                .to_string(),
        };
        println!("{name:<20} {} → {target}", entry.transport);
    }
    Ok(())
}

// ── Parallel command runner ───────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum DaemonCmd {
    Status,
    Whoami,
}

async fn cmd_with_connections(filter: Filter, cmd: DaemonCmd) -> Result<()> {
    let mut cfg = Config::load()?;

    let names: Vec<String> = match filter.connection {
        Some(ref name) => {
            if !cfg.connections.contains_key(name.as_str()) {
                anyhow::bail!("Unknown connection '{name}'. Run 'vex list' to see available.");
            }
            vec![name.clone()]
        }
        None => {
            if cfg.connections.is_empty() {
                println!("No connections. Run 'vex connect' to add one.");
                return Ok(());
            }
            let mut v: Vec<String> = cfg.connections.keys().cloned().collect();
            v.sort();
            v
        }
    };

    let tasks: Vec<(String, ConnectionEntry)> = names
        .iter()
        .map(|n| (n.clone(), cfg.connections[n].clone()))
        .collect();

    let mut set: JoinSet<(String, Option<String>, Result<String>)> = JoinSet::new();
    for (name, mut entry) in tasks {
        set.spawn(async move {
            let fp_before = entry.tls_fingerprint.clone();
            match Connection::from_entry(&mut entry).await {
                Ok(mut conn) => {
                    let new_fp = (entry.tls_fingerprint != fp_before)
                        .then(|| entry.tls_fingerprint.clone())
                        .flatten();
                    let output = run_cmd(&mut conn, cmd).await;
                    (name, new_fp, output)
                }
                Err(e) => (name, None, Err(e)),
            }
        });
    }

    let mut results: Vec<(String, Option<String>, Result<String>)> = Vec::new();
    while let Some(res) = set.join_next().await {
        results.push(res.context("task panicked")?);
    }
    results.sort_by(|a, b| a.0.cmp(&b.0));

    let mut cfg_changed = false;
    let mut any_error = false;
    for (name, new_fp, output) in results {
        if let Some(fp) = new_fp
            && let Some(entry) = cfg.connections.get_mut(&name)
        {
            entry.tls_fingerprint = Some(fp);
            cfg_changed = true;
        }
        match output {
            Ok(line) => println!("[{name}] {line}"),
            Err(e) => {
                println!("[{name}] error: {e:#}");
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
    Ok(())
}

async fn run_cmd(conn: &mut Connection, cmd: DaemonCmd) -> Result<String> {
    match cmd {
        DaemonCmd::Status => run_status(conn).await,
        DaemonCmd::Whoami => run_whoami(conn).await,
    }
}

async fn run_status(conn: &mut Connection) -> Result<String> {
    conn.send(&vex_proto::Command::Status).await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::DaemonStatus(s) => Ok(format!(
            "vexd v{} | uptime: {}s | clients: {}",
            s.version, s.uptime_secs, s.connected_clients
        )),
        other => Ok(format!("{other:?}")),
    }
}

async fn run_whoami(conn: &mut Connection) -> Result<String> {
    conn.send(&vex_proto::Command::Whoami).await?;
    let response: vex_proto::Response = conn.recv().await?;
    match response {
        vex_proto::Response::ClientInfo(info) => {
            if info.is_local {
                Ok("local (admin via Unix socket)".to_string())
            } else if let Some(id) = &info.token_id {
                Ok(format!("authenticated as token: {id}"))
            } else {
                Ok("unauthenticated TCP connection".to_string())
            }
        }
        other => Ok(format!("{other:?}")),
    }
}

// ── Repo commands ─────────────────────────────────────────────────────────────

async fn cmd_repo(action: RepoCmd) -> Result<()> {
    match action {
        RepoCmd::List(single) => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::RepoList).await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::RepoList(repos) => {
                    if repos.is_empty() {
                        println!("No repositories registered.");
                        return Ok(());
                    }
                    println!("{:<14} {:<20} PATH", "ID", "NAME");
                    for repo in &repos {
                        println!("{:<14} {:<20} {}", repo.id, repo.name, repo.path);
                    }
                }
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
        RepoCmd::Unregister { repo_id, single } => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::RepoUnregister {
                repo_id: repo_id.clone(),
            })
            .await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::RepoUnregistered => {
                    println!("Unregistered {repo_id}.");
                }
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
    }
    Ok(())
}

// ── Workstream commands ───────────────────────────────────────────────────────

async fn cmd_workstream(action: WorkstreamCmd) -> Result<()> {
    match action {
        WorkstreamCmd::Create {
            repo_id,
            name,
            branch,
            fetch,
            single,
        } => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::WorkstreamCreate {
                repo_id,
                name,
                branch,
                fetch_latest: fetch,
            })
            .await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::WorkstreamCreated(ws) => {
                    if fetch {
                        println!("Fetched latest origin/{}", ws.branch);
                    }
                    println!("Created workstream {} ({})", ws.id, ws.name);
                    println!("  branch:  {}", ws.branch);
                    println!("  session: {}", ws.tmux_session);
                }
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
        WorkstreamCmd::List { repo_id, single } => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::WorkstreamList {
                repo_id: repo_id.clone(),
            })
            .await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::WorkstreamList(repos) => {
                    print_workstream_table(&repos);
                }
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
        WorkstreamCmd::Delete {
            workstream_id,
            single,
        } => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::WorkstreamDelete {
                workstream_id: workstream_id.clone(),
            })
            .await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::WorkstreamDeleted => {
                    println!("Deleted workstream {workstream_id}.");
                }
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
    }
    Ok(())
}

// ── Agent commands ────────────────────────────────────────────────────────────

async fn cmd_agent(action: AgentCmd) -> Result<()> {
    match action {
        AgentCmd::Spawn {
            workstream_id,
            prompt,
            single,
        } => {
            if let Some(ws_id) = workstream_id {
                // Explicit workstream ID → create a new tmux window (standard mode)
                let (mut conn, _) = open_single_connection(single.connection).await?;
                conn.send(&vex_proto::Command::AgentSpawn {
                    workstream_id: ws_id,
                    prompt: prompt.unwrap_or_default(),
                })
                .await?;
                let resp: vex_proto::Response = conn.recv().await?;
                match resp {
                    vex_proto::Response::AgentSpawned(agent) => {
                        println!("Spawned {} in tmux window {}", agent.id, agent.tmux_window);
                    }
                    vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                    other => anyhow::bail!("Unexpected: {other:?}"),
                }
            } else {
                // No workstream ID → in-place mode: must be inside a vex tmux session
                if std::env::var("TMUX").is_err() {
                    anyhow::bail!(
                        "No workstream_id given and not inside a tmux session.\n\
                         Either specify a workstream: vex agent spawn <ws_id>\n\
                         Or run this from inside a vex workstream tmux pane."
                    );
                }
                let session = tmux_display_message("#S").await?;
                let ws_id = session
                    .strip_prefix("vex-")
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Current tmux session '{session}' is not a vex workstream \
                             (expected 'vex-<id>')"
                        )
                    })?
                    .to_string();
                let window_str = tmux_display_message("#{window_index}").await?;
                let tmux_window: u32 = window_str
                    .parse()
                    .with_context(|| format!("Could not parse window index '{window_str}'"))?;

                let (mut conn, _) = open_single_connection(single.connection).await?;
                conn.send(&vex_proto::Command::AgentSpawnInPlace {
                    workstream_id: ws_id,
                    tmux_window,
                    prompt: prompt.clone(),
                })
                .await?;
                let resp: vex_proto::Response = conn.recv().await?;
                match resp {
                    vex_proto::Response::AgentSpawnedInPlace { agent, exec_cmd } => {
                        println!("Registered as agent {}. Launching...", agent.id);
                        use std::os::unix::process::CommandExt;
                        let e = std::process::Command::new("sh")
                            .arg("-c")
                            .arg(&exec_cmd)
                            .exec();
                        anyhow::bail!("exec failed: {e}");
                    }
                    vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                    other => anyhow::bail!("Unexpected: {other:?}"),
                }
            }
        }
        AgentCmd::Kill { agent_id, single } => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::AgentKill {
                agent_id: agent_id.clone(),
            })
            .await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::AgentKilled => {
                    println!("Killed agent {agent_id}.");
                }
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
        AgentCmd::List {
            workstream_id,
            single,
        } => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::AgentList {
                workstream_id: workstream_id.clone(),
            })
            .await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::AgentList(agents) => {
                    if agents.is_empty() {
                        println!("No agents in {workstream_id}.");
                        return Ok(());
                    }
                    println!("{:<14} {:<35} {:<10} SPAWNED", "ID", "PROMPT", "STATUS");
                    for a in &agents {
                        let status = format!("{:?}", a.status);
                        let prompt = if a.prompt.len() > 33 {
                            format!("{}…", &a.prompt[..32])
                        } else {
                            a.prompt.clone()
                        };
                        let ago = tui::app::format_ago(a.spawned_at);
                        println!("{:<14} {:<35} {:<10} {ago}", a.id, prompt, status);
                    }
                }
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
    }
    Ok(())
}

// ── Tabular helpers ───────────────────────────────────────────────────────────

fn print_workstream_table(repos: &[vex_proto::Repository]) {
    let total: usize = repos.iter().map(|r| r.workstreams.len()).sum();
    if total == 0 {
        println!("No workstreams. Run 'vex workstream create' to add one.");
        return;
    }

    println!(
        "{:<12} {:<12} {:<20} {:<20} {:<10} {:<8} SHELL",
        "REPO", "ID", "NAME", "BRANCH", "STATUS", "AGENTS"
    );
    for repo in repos {
        for ws in &repo.workstreams {
            let running: usize = ws
                .agents
                .iter()
                .filter(|a| a.status == vex_proto::AgentStatus::Running)
                .count();
            let status = format!("{:?}", ws.status);
            println!(
                "{:<12} {:<12} {:<20} {:<20} {:<10} {:<8} ✓",
                repo.name, ws.id, ws.name, ws.branch, status, running
            );
        }
    }
}

// ── Connection helpers ────────────────────────────────────────────────────────

/// Open a single connection: try Unix socket first, then named/default config.
/// Returns `(Connection, display_label)`.
async fn open_single_connection(name: Option<String>) -> Result<(Connection, String)> {
    // If a name was given, use config
    if let Some(ref n) = name {
        let mut cfg = Config::load()?;
        let entry = cfg
            .connections
            .get_mut(n)
            .with_context(|| format!("Unknown connection '{n}'. Run 'vex list'."))?;
        let conn = Connection::from_entry(entry).await?;
        return Ok((conn, n.clone()));
    }

    // Try local Unix socket
    let socket_path = default_socket_path();
    if tokio::net::UnixStream::connect(&socket_path).await.is_ok() {
        let mut entry = ConnectionEntry {
            transport: "unix".to_string(),
            unix_socket: Some(socket_path),
            ..Default::default()
        };
        let conn = Connection::from_entry(&mut entry).await?;
        return Ok((conn, "localhost".to_string()));
    }

    // Fall back to configured connections
    let mut cfg = Config::load()?;
    if cfg.connections.is_empty() {
        anyhow::bail!(
            "No vexd connection available.\n\
             Start the daemon with 'vexd start', or add a connection with 'vex connect'."
        );
    }

    // Prefer "default", then any
    let name = if cfg.connections.contains_key("default") {
        "default".to_string()
    } else {
        cfg.connections.keys().next().cloned().unwrap()
    };

    let entry = cfg.connections.get_mut(&name).unwrap();
    let conn = Connection::from_entry(entry).await?;
    Ok((conn, name))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn default_socket_path() -> String {
    vex_home()
        .join("daemon")
        .join("vexd.sock")
        .to_string_lossy()
        .to_string()
}

async fn tmux_display_message(fmt: &str) -> Result<String> {
    let out = tokio::process::Command::new("tmux")
        .arg("display-message")
        .arg("-p")
        .arg(fmt)
        .output()
        .await
        .context("Failed to run tmux display-message")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn read_line() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_line(&mut buf)
        .map_err(|e| anyhow::anyhow!("Failed to read stdin: {e}"))?;
    Ok(buf)
}
