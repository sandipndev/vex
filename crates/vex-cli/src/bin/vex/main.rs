use vex_cli as vex_proto;

mod config;
mod connect;
mod tui;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use tokio::sync::Mutex;
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
    /// Attach to a workstream's shell (local: tmux exec; remote: PTY stream)
    Attach(AttachArgs),
    /// Start background connection manager (required for remote TCP connections)
    Start(StartManagerArgs),
    /// Stop the background connection manager
    Stop,
    /// Print shell completion script
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
    /// [internal] Background proxy daemon — do not call directly
    #[command(hide = true)]
    ProxyRun(ProxyRunArgs),
    /// Shell commands
    Shell {
        #[command(subcommand)]
        action: ShellCmd,
    },
    /// [internal] Shell PTY supervisor — spawned by vexd inside tmux windows
    #[command(hide = true, name = "shell-supervisor")]
    ShellSupervisor(ShellSupervisorArgs),
}

// ── Subcommand structs ────────────────────────────────────────────────────────

#[derive(Args)]
struct StartManagerArgs {
    /// Named TCP connection to proxy (default: first TCP connection in config)
    #[arg(long, short = 'c')]
    connection: Option<String>,
}

#[derive(Args)]
struct ProxyRunArgs {
    /// Named TCP connection to proxy
    #[arg(long, short = 'c')]
    connection: Option<String>,
}

#[derive(Args)]
struct AttachArgs {
    /// Workstream ID to attach to
    workstream_id: String,
    /// Shell ID to attach (default: first active shell in workstream)
    #[arg(long)]
    shell: Option<String>,
    #[command(flatten)]
    single: Single,
}

// ── Shell subcommands ─────────────────────────────────────────────────────────

#[derive(Subcommand)]
enum ShellCmd {
    /// List shell sessions for a workstream
    List {
        workstream_id: String,
        #[command(flatten)]
        single: Single,
    },
    /// Spawn a new shell window in a workstream
    Spawn {
        workstream_id: String,
        #[command(flatten)]
        single: Single,
    },
    /// Kill a shell session
    Kill {
        shell_id: String,
        #[command(flatten)]
        single: Single,
    },
}

