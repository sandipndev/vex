mod cli;
mod config;
mod error;
mod git;
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
        Commands::New { branch, repo } => workstream::create(repo.as_deref(), &branch),
        Commands::Switch { branch, repo } => workstream::switch(repo.as_deref(), branch.as_deref()),
        Commands::Rm { branch, repo } => workstream::remove(repo.as_deref(), &branch),
        Commands::List { repo } => workstream::list(repo.as_deref()),
        Commands::Open => {
            println_info!("TUI available shortly.");
            Ok(())
        }
        Commands::Exit => workstream::exit(),
        Commands::Rth { branch, repo } => workstream::rth(repo.as_deref(), branch.as_deref()),
        Commands::Status { branch, repo } => workstream::status(repo.as_deref(), branch.as_deref()),
        Commands::Completions { shell } => cmd_completions(shell),
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
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "vex", &mut std::io::stdout());
    Ok(())
}
