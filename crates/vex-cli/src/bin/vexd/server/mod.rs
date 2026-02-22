pub mod tcp;
pub mod unix;

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use vex_cli::{
    Agent, AgentStatus, Command, Repository, Response, ShellMsg, ShellSession, ShellStatus,
    Transport, VexProtoError, Workstream, WorkstreamStatus,
};

use crate::state::ShellRuntime;

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
    // Drop idle connections after 5 minutes with no command, preventing
    // authenticated clients from holding file descriptors open indefinitely.
    const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

    loop {
        let cmd: Command =
            match tokio::time::timeout(IDLE_TIMEOUT, vex_cli::framing::recv(&mut stream)).await {
                Ok(Ok(c)) => c,
                Ok(Err(_)) => break, // client disconnected / framing error
                Err(_) => break,     // idle timeout
            };
        // Streaming commands consume the connection — handle before dispatch
        match cmd {
            Command::ShellRegister {
                workstream_id,
                tmux_window,
            } => {
                // Shell supervisor registration is only allowed over the local
                // Unix socket — TCP clients must not be able to register a PTY
                // supervisor (they cannot run vexd-internal processes anyway).
                if matches!(transport, Transport::Tcp) {
                    vex_cli::framing::send(&mut stream, &Response::Error(VexProtoError::LocalOnly))
                        .await?;
                    return Ok(());
                }
                return handle_shell_register_streaming(stream, &state, workstream_id, tmux_window)
                    .await;
            }
            Command::AttachShell { shell_id } => {
                // Shell attach streams PTY output — restrict to local Unix socket
                // so remote TCP clients cannot snoop on interactive shell sessions.
                if matches!(transport, Transport::Tcp) {
                    vex_cli::framing::send(&mut stream, &Response::Error(VexProtoError::LocalOnly))
                        .await?;
                    return Ok(());
                }
                return handle_shell_attach_streaming(stream, &state, shell_id).await;
            }
            cmd => {
                let response = dispatch(cmd, &state, &transport, &token_id).await;
                vex_cli::framing::send(&mut stream, &response).await?;
            }
        }
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

        Command::RepoSetDefaultBranch { repo_id, branch } => {
            if matches!(transport, Transport::Tcp) {
                return Response::Error(VexProtoError::LocalOnly);
            }
            let mut store = state.repo_store.lock().await;
            match store.repos.iter_mut().find(|r| r.id == repo_id) {
                None => Response::Error(VexProtoError::NotFound),
                Some(repo) => {
                    repo.default_branch = branch;
                    if let Err(e) = store.save() {
                        tracing::error!("persist error: {e}");
                    }
                    Response::RepoDefaultBranchSet
                }
            }
        }

        // ── Workstreams ────────────────────────────────────────────────────────
        Command::WorkstreamCreate {
            repo_id,
            name,
            branch,
            from_ref,
            fetch_latest,
        } => handle_workstream_create(state, repo_id, name, branch, from_ref, fetch_latest).await,

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

        Command::AgentSpawnInPlace {
            workstream_id,
            tmux_window,
            prompt,
        } => handle_agent_spawn_in_place(state, workstream_id, tmux_window, prompt).await,

        Command::AgentKill { agent_id } => handle_agent_kill(state, agent_id).await,

        Command::AgentList { workstream_id } => {
            let store = state.repo_store.lock().await;
            match store.get_workstream(&workstream_id) {
                None => Response::Error(VexProtoError::NotFound),
                Some(ws) => Response::AgentList(ws.agents.clone()),
            }
        }

        // ── Shells (implementations added in a later step) ────────────────────
        Command::ShellSpawn { workstream_id } => handle_shell_spawn(state, workstream_id).await,

        Command::ShellKill { shell_id } => handle_shell_kill(state, shell_id).await,

        Command::ShellList { workstream_id } => {
            let store = state.repo_store.lock().await;
            match store.get_workstream(&workstream_id) {
                None => Response::Error(VexProtoError::NotFound),
                Some(ws) => Response::ShellList(ws.shells.clone()),
            }
        }

        // ShellRegister and AttachShell are intercepted in handle_connection
        // before dispatch is called; these arms are unreachable in practice.
        Command::ShellRegister { .. } | Command::AttachShell { .. } => Response::Error(
            VexProtoError::Internal("unexpected streaming command".into()),
        ),

        Command::DetachShell { shell_id: _ }
        | Command::PtyInput { .. }
        | Command::PtyResize { .. } => Response::Error(VexProtoError::Internal(
            "PTY streaming not yet implemented".into(),
        )),
    }
}

