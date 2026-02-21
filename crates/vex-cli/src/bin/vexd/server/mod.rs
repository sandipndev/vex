pub mod tcp;
pub mod unix;

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use vex_cli::{
    Agent, AgentStatus, Command, Repository, Response, Transport, VexProtoError, Workstream,
    WorkstreamStatus,
};

use crate::repo_store::{gen_id, next_agent_id, unix_ts};
use crate::state::AppState;

// ── Connection handler ────────────────────────────────────────────────────────

pub async fn handle_connection<S>(
    mut stream: S,
    state: Arc<AppState>,
    transport: Transport,
    token_id: Option<String>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let cmd: Command = match vex_cli::framing::recv(&mut stream).await {
            Ok(c) => c,
            Err(_) => break,
        };
        let response = dispatch(cmd, &state, &transport, &token_id).await;
        vex_cli::framing::send(&mut stream, &response).await?;
    }
    Ok(())
}

// ── Dispatcher ────────────────────────────────────────────────────────────────

async fn dispatch(
    cmd: Command,
    state: &Arc<AppState>,
    transport: &Transport,
    token_id: &Option<String>,
) -> Response {
    match cmd {
        // ── Existing ──────────────────────────────────────────────────────────
        Command::Status => Response::DaemonStatus(vex_cli::DaemonStatus {
            uptime_secs: state.uptime_secs(),
            connected_clients: state.connected_clients(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),

        Command::Whoami => Response::ClientInfo(vex_cli::ClientInfo {
            token_id: token_id.clone(),
            is_local: matches!(transport, Transport::Unix),
        }),

        Command::PairCreate { label, expire_secs } => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            let mut store = state.token_store.lock().await;
            match store.generate(label, expire_secs) {
                Ok((_, secret)) => {
                    let token = store.list().last().cloned().unwrap();
                    Response::Pair(vex_cli::PairPayload {
                        token_id: token.token_id,
                        token_secret: secret,
                        host: None,
                    })
                }
                Err(e) => Response::Error(VexProtoError::Internal(e.to_string())),
            }
        }

        Command::PairList => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            let store = state.token_store.lock().await;
            let clients = store
                .list()
                .iter()
                .map(|t| vex_cli::PairedClient {
                    token_id: t.token_id.clone(),
                    label: t.label.clone(),
                    created_at: t.created_at.to_rfc3339(),
                    expires_at: t.expires_at.map(|d| d.to_rfc3339()),
                    last_seen: t.last_seen.map(|d| d.to_rfc3339()),
                })
                .collect();
            Response::PairedClients(clients)
        }

        Command::PairRevoke { id } => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            let mut store = state.token_store.lock().await;
            if store.revoke(&id) {
                let _ = store.save();
                Response::Ok
            } else {
                Response::Error(VexProtoError::NotFound)
            }
        }

        Command::PairRevokeAll => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            let mut store = state.token_store.lock().await;
            let n = store.revoke_all();
            let _ = store.save();
            Response::Revoked(n as u32)
        }

        // ── Repos ──────────────────────────────────────────────────────────────
        Command::RepoRegister { path } => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            handle_repo_register(state, path).await
        }

        Command::RepoList => {
            let store = state.repo_store.lock().await;
            Response::RepoList(store.repos.clone())
        }

        Command::RepoUnregister { repo_id } => {
            let mut store = state.repo_store.lock().await;
            let pos = store.repos.iter().position(|r| r.id == repo_id);
            match pos {
                None => Response::Error(VexProtoError::NotFound),
                Some(i) => {
                    store.repos.remove(i);
                    if let Err(e) = store.save() {
                        tracing::error!("persist error: {e}");
                    }
                    Response::RepoUnregistered
                }
            }
        }

        // ── Workstreams ────────────────────────────────────────────────────────
        Command::WorkstreamCreate {
            repo_id,
            name,
            branch,
        } => handle_workstream_create(state, repo_id, name, branch).await,

        Command::WorkstreamList { repo_id } => {
            let store = state.repo_store.lock().await;
            let repos: Vec<Repository> = match &repo_id {
                None => store.repos.clone(),
                Some(rid) => store
                    .repos
                    .iter()
                    .filter(|r| &r.id == rid)
                    .cloned()
                    .collect(),
            };
            Response::WorkstreamList(repos)
        }

        Command::WorkstreamDelete { workstream_id } => {
            handle_workstream_delete(state, workstream_id).await
        }

        // ── Agents ────────────────────────────────────────────────────────────
        Command::AgentSpawn {
            workstream_id,
            prompt,
        } => handle_agent_spawn(state, workstream_id, prompt).await,

        Command::AgentKill { agent_id } => handle_agent_kill(state, agent_id).await,

        Command::AgentList { workstream_id } => {
            let store = state.repo_store.lock().await;
            match store.get_workstream(&workstream_id) {
                None => Response::Error(VexProtoError::NotFound),
                Some(ws) => Response::AgentList(ws.agents.clone()),
            }
        }
    }
}