#[derive(Args)]
struct ShellSupervisorArgs {
    /// Workstream ID this shell belongs to
    #[arg(long)]
    workstream: String,
    /// tmux window index in the workstream's session
    #[arg(long)]
    window: u32,
}

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
        Some(Commands::Attach(args)) => cmd_attach(args).await,
        Some(Commands::Start(args)) => cmd_start(args).await,
        Some(Commands::Stop) => cmd_stop(),
        Some(Commands::ProxyRun(args)) => cmd_proxy_run(args).await,
        Some(Commands::Shell { action }) => cmd_shell_command(action).await,
        Some(Commands::ShellSupervisor(args)) => cmd_shell_supervisor(args).await,
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
                // No named connections — auto-detect proxy socket or local Unix socket
                let (mut conn, label) = open_single_connection(None).await?;
                let result = run_cmd(&mut conn, cmd).await;
                match result {
                    Ok(line) => println!("[{label}] {line}"),
                    Err(e) => anyhow::bail!("{e:#}"),
                }
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
                    println!("{:<14} {:<20} {:<15} PATH", "ID", "NAME", "DEFAULT_BRANCH");
                    for repo in &repos {
                        println!(
                            "{:<14} {:<20} {:<15} {}",
                            repo.id, repo.name, repo.default_branch, repo.path
                        );
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
                        // Security: verify the command vexd told us to exec starts
                        // with the same binary as our locally configured agent command.
                        // This prevents a compromised/malicious vexd from exec'ing
                        // arbitrary commands in the user's terminal.
                        let user_cfg =
                            vex_cli::user_config::UserConfig::load(&vex_home());
                        let expected_cmd = user_cfg.agent_command();
                        let expected_bin =
                            expected_cmd.split_whitespace().next().unwrap_or("");
                        let actual_bin =
                            exec_cmd.split_whitespace().next().unwrap_or("");
                        if actual_bin != expected_bin {
                            anyhow::bail!(
                                "daemon returned an unexpected exec command \
                                 (expected binary '{}', got '{}'). Aborting.",
                                expected_bin,
                                actual_bin
                            );
                        }
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

// ── Attach command ────────────────────────────────────────────────────────────

async fn cmd_attach(args: AttachArgs) -> Result<()> {
    // Local fast-path: if no named connection is given and local vexd is
    // reachable, exec into `tmux attach-session` directly.
    if args.single.connection.is_none() {
        let sock = default_socket_path();
        if let Ok(stream) = tokio::net::UnixStream::connect(&sock).await {
            let mut conn = Connection::Unix(stream);
            conn.send(&vex_proto::Command::WorkstreamList { repo_id: None })
                .await?;
            let repos: Vec<vex_proto::Repository> = match conn.recv::<vex_proto::Response>().await?
            {
                vex_proto::Response::WorkstreamList(r) => r,
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            };
            let ws = repos
                .iter()
                .flat_map(|r| r.workstreams.iter())
                .find(|w| w.id == args.workstream_id)
                .ok_or_else(|| anyhow::anyhow!("Workstream '{}' not found", args.workstream_id))?;
            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new("tmux")
                .arg("attach-session")
                .arg("-t")
                .arg(&ws.tmux_session)
                .exec();
            return Err(anyhow::anyhow!("exec tmux failed: {err}"));
        }
    }

    // Remote path: attach via PTY streaming
    let (mut conn, _) = open_single_connection(args.single.connection).await?;

    conn.send(&vex_proto::Command::WorkstreamList { repo_id: None })
        .await?;
    let repos: Vec<vex_proto::Repository> = match conn.recv::<vex_proto::Response>().await? {
        vex_proto::Response::WorkstreamList(r) => r,
        vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("Unexpected: {other:?}"),
    };
    let ws = repos
        .iter()
        .flat_map(|r| r.workstreams.iter())
        .find(|w| w.id == args.workstream_id)
        .ok_or_else(|| anyhow::anyhow!("Workstream '{}' not found", args.workstream_id))?;

    let shell_id = if let Some(id) = args.shell {
        id
    } else {
        ws.shells
            .iter()
            .find(|s| s.status != vex_proto::ShellStatus::Exited)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No active shell in workstream '{}'. \
                     Shells may still be starting up.",
                    ws.id
                )
            })?
            .id
            .clone()
    };

    conn.send(&vex_proto::Command::AttachShell {
        shell_id: shell_id.clone(),
    })
    .await?;
    let resp: vex_proto::Response = conn.recv().await?;
    match resp {
        vex_proto::Response::ShellAttached => {}
        vex_proto::Response::Error(e) => anyhow::bail!("Attach failed: {e:?}"),
        other => anyhow::bail!("Unexpected: {other:?}"),
    }

    // Split stream for bidirectional ShellMsg streaming
    match conn {
        Connection::Unix(stream) => {
            let (r, w) = tokio::io::split(stream);
            pty_attach_remote(r, w).await
        }
        Connection::Tcp(stream) => {
            let (r, w) = tokio::io::split(*stream);
            pty_attach_remote(r, w).await
        }
    }
}

