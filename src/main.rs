mod cache;
mod cli;
mod config;
mod error;
mod git;
mod github;
mod repo;
mod tmux;
mod tui;
mod worker;
mod workstream;

use clap::{CommandFactory, FromArgMatches};
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
    let matches = Cli::command().version(env!("VEX_VERSION")).get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap();

    if config::is_first_run()
        && let None
        | Some(Commands::Open)
        | Some(Commands::New { .. })
        | Some(Commands::Switch { .. })
        | Some(Commands::Rm { .. })
        | Some(Commands::Exit)
        | Some(Commands::Rename { .. }) = &cli.command
    {
        println_info!("First run detected — running health check...\n");
        let _ = workstream::doctor();
        println!();
    }

    let result = match cli.command {
        None | Some(Commands::Open) => tui::run(),
        Some(Commands::New { branch, from, repo }) => {
            workstream::create(repo.as_deref(), &branch, from.as_deref())
        }
        Some(Commands::Switch { branch, repo }) => {
            workstream::switch(repo.as_deref(), branch.as_deref())
        }
        Some(Commands::Rm { branch, repo }) => workstream::remove(repo.as_deref(), &branch, false),
        Some(Commands::List { repo }) => workstream::list(repo.as_deref()),
        Some(Commands::Exit) => workstream::exit(),
        Some(Commands::Rth { branch, repo }) => workstream::rth(repo.as_deref(), branch.as_deref()),
        Some(Commands::Status { branch, repo }) => {
            workstream::status(repo.as_deref(), branch.as_deref())
        }
        Some(Commands::Rename {
            branch,
            new_branch,
            repo,
        }) => {
            let (old, new_name) = match &new_branch {
                Some(nb) => (Some(branch.as_str()), nb.as_str()),
                None => (None, branch.as_str()),
            };
            workstream::rename(repo.as_deref(), old, new_name, false)
        }
        Some(Commands::Doctor) => workstream::doctor(),
        Some(Commands::Completions { shell }) => cmd_completions(shell),
    };

    if let Err(e) = result {
        println_err!("Error: {e}");
        std::process::exit(1);
    }
}

fn cmd_completions(shell: cli::ShellChoice) -> Result<(), error::VexError> {
    use clap_complete::{Shell, generate};
    let shell = match shell {
        cli::ShellChoice::Bash => Shell::Bash,
        cli::ShellChoice::Zsh => Shell::Zsh,
        cli::ShellChoice::Fish => Shell::Fish,
    };
    let mut cmd = Cli::command().version(env!("VEX_VERSION"));
    generate(shell, &mut cmd, "vex", &mut std::io::stdout());
    Ok(())
}