// ── Repo handlers ─────────────────────────────────────────────────────────────

async fn handle_repo_register(state: &Arc<AppState>, path: String) -> Response {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return err(format!("path does not exist: {path}"));
    }
    if !p.join(".git").exists() {
        return err(format!("not a git repository: {path}"));
    }

    {
        let store = state.repo_store.lock().await;
        if store.find_by_path(&path).is_some() {
            return err(format!("repository already registered: {path}"));
        }
    }

    let name = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let id = gen_id("repo");

    if let Err(e) = std::fs::create_dir_all(state.worktrees_dir()) {
        return err(format!("failed to create worktrees dir: {e}"));
    }

    let repo = Repository {
        id: id.clone(),
        name: name.clone(),
        path: path.clone(),
        registered_at: unix_ts(),
        workstreams: Vec::new(),
    };

    let mut store = state.repo_store.lock().await;
    store.repos.push(repo.clone());
    if let Err(e) = store.save() {
        store.repos.pop();
        return err(format!("persist failed: {e}"));
    }

    tracing::info!("Registered repo {id} ({name}) at {path}");
    Response::RepoRegistered(repo)
}

// ── Workstream handlers ───────────────────────────────────────────────────────

async fn handle_workstream_create(
    state: &Arc<AppState>,
    repo_id: String,
    name: String,
    branch: String,
) -> Response {
    // Extract repo path under lock, then release
    let repo_path = {
        let store = state.repo_store.lock().await;
        match store.find_by_id(&repo_id) {
            None => return Response::Error(VexProtoError::NotFound),
            Some(repo) => {
                if repo.workstreams.iter().any(|w| w.name == name) {
                    return err(format!("workstream '{name}' already exists in this repo"));
                }
                repo.path.clone()
            }
        }
    };

    // Validate branch
    match tokio::process::Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .arg("branch")
        .arg("--list")
        .arg(&branch)
        .output()
        .await
    {
        Err(e) => return err(format!("git error: {e}")),
        Ok(out) => {
            if String::from_utf8_lossy(&out.stdout).trim().is_empty() {
                return err(format!("branch '{branch}' not found in {repo_path}"));
            }
        }
    }

    let ws_id = gen_id("ws");
    let worktree_path = state.worktrees_dir().join(&ws_id);
    let tmux_session = format!("vex-{ws_id}");

    // Create git worktree
    let out = match tokio::process::Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .arg("worktree")
        .arg("add")
        .arg(&worktree_path)
        .arg(&branch)
        .output()
        .await
    {
        Err(e) => return err(format!("git worktree add failed: {e}")),
        Ok(o) => o,
    };
    if !out.status.success() {
        return err(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    // Run config hooks
    for hook in state.user_config.register_hooks() {
        let hook_out = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&hook.run)
            .current_dir(&worktree_path)
            .output()
            .await;

        let failed = match hook_out {
            Err(_) => true,
            Ok(o) => !o.status.success(),
        };

        if failed {
            tracing::warn!("Hook '{}' failed — rolling back worktree", hook.run);
            let _ = remove_worktree(&repo_path, &worktree_path).await;
            return err(format!("hook '{}' failed", hook.run));
        }
    }

    // Create tmux session
    let tmux_out = tokio::process::Command::new("tmux")
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(&tmux_session)
        .arg("-c")
        .arg(&worktree_path)
        .output()
        .await;

    if let Err(e) = &tmux_out {
        let _ = remove_worktree(&repo_path, &worktree_path).await;
        return err(format!("tmux new-session failed: {e}"));
    }
    if !tmux_out.unwrap().status.success() {
        let _ = remove_worktree(&repo_path, &worktree_path).await;
        return err("tmux new-session failed".to_string());
    }

    // Rename window 0 → "shell"
    let _ = tokio::process::Command::new("tmux")
        .arg("rename-window")
        .arg("-t")
        .arg(format!("{tmux_session}:0"))
        .arg("shell")
        .output()
        .await;

    let ws = Workstream {
        id: ws_id.clone(),
        name: name.clone(),
        repo_id: repo_id.clone(),
        branch: branch.clone(),
        worktree_path: worktree_path.to_string_lossy().to_string(),
        tmux_session: tmux_session.clone(),
        status: WorkstreamStatus::Idle,
        agents: Vec::new(),
        created_at: unix_ts(),
    };

    {
        let mut store = state.repo_store.lock().await;
        if let Some(repo) = store.find_by_id_mut(&repo_id) {
            repo.workstreams.push(ws.clone());
        }
        if let Err(e) = store.save() {
            tracing::error!("Failed to persist workstream: {e}");
            let _ = tokio::process::Command::new("tmux")
                .arg("kill-session")
                .arg("-t")
                .arg(&tmux_session)
                .output()
                .await;
            let _ = remove_worktree(&repo_path, &worktree_path).await;
            return err(format!("persist failed: {e}"));
        }
    }

    tracing::info!("Created workstream {ws_id} ({name}) on branch {branch}");
    Response::WorkstreamCreated(ws)
}

async fn handle_workstream_delete(state: &Arc<AppState>, workstream_id: String) -> Response {
    // Extract what we need under lock
    let (repo_path, ws_clone) = {
        let store = state.repo_store.lock().await;
        let Some((ri, wi)) = store.ws_indices(&workstream_id) else {
            return Response::Error(VexProtoError::NotFound);
        };
        let repo = &store.repos[ri];
        let ws = &repo.workstreams[wi];
        (repo.path.clone(), ws.clone())
    };

    // Kill running agent windows (best-effort) and cancel monitors
    for agent in &ws_clone.agents {
        if agent.status == AgentStatus::Running {
            let _ = tokio::process::Command::new("tmux")
                .arg("kill-window")
                .arg("-t")
                .arg(format!("{}:{}", ws_clone.tmux_session, agent.tmux_window))
                .output()
                .await;
            let mut handles = state.monitor_handles.lock().await;
            if let Some(h) = handles.remove(&agent.id) {
                h.abort();
            }
        }
    }

    // Kill tmux session
    let _ = tokio::process::Command::new("tmux")
        .arg("kill-session")
        .arg("-t")
        .arg(&ws_clone.tmux_session)
        .output()
        .await;

    // Remove worktree
    let _ = remove_worktree(&repo_path, std::path::Path::new(&ws_clone.worktree_path)).await;

    // Update state
    let mut store = state.repo_store.lock().await;
    if let Some((ri, wi)) = store.ws_indices(&workstream_id) {
        store.repos[ri].workstreams.remove(wi);
    }
    if let Err(e) = store.save() {
        tracing::error!("persist error after delete: {e}");
    }

    tracing::info!("Deleted workstream {workstream_id}");
    Response::WorkstreamDeleted
}

// ── Agent handlers ────────────────────────────────────────────────────────────

async fn handle_agent_spawn(
    state: &Arc<AppState>,
    workstream_id: String,
    prompt: String,
) -> Response {
    // Validate workstream, get needed info
    let (tmux_session, worktree_path, next_id) = {
        let store = state.repo_store.lock().await;
        let Some((ri, wi)) = store.ws_indices(&workstream_id) else {
            return Response::Error(VexProtoError::NotFound);
        };
        let ws = &store.repos[ri].workstreams[wi];
        let next_id = next_agent_id(&ws.agents);
        (ws.tmux_session.clone(), ws.worktree_path.clone(), next_id)
    };

    // Verify tmux session is alive
    let alive = tokio::process::Command::new("tmux")
        .arg("has-session")
        .arg("-t")
        .arg(&tmux_session)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !alive {
        return err(format!("tmux session {tmux_session} is not running"));
    }

    // Create new tmux window and capture its index
    let window_out = tokio::process::Command::new("tmux")
        .arg("new-window")
        .arg("-t")
        .arg(&tmux_session)
        .arg("-c")
        .arg(&worktree_path)
        .arg("-n")
        .arg(&next_id)
        .arg("-P")
        .arg("-F")
        .arg("#{window_index}")
        .output()
        .await;

    let window_index: u32 = match window_out {
        Err(e) => return err(format!("tmux new-window failed: {e}")),
        Ok(o) if !o.status.success() => {
            return err(format!(
                "tmux new-window failed: {}",
                String::from_utf8_lossy(&o.stderr)
            ));
        }
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout);
            match s.trim().parse() {
                Ok(n) => n,
                Err(_) => return err("could not parse tmux window index".to_string()),
            }
        }
    };

    // Build agent command
    let agent_cmd = state.user_config.agent_command();
    let parts: Vec<&str> = agent_cmd.split_whitespace().collect();
    let cmd_str = build_send_keys_cmd(&parts, &prompt);

    // Send command to window
    let _ = tokio::process::Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(format!("{tmux_session}:{window_index}"))
        .arg(&cmd_str)
        .arg("Enter")
        .output()
        .await;

    let agent = Agent {
        id: next_id.clone(),
        workstream_id: workstream_id.clone(),
        tmux_window: window_index,
        prompt: prompt.clone(),
        status: AgentStatus::Running,
        exit_code: None,
        spawned_at: unix_ts(),
        exited_at: None,
    };

    // Persist
    {
        let mut store = state.repo_store.lock().await;
        if let Some((ri, wi)) = store.ws_indices(&workstream_id) {
            store.repos[ri].workstreams[wi].agents.push(agent.clone());
            store.repos[ri].workstreams[wi].status = WorkstreamStatus::Running;
        }
        if let Err(e) = store.save() {
            tracing::error!("persist error after spawn: {e}");
        }
    }

    // Spawn monitoring task
    let handle = tokio::spawn(monitor_agent(
        state.clone(),
        workstream_id,
        next_id.clone(),
        window_index,
    ));
    state
        .monitor_handles
        .lock()
        .await
        .insert(next_id.clone(), handle.abort_handle());

    tracing::info!("Spawned agent {next_id} in window {window_index}");
    Response::AgentSpawned(agent)
}