// ── Repo handlers ─────────────────────────────────────────────────────────────

/// Detect the default branch of a git repository.
/// Order: symbolic-ref refs/remotes/origin/HEAD → rev-parse --abbrev-ref HEAD → "main"
async fn detect_default_branch(repo_path: &str) -> String {
    if let Ok(out) = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("symbolic-ref")
        .arg("refs/remotes/origin/HEAD")
        .arg("--short")
        .output()
        .await
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(branch) = s.trim().strip_prefix("origin/") {
            return branch.to_string();
        }
    }

    if let Ok(out) = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output()
        .await
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() && s != "HEAD" {
            return s;
        }
    }

    "main".to_string()
}

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

    // Detect default branch while no lock is held
    let default_branch = detect_default_branch(&path).await;

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
        default_branch,
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
    name: Option<String>,
    branch: Option<String>,
    from_ref: Option<String>,
    fetch_latest: bool,
) -> Response {
    // Resolve branch/name and extract repo path under lock, then release
    let (repo_path, branch, name) = {
        let store = state.repo_store.lock().await;
        match store.find_by_id(&repo_id) {
            None => return Response::Error(VexProtoError::NotFound),
            Some(repo) => {
                let resolved_branch = branch.unwrap_or_else(|| repo.default_branch.clone());
                let resolved_name = name.unwrap_or_else(|| resolved_branch.clone());
                if repo.workstreams.iter().any(|w| w.name == resolved_name) {
                    return err(format!(
                        "workstream '{resolved_name}' already exists in this repo"
                    ));
                }
                (repo.path.clone(), resolved_branch, resolved_name)
            }
        }
    };

    let ws_id = gen_id("ws");
    let worktree_path = state.worktrees_dir().join(&ws_id);
    let tmux_session = format!("vex-{ws_id}");

    // Create git worktree — two modes:
    // 1. from_ref is Some: create new local branch <branch> from the given ref
    // 2. DWIM: resolve local → remote → auto-fetch → error with branch list
    let worktree_out = if let Some(ref r) = from_ref {
        worktree_add_new_branch(&repo_path, &worktree_path, &branch, r).await
    } else {
        if fetch_latest && let Err(e) = git_fetch_origin(&repo_path).await {
            return err(e);
        }

        if local_branch_exists(&repo_path, &branch).await {
            worktree_add_branch(&repo_path, &worktree_path, &branch).await
        } else if remote_branch_exists(&repo_path, &branch).await {
            worktree_add_tracking(&repo_path, &worktree_path, &branch).await
        } else {
            // Auto-fetch and retry
            let _ = git_fetch_origin(&repo_path).await;
            if remote_branch_exists(&repo_path, &branch).await {
                worktree_add_tracking(&repo_path, &worktree_path, &branch).await
            } else {
                let list = list_all_branches(&repo_path).await;
                return err(format!(
                    "branch '{branch}' not found locally or on origin.\nAvailable branches:\n{list}"
                ));
            }
        }
    };

    let out = match worktree_out {
        Err(e) => return err(format!("git worktree add failed: {e}")),
        Ok(o) => o,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut msg = format!("git worktree add failed: {}", stderr.trim());
        if stderr.contains("already checked out") {
            msg.push_str(
                "\nHint: use --from <branch> --branch <new-name> to create a new branch from it.",
            );
        }
        return err(msg);
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

    // Rename window 0 → "shell" and launch the PTY supervisor
    let _ = tokio::process::Command::new("tmux")
        .arg("rename-window")
        .arg("-t")
        .arg(format!("{tmux_session}:0"))
        .arg("shell")
        .output()
        .await;

    // Spawn `vex shell-supervisor` in window 0 so it registers with vexd as a PTY supervisor.
    let _ = tokio::process::Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(format!("{tmux_session}:0"))
        .arg(format!(
            "vex shell-supervisor --workstream {ws_id} --window 0"
        ))
        .arg("Enter")
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
        shells: Vec::new(),
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

async fn handle_agent_spawn_in_place(
    state: &Arc<AppState>,
    workstream_id: String,
    tmux_window: u32,
    prompt: Option<String>,
) -> Response {
    let next_id = {
        let store = state.repo_store.lock().await;
        let Some((ri, wi)) = store.ws_indices(&workstream_id) else {
            return Response::Error(VexProtoError::NotFound);
        };
        next_agent_id(&store.repos[ri].workstreams[wi].agents)
    };

    // Build the shell command the client will exec
    let agent_cmd = state.user_config.agent_command();
    let parts: Vec<&str> = agent_cmd.split_whitespace().collect();
    let exec_cmd = build_send_keys_cmd(&parts, prompt.as_deref().unwrap_or(""));

    let agent = Agent {
        id: next_id.clone(),
        workstream_id: workstream_id.clone(),
        tmux_window,
        prompt: prompt.clone().unwrap_or_default(),
        status: AgentStatus::Running,
        exit_code: None,
        spawned_at: unix_ts(),
        exited_at: None,
    };

    {
        let mut store = state.repo_store.lock().await;
        if let Some((ri, wi)) = store.ws_indices(&workstream_id) {
            store.repos[ri].workstreams[wi].agents.push(agent.clone());
            store.repos[ri].workstreams[wi].status = WorkstreamStatus::Running;
        }
        if let Err(e) = store.save() {
            tracing::error!("persist error after in-place spawn: {e}");
        }
    }

    let handle = tokio::spawn(monitor_agent(
        state.clone(),
        workstream_id,
        next_id.clone(),
        tmux_window,
    ));
    state
        .monitor_handles
        .lock()
        .await
        .insert(next_id.clone(), handle.abort_handle());

    tracing::info!("Registered in-place agent {next_id} at window {tmux_window}");
    Response::AgentSpawnedInPlace { agent, exec_cmd }
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

// ── Shell handlers ────────────────────────────────────────────────────────────

async fn handle_shell_spawn(state: &Arc<AppState>, workstream_id: String) -> Response {
    use crate::repo_store::gen_id;

    let (tmux_session, worktree_path) = {
        let store = state.repo_store.lock().await;
        let Some((ri, wi)) = store.ws_indices(&workstream_id) else {
            return Response::Error(VexProtoError::NotFound);
        };
        let ws = &store.repos[ri].workstreams[wi];
        (ws.tmux_session.clone(), ws.worktree_path.clone())
    };

    // Create a new tmux window for the shell
    let shell_id = gen_id("sh");
    let window_out = tokio::process::Command::new("tmux")
        .arg("new-window")
        .arg("-t")
        .arg(&tmux_session)
        .arg("-c")
        .arg(&worktree_path)
        .arg("-n")
        .arg(&shell_id)
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
        Ok(o) => match String::from_utf8_lossy(&o.stdout).trim().parse() {
            Ok(n) => n,
            Err(_) => return err("could not parse tmux window index".to_string()),
        },
    };

    // Launch `vex shell-supervisor` PTY supervisor in the new window
    let _ = tokio::process::Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(format!("{tmux_session}:{window_index}"))
        .arg(format!(
            "vex shell-supervisor --workstream {workstream_id} --window {window_index}"
        ))
        .arg("Enter")
        .output()
        .await;

    let session = ShellSession {
        id: shell_id.clone(),
        workstream_id: workstream_id.clone(),
        tmux_window: window_index,
        status: ShellStatus::Detached,
        started_at: unix_ts(),
        exited_at: None,
        exit_code: None,
    };

    {
        let mut store = state.repo_store.lock().await;
        if let Some((ri, wi)) = store.ws_indices(&workstream_id) {
            store.repos[ri].workstreams[wi].shells.push(session.clone());
        }
        if let Err(e) = store.save() {
            tracing::error!("persist error after shell spawn: {e}");
        }
    }

    tracing::info!("Spawned shell {shell_id} in window {window_index}");
    Response::ShellSpawned(session)
}

async fn handle_shell_kill(state: &Arc<AppState>, shell_id: String) -> Response {
    let (ws_id, tmux_session, window_index) = {
        let store = state.repo_store.lock().await;
        let Some((ri, wi, si)) = store.shell_indices(&shell_id) else {
            return Response::Error(VexProtoError::NotFound);
        };
        let ws = &store.repos[ri].workstreams[wi];
        let sh = &ws.shells[si];
        (ws.id.clone(), ws.tmux_session.clone(), sh.tmux_window)
    };

    let _ = tokio::process::Command::new("tmux")
        .arg("kill-window")
        .arg("-t")
        .arg(format!("{tmux_session}:{window_index}"))
        .output()
        .await;

    {
        let mut store = state.repo_store.lock().await;
        if let Some((ri, wi, si)) = store.shell_indices(&shell_id) {
            let sh = &mut store.repos[ri].workstreams[wi].shells[si];
            sh.status = ShellStatus::Exited;
            sh.exited_at = Some(unix_ts());
        }
        let _ = ws_id; // used above for future refresh logic
        if let Err(e) = store.save() {
            tracing::error!("persist error after shell kill: {e}");
        }
    }

    tracing::info!("Killed shell {shell_id}");
    Response::ShellKilled
}

// ── PTY streaming ─────────────────────────────────────────────────────────────

const RING_MAX: usize = 10_000;

fn ring_append(buf: &mut Vec<u8>, data: &[u8]) {
    buf.extend_from_slice(data);
    if buf.len() > RING_MAX {
        let excess = buf.len() - RING_MAX;
        buf.drain(..excess);
    }
}

/// Upgrade a connection to PTY supervisor mode after `ShellRegister`.
///
/// Creates a `ShellRuntime`, persists the `ShellSession`, then enters a
/// bidirectional `ShellMsg` streaming loop:
/// - `ShellMsg::Out` from supervisor → ring buffer + broadcast to clients
/// - `ShellMsg::Exited` from supervisor → broadcast + cleanup
/// - `ShellMsg::In` / `Resize` from clients (via `input_rx`) → supervisor
async fn handle_shell_register_streaming<S>(
    stream: S,
    state: &Arc<AppState>,
    workstream_id: String,
    tmux_window: u32,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;

    // Validate workstream
    {
        let store = state.repo_store.lock().await;
        if store.ws_indices(&workstream_id).is_none() {
            drop(store);
            let mut s = stream;
            let _ = vex_cli::framing::send(&mut s, &Response::Error(VexProtoError::NotFound)).await;
            return Ok(());
        }
    }

    let shell_id = gen_id("sh");

    // Build ShellRuntime
    let (broadcast_tx, _initial_rx) = tokio::sync::broadcast::channel::<ShellMsg>(256);
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<ShellMsg>(256);
    drop(_initial_rx); // clients subscribe() to get their own receivers

    let runtime = Arc::new(ShellRuntime {
        output_buf: tokio::sync::Mutex::new(Vec::new()),
        broadcast_tx,
        input_tx,
    });

    // Persist ShellSession
    let session = ShellSession {
        id: shell_id.clone(),
        workstream_id: workstream_id.clone(),
        tmux_window,
        status: ShellStatus::Active,
        started_at: unix_ts(),
        exited_at: None,
        exit_code: None,
    };
    {
        let mut store = state.repo_store.lock().await;
        if let Some((ri, wi)) = store.ws_indices(&workstream_id) {
            store.repos[ri].workstreams[wi].shells.push(session);
        }
        let _ = store.save();
    }

    state
        .shell_runtimes
        .lock()
        .await
        .insert(shell_id.clone(), Arc::clone(&runtime));

    // Acknowledge registration
    let (mut net_read, mut net_write) = tokio::io::split(stream);
    vex_cli::framing::send(
        &mut net_write,
        &Response::ShellRegistered {
            shell_id: shell_id.clone(),
        },
    )
    .await?;

    tracing::info!("Shell supervisor {shell_id} connected (workstream {workstream_id})");

    let mut exit_code: Option<i32> = None;
    loop {
        tokio::select! {
            biased;

            // Message from supervisor (PTY output or exit notification)
            msg_result = vex_cli::framing::recv::<_, ShellMsg>(&mut net_read) => {
                match msg_result {
                    Ok(ShellMsg::Out { data }) => {
                        if let Ok(raw) = b64.decode(&data) {
                            let mut buf = runtime.output_buf.lock().await;
                            ring_append(&mut buf, &raw);
                        }
                        let _ = runtime.broadcast_tx.send(ShellMsg::Out { data });
                    }
                    Ok(ShellMsg::Exited { code }) => {
                        exit_code = code;
                        let _ = runtime.broadcast_tx.send(ShellMsg::Exited { code });
                        break;
                    }
                    Ok(_) => {} // unexpected from supervisor
                    Err(_) => {
                        // Supervisor disconnected unexpectedly
                        let _ = runtime.broadcast_tx.send(ShellMsg::Exited { code: None });
                        break;
                    }
                }
            }

            // Input / resize from an attached client → forward to supervisor
            Some(msg) = input_rx.recv() => {
                if vex_cli::framing::send(&mut net_write, &msg).await.is_err() {
                    break;
                }
            }
        }
    }

    // Cleanup
    state.shell_runtimes.lock().await.remove(&shell_id);
    {
        let mut store = state.repo_store.lock().await;
        if let Some((ri, wi, si)) = store.shell_indices(&shell_id) {
            let sh = &mut store.repos[ri].workstreams[wi].shells[si];
            sh.status = ShellStatus::Exited;
            sh.exited_at = Some(unix_ts());
            sh.exit_code = exit_code;
        }
        let _ = store.save();
    }

    tracing::info!("Shell {shell_id} exited (code={exit_code:?})");
    Ok(())
}

/// Upgrade a connection to PTY client mode after `AttachShell`.
///
/// Replays the ring buffer, subscribes to live output, and forwards
/// client keyboard input / resize events to the PTY supervisor.
async fn handle_shell_attach_streaming<S>(
    stream: S,
    state: &Arc<AppState>,
    shell_id: String,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;

    // Look up the runtime (supervisor must have registered first)
    let runtime = state.shell_runtimes.lock().await.get(&shell_id).cloned();

    let runtime = match runtime {
        Some(r) => r,
        None => {
            let mut s = stream;
            let _ = vex_cli::framing::send(&mut s, &Response::Error(VexProtoError::NotFound)).await;
            return Ok(());
        }
    };

    let (mut net_read, mut net_write) = tokio::io::split(stream);

    // Acknowledge attach
    vex_cli::framing::send(&mut net_write, &Response::ShellAttached).await?;

    // Replay scrollback
    {
        let buf = runtime.output_buf.lock().await;
        if !buf.is_empty() {
            let encoded = b64.encode(&*buf);
            vex_cli::framing::send(&mut net_write, &ShellMsg::Out { data: encoded }).await?;
        }
    }

    // Subscribe to live output and clone the input sender
    let mut broadcast_rx = runtime.broadcast_tx.subscribe();
    let input_tx = runtime.input_tx.clone();

    tracing::info!("Client attached to shell {shell_id}");

    loop {
        tokio::select! {
            biased;

            // Live output from supervisor → client
            msg = broadcast_rx.recv() => {
                match msg {
                    Ok(shell_msg) => {
                        let is_exit = matches!(shell_msg, ShellMsg::Exited { .. });
                        vex_cli::framing::send(&mut net_write, &shell_msg).await?;
                        if is_exit {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Re-sync: replay the current ring buffer
                        let buf = runtime.output_buf.lock().await;
                        if !buf.is_empty() {
                            let encoded = b64.encode(&*buf);
                            let _ = vex_cli::framing::send(
                                &mut net_write,
                                &ShellMsg::Out { data: encoded },
                            )
                            .await;
                        }
                    }
                    Err(_) => break, // supervisor gone
                }
            }

            // Input / resize from client → supervisor
            msg_result = vex_cli::framing::recv::<_, ShellMsg>(&mut net_read) => {
                match msg_result {
                    Ok(msg) => match &msg {
                        ShellMsg::In { .. } | ShellMsg::Resize { .. } => {
                            let _ = input_tx.send(msg).await;
                        }
                        _ => break, // client sent unexpected message
                    },
                    Err(_) => break, // client disconnected
                }
            }
        }
    }

    tracing::info!("Client detached from shell {shell_id}");
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn err(msg: String) -> Response {
    Response::Error(VexProtoError::Internal(msg))
}

/// Build `<cmd_parts> '<prompt>'` with single-quote escaping for the prompt.
/// If prompt is empty the base command is returned without a trailing argument.
fn build_send_keys_cmd(cmd_parts: &[&str], prompt: &str) -> String {
    if prompt.is_empty() {
        return cmd_parts.join(" ");
    }
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

// ── Git helpers for worktree creation ─────────────────────────────────────────

/// Check whether a local branch exists: `git branch --list <branch>`
async fn local_branch_exists(repo_path: &str, branch: &str) -> bool {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("branch")
        .arg("--list")
        .arg(branch)
        .output()
        .await
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// Check whether a remote tracking branch exists: `git rev-parse --verify origin/<branch>`
async fn remote_branch_exists(repo_path: &str, branch: &str) -> bool {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("rev-parse")
        .arg("--verify")
        .arg(format!("origin/{branch}"))
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `git fetch origin` — returns an error string on failure, Ok(()) on success.
async fn git_fetch_origin(repo_path: &str) -> Result<(), String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("fetch")
        .arg("origin")
        .output()
        .await;
    match out {
        Err(e) => Err(format!("failed to run git fetch: {e}")),
        Ok(o) if !o.status.success() => Err(format!(
            "failed to fetch origin: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Ok(_) => Ok(()),
    }
}

/// `git worktree add <path> <branch>` — check out an existing local branch.
async fn worktree_add_branch(
    repo_path: &str,
    worktree_path: &std::path::Path,
    branch: &str,
) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("worktree")
        .arg("add")
        .arg(worktree_path)
        .arg(branch)
        .output()
        .await
}

/// `git worktree add <path> --track -b <branch> origin/<branch>`
/// — create a new local tracking branch from origin.
async fn worktree_add_tracking(
    repo_path: &str,
    worktree_path: &std::path::Path,
    branch: &str,
) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("worktree")
        .arg("add")
        .arg(worktree_path)
        .arg("--track")
        .arg("-b")
        .arg(branch)
        .arg(format!("origin/{branch}"))
        .output()
        .await
}

/// `git worktree add <path> -b <branch> <start_point>`
/// — create a new local branch from an explicit ref.
async fn worktree_add_new_branch(
    repo_path: &str,
    worktree_path: &std::path::Path,
    branch: &str,
    start_point: &str,
) -> std::io::Result<std::process::Output> {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("worktree")
        .arg("add")
        .arg(worktree_path)
        .arg("-b")
        .arg(branch)
        .arg(start_point)
        .output()
        .await
}

/// List all branches (`git branch -a`) as a display string.
async fn list_all_branches(repo_path: &str) -> String {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("branch")
        .arg("-a")
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{RING_MAX, build_send_keys_cmd, ring_append};

    #[test]
    fn build_cmd_empty_prompt_returns_base_command() {
        assert_eq!(
            build_send_keys_cmd(&["claude", "--model", "sonnet"], ""),
            "claude --model sonnet"
        );
    }

    #[test]
    fn build_cmd_simple_prompt() {
        assert_eq!(
            build_send_keys_cmd(&["claude"], "write a test"),
            "claude 'write a test'"
        );
    }

    #[test]
    fn build_cmd_prompt_with_single_quotes() {
        // Single quotes in prompt must be escaped for shell safety
        assert_eq!(
            build_send_keys_cmd(&["claude"], "it's a test"),
            "claude 'it'\\''s a test'"
        );
    }

    #[test]
    fn build_cmd_multi_word_base() {
        assert_eq!(
            build_send_keys_cmd(
                &["npx", "claude", "--dangerously-skip-permissions"],
                "fix bug"
            ),
            "npx claude --dangerously-skip-permissions 'fix bug'"
        );
    }

    #[test]
    fn ring_append_basic() {
        let mut buf = Vec::new();
        ring_append(&mut buf, b"hello");
        assert_eq!(buf, b"hello");
    }

    #[test]
    fn ring_append_stays_within_max() {
        let mut buf = Vec::new();
        let chunk = vec![b'a'; RING_MAX / 2 + 100];
        ring_append(&mut buf, &chunk);
        ring_append(&mut buf, &chunk);
        assert!(buf.len() <= RING_MAX);
    }

    #[test]
    fn ring_append_trims_oldest_bytes() {
        let mut buf = Vec::new();
        ring_append(&mut buf, &vec![b'a'; RING_MAX]);
        ring_append(&mut buf, b"NEW");
        assert_eq!(buf.len(), RING_MAX);
        assert!(buf.ends_with(b"NEW"));
    }

    #[test]
    fn ring_append_exact_max_does_not_trim() {
        let mut buf = Vec::new();
        ring_append(&mut buf, &vec![b'x'; RING_MAX]);
        assert_eq!(buf.len(), RING_MAX);
    }
}