/// Stream PTY data between the remote shell and the local terminal.
///
/// Expects the handshake (AttachShell / ShellAttached) to already be done.
/// Puts the terminal in raw mode for the duration and restores it on exit.
async fn pty_attach_remote<R, W>(mut net_read: R, mut net_write: W) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use base64::Engine;
    use tokio::io::AsyncWriteExt;

    let b64 = base64::engine::general_purpose::STANDARD;

    // Send our current terminal size so the remote PTY resizes to match
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    vex_proto::framing::send(&mut net_write, &vex_proto::ShellMsg::Resize { cols, rows }).await?;

    // Stdin reader thread → mpsc channel (raw bytes)
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdin_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // SIGWINCH → terminal resize events (Unix-only tool)
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;

    // Put terminal in raw mode; best-effort restore on all exit paths
    crossterm::terminal::enable_raw_mode()?;

    let mut stdout = tokio::io::stdout();
    let exit_code: Option<i32>;

    macro_rules! cleanup_return {
        ($e:expr) => {{
            crossterm::terminal::disable_raw_mode().ok();
            return $e;
        }};
    }

    loop {
        tokio::select! {
            biased;

            // Output / exit from the remote shell
            msg_result = vex_proto::framing::recv::<_, vex_proto::ShellMsg>(&mut net_read) => {
                match msg_result {
                    Err(e) => cleanup_return!(Err(e.into())),
                    Ok(vex_proto::ShellMsg::Out { data }) => {
                        match b64.decode(&data) {
                            Ok(bytes) => {
                                if let Err(e) = stdout.write_all(&bytes).await {
                                    cleanup_return!(Err(e.into()));
                                }
                                let _ = stdout.flush().await;
                            }
                            Err(e) => cleanup_return!(Err(e.into())),
                        }
                    }
                    Ok(vex_proto::ShellMsg::Exited { code }) => {
                        exit_code = code;
                        break;
                    }
                    Ok(_) => {}
                }
            }

            // Keyboard input → remote shell
            Some(bytes) = stdin_rx.recv() => {
                let encoded = b64.encode(&bytes);
                if let Err(e) = vex_proto::framing::send(
                    &mut net_write,
                    &vex_proto::ShellMsg::In { data: encoded },
                ).await {
                    cleanup_return!(Err(e.into()));
                }
            }

            // Terminal resize (SIGWINCH)
            _ = sigwinch.recv() => {
                if let Ok((c, r)) = crossterm::terminal::size() {
                    let _ = vex_proto::framing::send(
                        &mut net_write,
                        &vex_proto::ShellMsg::Resize { cols: c, rows: r },
                    ).await;
                }
            }
        }
    }

    crossterm::terminal::disable_raw_mode().ok();

    if let Some(code) = exit_code
        && code != 0
    {
        std::process::exit(code);
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

/// Open a single connection: try Unix socket first, then proxy socket, then config.
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

    // Try local Unix socket (direct connection to a local vexd)
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

    // Try proxy socket created by `vex start` (for TCP connections)
    let proxy_sock = proxy_sock_path();
    if tokio::net::UnixStream::connect(&proxy_sock).await.is_ok() {
        let mut entry = ConnectionEntry {
            transport: "unix".to_string(),
            unix_socket: Some(proxy_sock),
            ..Default::default()
        };
        let conn = Connection::from_entry(&mut entry).await?;
        return Ok((conn, "proxy".to_string()));
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

// ── Background connection manager ────────────────────────────────────────────

async fn cmd_start(args: StartManagerArgs) -> Result<()> {
    // 1. Check if already running
    if let Some(pid) = read_vex_pid()
        && vex_pid_is_alive(pid)
    {
        println!("vex already running (pid {pid})");
        return Ok(());
    }

    // 2. Validate a TCP connection exists in config
    let cfg = Config::load()?;
    let (name, _) = find_tcp_entry(&cfg, args.connection.as_deref())?;

    // 3. Spawn detached proxy-run child
    let exe = std::env::current_exe().context("cannot find own executable")?;
    let home = vex_home();
    std::fs::create_dir_all(&home)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("vex-proxy.log"))
        .context("opening vex-proxy.log")?;

    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("proxy-run");
    if let Some(ref c) = args.connection {
        cmd.args(["--connection", c]);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);

    // Detach from terminal: start in new session so it survives shell exit
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let child = cmd.spawn().context("failed to spawn vex proxy")?;
    let pid = child.id();
    drop(child); // detach without waiting

    // Brief pause to let the child write its PID file and bind the socket
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    if vex_pid_is_alive(pid) {
        println!("vex started — proxying '{name}' (pid {pid})");
    } else {
        anyhow::bail!(
            "vex proxy failed to start — check {}/vex-proxy.log",
            home.display()
        );
    }
    Ok(())
}

fn cmd_stop() -> Result<()> {
    let pid_path = vex_pid_file();
    let Some(pid) = read_vex_pid() else {
        println!("vex not running");
        return Ok(());
    };
    if !vex_pid_is_alive(pid) {
        println!("vex not running (stale pid file)");
        let _ = std::fs::remove_file(&pid_path);
        return Ok(());
    }
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !vex_pid_is_alive(pid) {
            let _ = std::fs::remove_file(&pid_path);
            println!("vex stopped");
            return Ok(());
        }
    }
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(200));
    let _ = std::fs::remove_file(&pid_path);
    println!("vex killed");
    Ok(())
}

