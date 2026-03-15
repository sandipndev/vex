use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use vex_cli::proto::{ClientMessage, ServerMessage};

use super::client::request;

/// Make a relative path absolute using the client's cwd.
/// The daemon will canonicalize (resolve symlinks, verify existence).
fn resolve_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

pub async fn repo_add(port: u16, name: &str, path: &Path) -> Result<()> {
    let path = resolve_path(path);
    let resp = request(
        port,
        &ClientMessage::RepoAdd {
            name: name.to_string(),
            path,
        },
    )
    .await?;
    match resp {
        ServerMessage::RepoAdded { name, path } => {
            println!("added repo '{}' at {}", name, path.display());
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn repo_remove(port: u16, name: &str) -> Result<()> {
    let resp = request(
        port,
        &ClientMessage::RepoRemove {
            name: name.to_string(),
        },
    )
    .await?;
    match resp {
        ServerMessage::RepoRemoved { name } => {
            println!("removed repo '{}'", name);
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn repo_list(port: u16) -> Result<()> {
    let resp = request(port, &ClientMessage::RepoList).await?;
    match resp {
        ServerMessage::Repos { repos } => {
            if repos.is_empty() {
                println!("no repos registered");
            } else {
                println!("{:<20}  PATH", "NAME");
                for r in repos {
                    println!("{:<20}  {}", r.name, r.path.display());
                }
            }
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn repo_introspect_path(port: u16, path: &Path) -> Result<()> {
    let path = resolve_path(path);
    let resp = request(port, &ClientMessage::RepoIntrospectPath { path }).await?;
    match resp {
        ServerMessage::RepoIntrospected {
            suggested_name,
            path,
            git_remote,
            git_branch,
        } => {
            println!("{:<16}  {}", "Name:", suggested_name);
            println!("{:<16}  {}", "Path:", path.display());
            if let Some(remote) = git_remote {
                println!("{:<16}  {}", "Git remote:", remote);
            }
            if let Some(branch) = git_branch {
                println!("{:<16}  {}", "Git branch:", branch);
            }
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}
