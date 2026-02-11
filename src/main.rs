mod cli;
mod config;
mod error;
mod git;
mod github;
mod repo;
mod tmux;
mod workstream;

use clap::{CommandFactory, Parser};
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
        Commands::Sync { repo } => workstream::sync(repo.as_deref()),
        Commands::Status { branch, repo } => workstream::status(repo.as_deref(), branch.as_deref()),
        Commands::Pr { branch, repo } => workstream::pr(repo.as_deref(), branch.as_deref()),
        Commands::Open => workstream::open(),
        Commands::Repos => cmd_repos(),
        Commands::Config => config::open_config_in_editor(),
        Commands::Reload => cmd_reload(),
        Commands::Completions { shell } => cmd_completions(shell),
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

fn cmd_reload() -> Result<(), error::VexError> {
    let config = config::Config::load_or_create()?;
    println_ok!("Config reloaded successfully");
    println_info!(
        "{} window(s): {}",
        config.windows.len(),
        config
            .windows
            .iter()
            .map(|w| w.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !config.hooks.on_create.is_empty() {
        println_info!("on_create hooks: {}", config.hooks.on_create.join(", "));
    }
    Ok(())
}

fn cmd_completions(shell: cli::ShellChoice) -> Result<(), error::VexError> {
    use clap_complete::{Shell, generate};
    let shell = match shell {
        cli::ShellChoice::Bash => Shell::Bash,
        cli::ShellChoice::Zsh => Shell::Zsh,
        cli::ShellChoice::Fish => Shell::Fish,
    };
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "vex", &mut std::io::stdout());
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