async fn cmd_proxy_run(args: ProxyRunArgs) -> Result<()> {
    let cfg = Config::load()?;
    let (name, entry) = find_tcp_entry(&cfg, args.connection.as_deref())?;

    // Write own PID file
    let home = vex_home();
    std::fs::create_dir_all(&home)?;
    let pid = std::process::id();
    std::fs::write(vex_pid_file(), pid.to_string())?;

    // Remove stale socket if any
    let sock = proxy_sock_path();
    let _ = std::fs::remove_file(&sock);

    eprintln!("vex proxy started (pid {pid}), proxying '{name}'");

    run_proxy(entry, sock).await
}

/// State shared across all proxy client handlers.
struct ProxyState {
    entry: ConnectionEntry,
    conn: Option<Connection>,
    /// Current backoff duration before the next reconnect attempt.
    backoff: std::time::Duration,
    /// Don't try to reconnect until this instant.
    next_attempt: Option<std::time::Instant>,
}

impl ProxyState {
    fn new(entry: ConnectionEntry) -> Self {
        Self {
            entry,
            conn: None,
            backoff: std::time::Duration::from_secs(1),
            next_attempt: None,
        }
    }

    /// Execute a command through the persistent vexd connection.
    /// Reconnects (with backoff) if the connection is absent or broken.
    async fn execute(&mut self, cmd: &vex_proto::Command) -> Result<vex_proto::Response> {
        // Ensure we have a connection
        if self.conn.is_none() {
            if let Some(until) = self.next_attempt {
                let now = std::time::Instant::now();
                if now < until {
                    tokio::time::sleep(until - now).await;
                }
            }
            let mut entry = self.entry.clone();
            match Connection::from_entry(&mut entry).await {
                Ok(c) => {
                    self.conn = Some(c);
                    self.backoff = std::time::Duration::from_secs(1);
                    self.next_attempt = None;
                    eprintln!("vex proxy: connected to vexd");
                }
                Err(e) => {
                    self.next_attempt = Some(std::time::Instant::now() + self.backoff);
                    self.backoff = (self.backoff * 2).min(std::time::Duration::from_secs(30));
                    return Err(e);
                }
            }
        }

        let conn = self.conn.as_mut().unwrap();
        let result: Result<vex_proto::Response> = async {
            conn.send(cmd).await?;
            conn.recv().await
        }
        .await;

        match result {
            Ok(resp) => Ok(resp),
            Err(e) => {
                // Mark connection dead; next request will reconnect
                self.conn = None;
                Err(e)
            }
        }
    }
}

async fn run_proxy(entry: ConnectionEntry, sock_path: String) -> Result<()> {
    let listener =
        tokio::net::UnixListener::bind(&sock_path).with_context(|| format!("bind {sock_path}"))?;
    // Restrict proxy socket to owner-only so other local users cannot connect
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;
    }

    let state = Arc::new(Mutex::new(ProxyState::new(entry)));

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    loop {
        tokio::select! {
            res = listener.accept() => {
                let (client, _) = res?;
                let st = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_proxy_client(client, st).await {
                        eprintln!("proxy client error: {e:#}");
                    }
                });
            }
            _ = sigterm.recv() => {
                eprintln!("vex proxy: received SIGTERM, shutting down");
                break;
            }
            _ = sigint.recv() => {
                eprintln!("vex proxy: received SIGINT, shutting down");
                break;
            }
        }
    }

    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(vex_pid_file());
    Ok(())
}

