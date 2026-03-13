use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, bail};
use chrono::Utc;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};
use uuid::Uuid;
use vex_cli::proto::{AgentEvent, AgentInfo, AgentState, ServerMessage};

pub struct AgentConfig {
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Vec<String>,
    pub max_turns: Option<u32>,
    pub cwd: Option<String>,
}

struct AgentHandle {
    pub id: Uuid,
    pub config: AgentConfig,
    pub claude_session_id: Option<String>,
    pub status: AgentState,
    pub turn_count: u32,
    pub created_at: chrono::DateTime<Utc>,
    pub child: Option<tokio::process::Child>,
}

pub struct AgentManager {
    agents: Arc<Mutex<HashMap<Uuid, AgentHandle>>>,
}

fn build_claude_args(
    config: &AgentConfig,
    prompt: &str,
    claude_session_id: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
    ];

    if let Some(model) = &config.model {
        args.push("--model".to_string());
        args.push(model.clone());
    }

    if let Some(mode) = &config.permission_mode {
        args.push("--permission-mode".to_string());
        args.push(mode.clone());
    }

    for tool in &config.allowed_tools {
        args.push("--allowedTools".to_string());
        args.push(tool.clone());
    }

    if let Some(turns) = config.max_turns {
        args.push("--max-turns".to_string());
        args.push(turns.to_string());
    }

    if let Some(session_id) = claude_session_id {
        args.push("--resume".to_string());
        args.push(session_id.to_string());
    }

    args.push(prompt.to_string());

    args
}

impl AgentManager {
    pub fn new() -> Self {
        Self {
            agents: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn create_agent(&self, config: AgentConfig) -> Uuid {
        let id = Uuid::new_v4();
        let handle = AgentHandle {
            id,
            config,
            claude_session_id: None,
            status: AgentState::Idle,
            turn_count: 0,
            created_at: Utc::now(),
            child: None,
        };
        let mut agents = self.agents.lock().await;
        agents.insert(id, handle);
        info!("created agent {}", id);
        id
    }

    pub async fn send_prompt(
        &self,
        id: Uuid,
        prompt: String,
    ) -> Result<mpsc::Receiver<ServerMessage>> {
        let (args, cwd) = {
            let mut agents = self.agents.lock().await;
            let handle = agents
                .get_mut(&id)
                .ok_or_else(|| anyhow::anyhow!("agent not found: {}", id))?;

            if handle.status == AgentState::Processing {
                bail!("agent {} is already processing a prompt", id);
            }

            handle.status = AgentState::Processing;

            let args =
                build_claude_args(&handle.config, &prompt, handle.claude_session_id.as_deref());
            let cwd = handle.config.cwd.clone();
            (args, cwd)
        };

        let mut cmd = Command::new("claude");
        cmd.args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());

        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!(
                "failed to spawn claude: {} (is claude installed and in PATH?)",
                e
            )
        })?;

        let stdout = child.stdout.take().expect("stdout was piped");

        // Store the child process
        {
            let mut agents = self.agents.lock().await;
            if let Some(handle) = agents.get_mut(&id) {
                handle.child = Some(child);
            }
        }

        let (tx, rx) = mpsc::channel::<ServerMessage>(256);
        let agents = Arc::clone(&self.agents);

        tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut captured_session_id: Option<String> = None;

            while let Ok(Some(line)) = lines.next_line().await {
                // Try to extract type and session_id from the JSON line
                let event_type = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
                    .unwrap_or_default();

                // Capture session_id from any line that has it
                if captured_session_id.is_none()
                    && let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
                    && let Some(sid) = v.get("session_id").and_then(|s| s.as_str())
                {
                    captured_session_id = Some(sid.to_string());
                }

                let event = AgentEvent {
                    event_type,
                    raw_json: line,
                };

                if tx
                    .send(ServerMessage::AgentOutput { id, event })
                    .await
                    .is_err()
                {
                    // Client disconnected, but let the process finish
                    break;
                }
            }

            // Wait for the child process to exit
            {
                let mut agents = agents.lock().await;
                if let Some(handle) = agents.get_mut(&id) {
                    if let Some(ref mut child) = handle.child {
                        let status = child.wait().await;
                        match status {
                            Ok(s) if s.success() => {
                                handle.status = AgentState::Idle;
                            }
                            Ok(s) => {
                                handle.status =
                                    AgentState::Error(format!("claude exited with status {}", s));
                            }
                            Err(e) => {
                                handle.status = AgentState::Error(format!("wait error: {}", e));
                            }
                        }
                    } else {
                        handle.status = AgentState::Idle;
                    }
                    handle.child = None;
                    handle.turn_count += 1;
                    if let Some(sid) = captured_session_id {
                        handle.claude_session_id = Some(sid);
                    }
                    let turn_count = handle.turn_count;

                    // Send done message (ignore if client gone)
                    let _ = tx
                        .send(ServerMessage::AgentPromptDone { id, turn_count })
                        .await;
                }
            }

            info!("agent {} prompt finished", id);
        });

        Ok(rx)
    }

    pub async fn get_status_full(&self, id: Uuid) -> Result<(AgentInfo, Option<String>)> {
        let agents = self.agents.lock().await;
        let handle = agents
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("agent not found: {}", id))?;
        Ok((
            AgentInfo {
                id: handle.id,
                status: handle.status.clone(),
                model: handle.config.model.clone(),
                turn_count: handle.turn_count,
                created_at: handle.created_at,
            },
            handle.claude_session_id.clone(),
        ))
    }

    pub async fn list_agents(&self) -> Vec<AgentInfo> {
        let agents = self.agents.lock().await;
        agents
            .values()
            .map(|h| AgentInfo {
                id: h.id,
                status: h.status.clone(),
                model: h.config.model.clone(),
                turn_count: h.turn_count,
                created_at: h.created_at,
            })
            .collect()
    }

    pub async fn kill_agent(&self, id: Uuid) -> Result<()> {
        let mut agents = self.agents.lock().await;
        let handle = agents
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("agent not found: {}", id))?;

        // Kill running child process if any
        if let Some(ref mut child) = handle.child {
            let _ = child.kill().await;
            warn!("killed running claude process for agent {}", id);
        }

        agents.remove(&id);
        info!("removed agent {}", id);
        Ok(())
    }
}
