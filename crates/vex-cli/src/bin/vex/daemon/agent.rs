use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::debug;
use uuid::Uuid;
use vex_cli::proto::AgentEntry;

use super::session::SessionManager;

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub vex_session_id: Uuid,
    pub claude_session_id: String,
    pub claude_pid: u32,
    pub cwd: PathBuf,
    pub jsonl_path: PathBuf,
    pub detected_at: DateTime<Utc>,
    pub needs_intervention: bool,
}

impl AgentInfo {
    pub fn to_entry(&self) -> AgentEntry {
        AgentEntry {
            vex_session_id: self.vex_session_id,
            claude_session_id: self.claude_session_id.clone(),
            claude_pid: self.claude_pid,
            cwd: self.cwd.clone(),
            detected_at: self.detected_at,
            needs_intervention: self.needs_intervention,
        }
    }
}

pub type AgentStore = Arc<Mutex<HashMap<Uuid, AgentInfo>>>;

pub fn new_agent_store() -> AgentStore {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Deserialize)]
struct ClaudeSessionFile {
    pid: u32,
    #[serde(rename = "sessionId")]
    session_id: String,
    cwd: PathBuf,
}

/// Spawn a background task that periodically scans for Claude Code processes
/// that are children of vex session shells.
pub fn spawn_detection_task(manager: Arc<SessionManager>, store: AgentStore) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            interval.tick().await;
            if let Err(e) = detect_agents(&manager, &store).await {
                debug!("agent detection error: {}", e);
            }
        }
    });
}

async fn detect_agents(manager: &SessionManager, store: &AgentStore) -> anyhow::Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let sessions_dir = home.join(".claude").join("sessions");
    let shell_pids = manager.shell_pids().await;

    if !sessions_dir.exists() || shell_pids.is_empty() {
        store.lock().await.clear();
        return Ok(());
    }

    // Scan Claude session files
    let mut found: HashMap<Uuid, AgentInfo> = HashMap::new();

    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => {
            store.lock().await.clear();
            return Ok(());
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let claude_session: ClaudeSessionFile = match serde_json::from_str(&data) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Check if the Claude process is still alive
        let proc_stat = PathBuf::from("/proc").join(claude_session.pid.to_string());
        if !proc_stat.exists() {
            continue;
        }

        // Walk the parent chain to see if any ancestor matches a vex shell PID
        if let Some(vex_session_id) = find_ancestor_match(claude_session.pid, &shell_pids) {
            let jsonl_path =
                derive_jsonl_path(&home, &claude_session.cwd, &claude_session.session_id);
            let needs_intervention = check_needs_intervention(&jsonl_path);

            found.insert(
                vex_session_id,
                AgentInfo {
                    vex_session_id,
                    claude_session_id: claude_session.session_id,
                    claude_pid: claude_session.pid,
                    cwd: claude_session.cwd,
                    jsonl_path,
                    detected_at: Utc::now(),
                    needs_intervention,
                },
            );
        }
    }

    // Update store — preserve detected_at for existing entries
    let mut agents = store.lock().await;
    for (id, mut info) in found {
        if let Some(existing) = agents.get(&id)
            && existing.claude_pid == info.claude_pid
            && existing.claude_session_id == info.claude_session_id
        {
            info.detected_at = existing.detected_at;
        }
        agents.insert(id, info);
    }

    // Remove entries whose vex session or claude process no longer exists
    agents.retain(|vex_id, info| {
        shell_pids.contains_key(vex_id) && Path::new(&format!("/proc/{}", info.claude_pid)).exists()
    });

    Ok(())
}

/// Walk /proc/{pid}/stat parent chain upward to find a matching vex shell PID.
fn find_ancestor_match(pid: u32, shell_pids: &HashMap<Uuid, u32>) -> Option<Uuid> {
    let mut current = pid;
    // Limit walk depth to avoid infinite loops
    for _ in 0..64 {
        let ppid = read_ppid(current)?;

        // Check if ppid matches any vex shell
        for (session_id, &shell_pid) in shell_pids {
            if ppid == shell_pid {
                return Some(*session_id);
            }
        }

        if ppid <= 1 {
            return None;
        }
        current = ppid;
    }
    None
}

/// Read the parent PID from /proc/{pid}/stat.
fn read_ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    // Format: pid (comm) state ppid ...
    // comm can contain spaces and parens, so find the last ')' first
    let after_comm = stat.rfind(')')? + 2; // skip ') '
    let rest = stat.get(after_comm..)?;
    let mut fields = rest.split_whitespace();
    let _state = fields.next()?;
    let ppid_str = fields.next()?;
    ppid_str.parse().ok()
}

/// Check if the agent needs human intervention by reading the last line of the JSONL.
/// Returns true if the last meaningful entry is an "assistant" message (Claude finished
/// its turn and is waiting for user input).
fn check_needs_intervention(jsonl_path: &Path) -> bool {
    use std::io::{BufRead, BufReader};

    let file = match std::fs::File::open(jsonl_path) {
        Ok(f) => f,
        Err(_) => return false,
    };

    let reader = BufReader::new(file);
    let mut last_type = String::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Quick extraction of "type" without full parse
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed)
            && let Some(t) = v.get("type").and_then(|t| t.as_str())
        {
            last_type = t.to_string();
        }
    }

    last_type == "assistant"
}

/// Derive the JSONL conversation file path from cwd and session ID.
fn derive_jsonl_path(home: &Path, cwd: &Path, session_id: &str) -> PathBuf {
    let encoded_cwd = cwd.to_string_lossy().replace('/', "-");
    home.join(".claude")
        .join("projects")
        .join(&encoded_cwd)
        .join(format!("{}.jsonl", session_id))
}