async fn handle_proxy_client(
    mut client: tokio::net::UnixStream,
    state: Arc<Mutex<ProxyState>>,
) -> Result<()> {
    // Read command from client using the shared framing protocol
    let cmd: vex_proto::Command = vex_proto::framing::recv(&mut client).await?;

    // Forward to vexd
    let resp = state.lock().await.execute(&cmd).await;

    // Always send a framed response back (errors become protocol-level errors)
    let resp = resp.unwrap_or_else(|e| {
        vex_proto::Response::Error(vex_proto::VexProtoError::Internal(e.to_string()))
    });
    vex_proto::framing::send(&mut client, &resp).await?;

    Ok(())
}

// ── Background manager helpers ────────────────────────────────────────────────

fn vex_pid_file() -> std::path::PathBuf {
    vex_home().join("vex.pid")
}

fn read_vex_pid() -> Option<u32> {
    std::fs::read_to_string(vex_pid_file())
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn vex_pid_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn proxy_sock_path() -> String {
    vex_home()
        .join("vex-client.sock")
        .to_string_lossy()
        .to_string()
}

/// Find a TCP connection entry in config.
/// Prefers `name` if given, then "default", then any TCP entry.
fn find_tcp_entry(cfg: &Config, name: Option<&str>) -> Result<(String, ConnectionEntry)> {
    if let Some(n) = name {
        let entry = cfg
            .connections
            .get(n)
            .with_context(|| format!("Connection '{n}' not found. Run 'vex list'."))?;
        if entry.transport != "tcp" {
            anyhow::bail!("Connection '{n}' is not a TCP connection");
        }
        return Ok((n.to_string(), entry.clone()));
    }

    // Prefer "default" if it's TCP
    if let Some(entry) = cfg.connections.get("default")
        && entry.transport == "tcp"
    {
        return Ok(("default".to_string(), entry.clone()));
    }

    // Fall back to first TCP entry
    for (n, entry) in &cfg.connections {
        if entry.transport == "tcp" {
            return Ok((n.clone(), entry.clone()));
        }
    }

    anyhow::bail!("No TCP connection found. Run 'vex connect --host <host:port>' to add one.")
}

// ── Shell subcommand dispatch ──────────────────────────────────────────────────

async fn cmd_shell_command(action: ShellCmd) -> Result<()> {
    match action {
        ShellCmd::List {
            workstream_id,
            single,
        } => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::ShellList {
                workstream_id: workstream_id.clone(),
            })
            .await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::ShellList(shells) => {
                    if shells.is_empty() {
                        println!("No shells in {workstream_id}.");
                        return Ok(());
                    }
                    println!("{:<14} {:<10} {:<8} STARTED", "ID", "STATUS", "WINDOW");
                    for s in &shells {
                        let status = format!("{:?}", s.status);
                        let ago = tui::app::format_ago(s.started_at);
                        println!("{:<14} {:<10} {:<8} {ago}", s.id, status, s.tmux_window);
                    }
                }
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
        ShellCmd::Spawn {
            workstream_id,
            single,
        } => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::ShellSpawn {
                workstream_id: workstream_id.clone(),
            })
            .await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::ShellSpawned(s) => {
                    println!("Spawned shell {} in tmux window {}", s.id, s.tmux_window);
                }
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
        ShellCmd::Kill { shell_id, single } => {
            let (mut conn, _) = open_single_connection(single.connection).await?;
            conn.send(&vex_proto::Command::ShellKill {
                shell_id: shell_id.clone(),
            })
            .await?;
            let resp: vex_proto::Response = conn.recv().await?;
            match resp {
                vex_proto::Response::ShellKilled => {
                    println!("Killed shell {shell_id}.");
                }
                vex_proto::Response::Error(e) => anyhow::bail!("{e:?}"),
                other => anyhow::bail!("Unexpected: {other:?}"),
            }
        }
    }
    Ok(())
}

// ── Shell PTY supervisor ──────────────────────────────────────────────────────

