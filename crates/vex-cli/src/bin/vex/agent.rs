use std::io::Write;

use anyhow::{Result, bail};
use tokio::io;
use tokio::net::TcpStream;
use uuid::Uuid;
use vex_cli::proto::{ClientMessage, Frame, ServerMessage, read_frame, send_client_message};

async fn connect(port: u16) -> Result<TcpStream> {
    TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| {
        anyhow::anyhow!(
            "could not connect to daemon on port {}: {} (is the daemon running?)",
            port,
            e
        )
    })
}

async fn request(port: u16, msg: &ClientMessage) -> Result<ServerMessage> {
    let stream = connect(port).await?;
    let (mut reader, mut writer) = io::split(stream);

    send_client_message(&mut writer, msg).await?;

    match read_frame(&mut reader).await? {
        Some(Frame::Control(data)) => {
            let resp: ServerMessage = serde_json::from_slice(&data)?;
            Ok(resp)
        }
        Some(Frame::Data(_)) => bail!("unexpected data frame"),
        None => bail!("server closed connection"),
    }
}

pub async fn agent_create(
    port: u16,
    model: Option<String>,
    permission_mode: Option<String>,
    allowed_tools: Vec<String>,
    max_turns: Option<u32>,
    cwd: Option<String>,
) -> Result<()> {
    let resp = request(
        port,
        &ClientMessage::CreateAgent {
            model,
            permission_mode,
            allowed_tools,
            max_turns,
            cwd,
        },
    )
    .await?;
    match resp {
        ServerMessage::AgentCreated { id } => {
            println!("{}", id);
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn agent_prompt(port: u16, id_prefix: &str, prompt: &str) -> Result<()> {
    let id = resolve_agent_id(port, id_prefix).await?;

    let stream = connect(port).await?;
    let (mut reader, mut writer) = io::split(stream);

    send_client_message(
        &mut writer,
        &ClientMessage::AgentPrompt {
            id,
            prompt: prompt.to_string(),
        },
    )
    .await?;

    // Stream events until AgentPromptDone
    let mut stdout = std::io::stdout().lock();
    loop {
        match read_frame(&mut reader).await? {
            Some(Frame::Control(data)) => {
                let msg: ServerMessage = serde_json::from_slice(&data)?;
                match msg {
                    ServerMessage::AgentOutput { event, .. } => {
                        render_agent_event(&event.event_type, &event.raw_json, &mut stdout)?;
                    }
                    ServerMessage::AgentPromptDone { turn_count, .. } => {
                        let _ = stdout.flush();
                        eprintln!("\n[prompt done, turn {}]", turn_count);
                        break;
                    }
                    ServerMessage::Error { message } => {
                        let _ = stdout.flush();
                        bail!("{}", message);
                    }
                    _ => {}
                }
            }
            Some(Frame::Data(_)) => {}
            None => {
                let _ = stdout.flush();
                bail!("server closed connection");
            }
        }
    }

    Ok(())
}

fn render_agent_event(event_type: &str, raw_json: &str, out: &mut impl Write) -> Result<()> {
    match event_type {
        "content_block_delta" => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw_json)
                && let Some(text) = v
                    .get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(|t| t.as_str())
            {
                write!(out, "{}", text)?;
                out.flush()?;
            }
        }
        "result" => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw_json)
                && let Some(text) = v.get("result").and_then(|r| r.as_str())
            {
                write!(out, "{}", text)?;
                out.flush()?;
            }
        }
        "error" => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw_json)
                && let Some(msg) = v.get("error").and_then(|e| e.as_str())
            {
                writeln!(out, "\nerror: {}", msg)?;
                out.flush()?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub async fn agent_status(port: u16, id_prefix: &str) -> Result<()> {
    let id = resolve_agent_id(port, id_prefix).await?;
    let resp = request(port, &ClientMessage::AgentStatus { id }).await?;
    match resp {
        ServerMessage::AgentStatusResponse {
            id,
            status,
            claude_session_id,
            model,
            turn_count,
        } => {
            println!("Agent:      {}", id);
            println!("Status:     {}", status);
            println!("Model:      {}", model.as_deref().unwrap_or("(default)"));
            println!("Turns:      {}", turn_count);
            println!(
                "Session ID: {}",
                claude_session_id.as_deref().unwrap_or("(none)")
            );
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn agent_list(port: u16) -> Result<()> {
    let resp = request(port, &ClientMessage::ListAgents).await?;
    match resp {
        ServerMessage::Agents { agents } => {
            if agents.is_empty() {
                println!("no agents");
            } else {
                println!(
                    "{:<36}  {:<12}  {:<10}  {:>5}  CREATED",
                    "ID", "STATUS", "MODEL", "TURNS"
                );
                for a in agents {
                    let status_str = truncate_status(&a.status.to_string(), 12);
                    println!(
                        "{:<36}  {:<12}  {:<10}  {:>5}  {}",
                        a.id,
                        status_str,
                        a.model.as_deref().unwrap_or("(default)"),
                        a.turn_count,
                        a.created_at.format("%Y-%m-%d %H:%M:%S")
                    );
                }
            }
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn agent_kill(port: u16, id_prefix: &str) -> Result<()> {
    let id = resolve_agent_id(port, id_prefix).await?;
    let resp = request(port, &ClientMessage::KillAgent { id }).await?;
    match resp {
        ServerMessage::Error { message } => bail!("{}", message),
        _ => {
            println!("killed agent {}", id);
            Ok(())
        }
    }
}

fn truncate_status(s: &str, max: usize) -> String {
    // Take only the first line for table display
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() <= max {
        first_line.to_string()
    } else {
        format!("{}…", &first_line[..max - 1])
    }
}

async fn resolve_agent_id(port: u16, prefix: &str) -> Result<Uuid> {
    if let Ok(id) = prefix.parse::<Uuid>() {
        return Ok(id);
    }

    let resp = request(port, &ClientMessage::ListAgents).await?;
    match resp {
        ServerMessage::Agents { agents } => {
            let matches: Vec<_> = agents
                .iter()
                .filter(|a| a.id.to_string().starts_with(prefix))
                .collect();
            match matches.len() {
                0 => bail!("no agent matching prefix '{}'", prefix),
                1 => Ok(matches[0].id),
                n => bail!("ambiguous prefix '{}' matches {} agents", prefix, n),
            }
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}