async fn handle_agent_kill(state: &Arc<AppState>, agent_id: String) -> Response {
    let (ws_id, tmux_session, window_index) = {
        let store = state.repo_store.lock().await;
        let Some((ri, wi, ai)) = store.agent_indices(&agent_id) else {
            return Response::Error(VexProtoError::NotFound);
        };
        let ws = &store.repos[ri].workstreams[wi];
        let agent = &ws.agents[ai];
        (ws.id.clone(), ws.tmux_session.clone(), agent.tmux_window)
    };

    // Cancel monitor task
    if let Some(handle) = state.monitor_handles.lock().await.remove(&agent_id) {
        handle.abort();
    }

    // Kill tmux window
    let _ = tokio::process::Command::new("tmux")
        .arg("kill-window")
        .arg("-t")
        .arg(format!("{tmux_session}:{window_index}"))
        .output()
        .await;

    // Update state
    {
        let mut store = state.repo_store.lock().await;
        if let Some((ri, wi, ai)) = store.agent_indices(&agent_id) {
            let agent = &mut store.repos[ri].workstreams[wi].agents[ai];
            agent.status = AgentStatus::Exited;
            agent.exited_at = Some(unix_ts());
        }
        store.refresh_ws_status(&ws_id);
        if let Err(e) = store.save() {
            tracing::error!("persist error after kill: {e}");
        }
    }

    tracing::info!("Killed agent {agent_id}");
    Response::AgentKilled
}

