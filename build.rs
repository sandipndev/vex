use clap::CommandFactory;
use clap_complete::{Shell, generate_to};
use std::env;
use std::io::Error;

#[path = "src/cli.rs"]
mod cli;

fn main() -> Result<(), Error> {
    let version = std::process::Command::new("git")
        .args(["describe", "--tags", "--always"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|v| v.trim().trim_start_matches('v').to_string())
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap());
    println!("cargo::rustc-env=VEX_VERSION={version}");

    let outdir = match env::var_os("VEX_COMPLETIONS_DIR") {
        Some(dir) => dir.into(),
        None => {
            let outdir = env::var_os("OUT_DIR").unwrap();
            std::path::PathBuf::from(outdir)
        }
    };

    let mut cmd = cli::Cli::command();
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        generate_to(shell, &mut cmd, "vex", &outdir)?;
    }

    println!("cargo::rerun-if-changed=src/cli.rs");
    Ok(())
}
