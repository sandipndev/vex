use vex_cli as vex_proto;

mod config;
mod connect;

use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use tokio::task::JoinSet;

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
    /// Remove a saved connection (-c <name> or --all)
    Disconnect(DisconnectArgs),
    /// List all saved connections
    List,
    /// Print daemon status for all connections (or one with -c)
    Status(Filter),
    /// Show who you are connected as for all connections (or one with -c)
    Whoami(Filter),
    /// Print shell completion script
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
}

#[derive(Args)]
struct ConnectArgs {
    /// Name for this connection
    #[arg(long, short = 'n', default_value = "default")]
    name: String,
    /// TCP host:port of a remote daemon (omit to use local Unix socket)
    #[arg(long)]
    host: Option<String>,
}

#[derive(Args)]
struct DisconnectArgs {
    /// Connection name to remove
    #[arg(long, short = 'c')]
    connection: Option<String>,
    /// Remove all saved connections
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct Filter {
    /// Limit to a single named connection (default: run against all)
    #[arg(long, short = 'c')]
    connection: Option<String>,
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
        Commands::Disconnect(args) => cmd_disconnect(args),
        Commands::List => cmd_list(),
        Commands::Status(filter) => cmd_with_connections(filter, DaemonCmd::Status).await,
        Commands::Whoami(filter) => cmd_with_connections(filter, DaemonCmd::Whoami).await,
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "vex", &mut std::io::stdout());
            Ok(())
        }
    }
}

// ── Command implementations ──────────────────────────────────────────────────

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
                .unwrap_or("~/.vexd/vexd.sock")
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

    // Determine which connections to target
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

    // Clone entries for parallel tasks (we'll merge TOFU updates back after)
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

    // Collect results; sort by name for stable output
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
                eprintln!("[{name}] error: {e}");
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

// ── Helpers ──────────────────────────────────────────────────────────────────

fn default_socket_path() -> String {
    dirs::home_dir()
        .map(|h| h.join(".vexd").join("vexd.sock"))
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
