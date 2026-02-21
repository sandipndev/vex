mod config;
mod connect;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use config::{Config, ConnectionEntry};
use connect::Connection;

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "vex", about = "Vex client — connects to one or more vexd daemons")]
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
                    anyhow::anyhow!(
                        "Cannot reach vexd at {socket_path} — is it running? ({e})"
                    )
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

            let (token_id, token_secret) = pairing
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!(
                    "Invalid pairing token — expected <token_id>:<secret>"
                ))?;

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
            _ => entry.unix_socket.as_deref().unwrap_or("~/.vexd/vexd.sock").to_string(),
        };
        println!("{marker} {name:<20} {} → {target}", entry.transport);
    }
    Ok(())
}

// ── Generic command runner ────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum DaemonCmd {
    Status,
    Whoami,
}

async fn run_cmd(conn: &mut Connection, cmd: DaemonCmd) -> Result<()> {
    match cmd {
        DaemonCmd::Status => run_status(conn).await,
        DaemonCmd::Whoami => run_whoami(conn).await,
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
                    if let Err(e) = run_cmd(&mut conn, cmd).await {
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
