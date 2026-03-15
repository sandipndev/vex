use anyhow::{Result, bail};
use vex_cli::proto::{ClientMessage, ServerMessage};

use super::client::request;

pub async fn workstream_create(port: u16, repo: &str, name: &str) -> Result<()> {
    let resp = request(
        port,
        &ClientMessage::WorkstreamCreate {
            repo: repo.to_string(),
            name: name.to_string(),
        },
    )
    .await?;
    match resp {
        ServerMessage::WorkstreamCreated {
            repo,
            name,
            worktree_path,
        } => {
            println!(
                "created workstream '{}' for repo '{}' at {}",
                name,
                repo,
                worktree_path.display()
            );
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn workstream_list(port: u16, repo: Option<&str>) -> Result<()> {
    let resp = request(
        port,
        &ClientMessage::WorkstreamList {
            repo: repo.map(String::from),
        },
    )
    .await?;
    match resp {
        ServerMessage::Workstreams { workstreams } => {
            if workstreams.is_empty() {
                println!("no workstreams");
            } else {
                println!("{:<15}  {:<20}  PATH", "REPO", "WORKSTREAM");
                for ws in workstreams {
                    println!(
                        "{:<15}  {:<20}  {}",
                        ws.repo,
                        ws.name,
                        ws.worktree_path.display()
                    );
                }
            }
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn workstream_remove(port: u16, repo: &str, name: &str) -> Result<()> {
    let resp = request(
        port,
        &ClientMessage::WorkstreamRemove {
            repo: repo.to_string(),
            name: name.to_string(),
        },
    )
    .await?;
    match resp {
        ServerMessage::WorkstreamRemoved { repo, name } => {
            println!("removed workstream '{}' from repo '{}'", name, repo);
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}
