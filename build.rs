use clap::CommandFactory;
use clap_complete::{Shell, generate_to};
use std::env;
use std::io::Error;

#[path = "src/cli.rs"]
mod cli;

fn main() -> Result<(), Error> {
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
