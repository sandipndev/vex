mod cli;
mod config;
mod error;
mod git;
mod github;
mod repo;
mod tmux;
mod workstream;

use clap::Parser;
use cli::{Cli, Commands};

#[macro_export]
macro_rules! println_ok {
    ($($arg:tt)*) => {
        println!("\x1b[32m{}\x1b[0m", format!($($arg)*))
    };
}

#[macro_export]
macro_rules! println_info {
    ($($arg:tt)*) => {
        println!("\x1b[34m{}\x1b[0m", format!($($arg)*))
    };
}

#[macro_export]
macro_rules! println_err {
    ($($arg:tt)*) => {
        eprintln!("\x1b[31m{}\x1b[0m", format!($($arg)*))
    };
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Init => cmd_init(),
        Commands::New { branch, repo } => workstream::create(repo.as_deref(), &branch),
        Commands::Attach { branch, repo } => workstream::attach(repo.as_deref(), &branch),
        Commands::Rm { branch, repo } => workstream::remove(repo.as_deref(), &branch),
        Commands::List { repo } => workstream::list(repo.as_deref()),
        Commands::Repos => cmd_repos(),
        Commands::Config => config::open_config_in_editor(),
    };

    if let Err(e) = result {
        println_err!("Error: {e}");
        std::process::exit(1);
    }
}

fn cmd_init() -> Result<(), error::VexError> {
    config::ensure_vex_dirs()?;
    let _ = config::Config::load_or_create()?;
    let meta = repo::init_repo()?;
    println_ok!("Registered repo '{}' at {}", meta.name, meta.path);
    println_ok!("Default branch: {}", meta.default_branch);
    Ok(())
}

fn cmd_repos() -> Result<(), error::VexError> {
    let repos = repo::list_repos()?;
    if repos.is_empty() {
        println_info!("No repos registered. Run `vex init` in a git repo.");
        return Ok(());
    }
    for r in &repos {
        println!(
            "{} ({}) [{}] - {} workstream(s)",
            r.name,
            r.path,
            r.default_branch,
            r.workstreams.len()
        );
    }
    Ok(())
}