// ── Agent monitor ─────────────────────────────────────────────────────────────

/// Polls `tmux list-windows` every 5 seconds. When the agent's window
/// disappears, marks the agent Exited and persists state.
pub async fn monitor_agent(
    state: Arc<AppState>,
    workstream_id: String,
    agent_id: String,
    tmux_window: u32,
) {
    let tmux_session = {
        let store = state.repo_store.lock().await;
        store
            .get_workstream(&workstream_id)
            .map(|ws| ws.tmux_session.clone())
            .unwrap_or_default()
    };

    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;

        let window_alive = check_window_alive(&tmux_session, tmux_window).await;
        if !window_alive {
            tracing::info!("Agent {agent_id} window closed — marking Exited");
            let mut store = state.repo_store.lock().await;
            if let Some((ri, wi, ai)) = store.agent_indices(&agent_id) {
                let agent = &mut store.repos[ri].workstreams[wi].agents[ai];
                if agent.status == AgentStatus::Running {
                    agent.status = AgentStatus::Exited;
                    agent.exited_at = Some(unix_ts());
                }
            }
            store.refresh_ws_status(&workstream_id);
            if let Err(e) = store.save() {
                tracing::error!("persist error in monitor: {e}");
            }
            break;
        }
    }
}

async fn check_window_alive(session: &str, window_index: u32) -> bool {
    let Ok(out) = tokio::process::Command::new("tmux")
        .arg("list-windows")
        .arg("-t")
        .arg(session)
        .arg("-F")
        .arg("#{window_index}")
        .output()
        .await
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim() == window_index.to_string())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn err(msg: String) -> Response {
    Response::Error(VexProtoError::Internal(msg))
}

/// Build `<cmd_parts> '<prompt>'` with single-quote escaping for the prompt.
fn build_send_keys_cmd(cmd_parts: &[&str], prompt: &str) -> String {
    let escaped = prompt.replace('\'', "'\\''");
    format!("{} '{escaped}'", cmd_parts.join(" "))
}

async fn remove_worktree(repo_path: &str, worktree_path: &std::path::Path) -> anyhow::Result<()> {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(worktree_path)
        .output()
        .await?;
    Ok(())
}
