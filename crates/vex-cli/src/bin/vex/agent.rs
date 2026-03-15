use std::io::Write;

use anyhow::{Result, bail};
use serde_json::Value;
use tokio::io;
use uuid::Uuid;
use vex_cli::proto::{
    AgentEntry, ClientMessage, Frame, ServerMessage, read_frame, send_client_message,
};

use super::client::{connect, request};

fn print_agent_table(agents: &[AgentEntry]) {
    println!(
        "{:<36}  {:<12}  {:<6}  CWD",
        "VEX SESSION", "CLAUDE ID", "PID"
    );
    for a in agents {
        println!(
            "{:<36}  {:<12}  {:<6}  {}",
            a.vex_session_id,
            &a.claude_session_id[..a.claude_session_id.len().min(12)],
            a.claude_pid,
            a.cwd.display(),
        );
    }
}

pub async fn agent_list(port: u16) -> Result<()> {
    let resp = request(port, &ClientMessage::AgentList).await?;
    match resp {
        ServerMessage::AgentListResponse { agents } => {
            if agents.is_empty() {
                println!("no agents detected");
            } else {
                print_agent_table(&agents);
            }
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn agent_notifications(port: u16) -> Result<()> {
    let resp = request(port, &ClientMessage::AgentNotifications).await?;
    match resp {
        ServerMessage::AgentListResponse { agents } => {
            if agents.is_empty() {
                println!("no agents need intervention");
            } else {
                print_agent_table(&agents);
            }
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn agent_watch(port: u16, session_id_prefix: &str, show_thinking: bool) -> Result<()> {
    let session_id = resolve_agent_session(port, session_id_prefix).await?;

    let stream = connect(port).await?;
    let (mut reader, mut writer) = io::split(stream);

    send_client_message(&mut writer, &ClientMessage::AgentWatch { session_id }).await?;

    loop {
        match read_frame(&mut reader).await? {
            Some(Frame::Control(data)) => {
                let msg: ServerMessage = serde_json::from_slice(&data)?;
                match msg {
                    ServerMessage::AgentConversationLine { line, .. } => {
                        format_conversation_line(&line, show_thinking);
                    }
                    ServerMessage::AgentWatchEnd { .. } => {
                        eprintln!("[agent ended]");
                        break;
                    }
                    ServerMessage::Error { message } => {
                        bail!("{}", message);
                    }
                    _ => {}
                }
            }
            Some(Frame::Data(_)) => {}
            None => {
                eprintln!("[server disconnected]");
                break;
            }
        }
    }

    Ok(())
}

pub async fn agent_prompt(
    port: u16,
    session_id_prefix: &str,
    text: &str,
    show_thinking: bool,
) -> Result<()> {
    let session_id = resolve_agent_session(port, session_id_prefix).await?;

    // Send the prompt
    let resp = request(
        port,
        &ClientMessage::AgentPrompt {
            session_id,
            text: text.to_string(),
        },
    )
    .await?;

    if let ServerMessage::Error { message } = resp {
        bail!("{}", message);
    }

    // Switch to watch mode
    agent_watch(port, &session_id.to_string(), show_thinking).await
}

async fn resolve_agent_session(port: u16, prefix: &str) -> Result<Uuid> {
    // Try parsing as a full UUID first
    if let Ok(id) = prefix.parse::<Uuid>() {
        return Ok(id);
    }

    // Otherwise, list agents and match by prefix
    let resp = request(port, &ClientMessage::AgentList).await?;
    match resp {
        ServerMessage::AgentListResponse { agents } => {
            let matches: Vec<_> = agents
                .iter()
                .filter(|a| a.vex_session_id.to_string().starts_with(prefix))
                .collect();
            match matches.len() {
                0 => bail!("no agent matching prefix '{}'", prefix),
                1 => Ok(matches[0].vex_session_id),
                n => bail!("ambiguous prefix '{}' matches {} agents", prefix, n),
            }
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

fn format_conversation_line(line: &str, show_thinking: bool) {
    let parsed: Result<Value, _> = serde_json::from_str(line);
    let Ok(v) = parsed else {
        return;
    };

    let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match msg_type {
        "user" => {
            if let Some(content) = v.get("message").and_then(|m| m.get("content"))
                && let Some(text) = extract_text(content)
            {
                println!("> {}", text);
            }
        }
        "assistant" => {
            if let Some(content) = v.get("message").and_then(|m| m.get("content")) {
                format_assistant_content(content, show_thinking);
            }
        }
        _ => {}
    }

    let _ = std::io::stdout().flush();
}

fn format_assistant_content(content: &Value, show_thinking: bool) {
    match content {
        Value::String(s) => {
            println!("{}", s);
        }
        Value::Array(items) => {
            for item in items {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match item_type {
                    "text" => {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            println!("{}", text);
                        }
                    }
                    "tool_use" => {
                        let name = item
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let input = item.get("input").cloned().unwrap_or(Value::Null);
                        let summary = tool_summary(name, &input);
                        println!("{}", summary);
                    }
                    "thinking" => {
                        if show_thinking
                            && let Some(text) = item.get("thinking").and_then(|t| t.as_str())
                        {
                            println!("[thinking] {}", text);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn tool_summary(name: &str, input: &Value) -> String {
    match name {
        "Read" | "Edit" | "Write" => {
            let path = input
                .get("file_path")
                .and_then(|p| p.as_str())
                .unwrap_or("?");
            format!("[tool: {}] {}", name, path)
        }
        "Bash" => {
            let cmd = input.get("command").and_then(|c| c.as_str()).unwrap_or("?");
            let short = if cmd.len() > 60 { &cmd[..60] } else { cmd };
            format!("[tool: Bash] {}", short)
        }
        "Grep" | "Glob" => {
            let pat = input.get("pattern").and_then(|p| p.as_str()).unwrap_or("?");
            format!("[tool: {}] {}", name, pat)
        }
        _ => format!("[tool: {}]", name),
    }
}

fn extract_text(content: &Value) -> Option<String> {
    match content {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let texts: Vec<&str> = items
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        item.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        _ => None,
    }
}