async fn cmd_shell_supervisor(args: ShellSupervisorArgs) -> Result<()> {
    use base64::Engine;
    use std::io::{Read as _, Write as _};

    let b64 = base64::engine::general_purpose::STANDARD;

    // Connect directly to the local vexd Unix socket
    let sock = default_socket_path();
    let stream = tokio::net::UnixStream::connect(&sock)
        .await
        .with_context(|| format!("cannot reach vexd at {sock}"))?;
    let (mut net_read, mut net_write) = tokio::io::split(stream);

    // Register this shell session with vexd
    vex_proto::framing::send(
        &mut net_write,
        &vex_proto::Command::ShellRegister {
            workstream_id: args.workstream.clone(),
            tmux_window: args.window,
        },
    )
    .await?;

    let resp: vex_proto::Response = vex_proto::framing::recv(&mut net_read).await?;
    let shell_id = match resp {
        vex_proto::Response::ShellRegistered { shell_id } => shell_id,
        vex_proto::Response::Error(e) => anyhow::bail!("ShellRegister failed: {e:?}"),
        other => anyhow::bail!("Unexpected response to ShellRegister: {other:?}"),
    };

    // Determine which shell binary to run
    let user_cfg = vex_cli::user_config::UserConfig::load(&vex_home());
    let shell_bin = user_cfg.shell_binary();

    // Open a PTY pair
    let pty_system = portable_pty::native_pty_system();
    let pty_pair = pty_system.openpty(portable_pty::PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    // Spawn the shell process inside the PTY slave
    let mut cmd_builder = portable_pty::CommandBuilder::new(&shell_bin);
    cmd_builder.env("TERM", "xterm-256color");
    let mut child = pty_pair.slave.spawn_command(cmd_builder)?;
    drop(pty_pair.slave); // Drop slave fd after spawn

    // Get writer before cloning the reader (take_writer consumes the write side)
    let mut pty_writer = pty_pair.master.take_writer()?;

    // Background thread: read PTY output → mpsc channel
    let (pty_out_tx, mut pty_out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    {
        let mut reader = pty_pair.master.try_clone_reader()?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if pty_out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
    // Check-child-exited channel
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::channel::<Option<i32>>(1);
    std::thread::spawn(move || {
        let code = child.wait().ok().map(|s| s.exit_code() as i32);
        let _ = exit_tx.blocking_send(code);
    });

    // Main event loop
    loop {
        tokio::select! {
            biased;

            // Shell process exited
            Some(code) = exit_rx.recv() => {
                // Drain remaining PTY output
                while let Ok(data) = pty_out_rx.try_recv() {
                    let encoded = b64.encode(&data);
                    let _ = vex_proto::framing::send(
                        &mut net_write,
                        &vex_proto::ShellMsg::Out { data: encoded },
                    ).await;
                }
                let _ = vex_proto::framing::send(
                    &mut net_write,
                    &vex_proto::ShellMsg::Exited { code },
                ).await;
                break;
            }

            // PTY produced output → forward to vexd
            Some(data) = pty_out_rx.recv() => {
                let encoded = b64.encode(&data);
                vex_proto::framing::send(
                    &mut net_write,
                    &vex_proto::ShellMsg::Out { data: encoded },
                )
                .await?;
            }

            // vexd sent a PTY message (input or resize)
            msg_result = vex_proto::framing::recv::<_, vex_proto::ShellMsg>(&mut net_read) => {
                match msg_result? {
                    vex_proto::ShellMsg::In { data } => {
                        let bytes = b64.decode(&data)?;
                        pty_writer.write_all(&bytes)?;
                    }
                    vex_proto::ShellMsg::Resize { cols, rows } => {
                        let _ = pty_pair.master.resize(portable_pty::PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    vex_proto::ShellMsg::Exited { .. } => break,
                    vex_proto::ShellMsg::Out { .. } => {} // unexpected
                }
            }
        }
    }

    drop(shell_id); // silence unused warning; used above in ShellRegister
    Ok(())
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
