use vex_cli as vex_proto;

use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use vex_proto::{Command, Response, Transport, VexProtoError};

use crate::state::AppState;
use crate::tmux;

pub mod attach;
pub mod http;
pub mod tcp;
pub mod unix;

/// Result of dispatching a command — either a simple response or a request to
/// enter PTY attach mode (TCP-only).
pub enum DispatchResult {
    Reply(Response),
    AttachShell {
        response: Response,
        tmux_target: String,
    },
}

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
        let cmd: Command = match vex_proto::framing::recv(&mut stream).await {
            Ok(c) => c,
            Err(_) => break, // connection closed or broken
        };

        match dispatch(cmd, &state, &transport, &token_id).await {
            DispatchResult::Reply(response) => {
                vex_proto::framing::send(&mut stream, &response).await?;
            }
            DispatchResult::AttachShell {
                response,
                tmux_target,
            } => {
                // Send the ShellAttachReady response, then switch to PTY bridge
                vex_proto::framing::send(&mut stream, &response).await?;
                if let Err(e) = attach::run_pty_bridge(stream, &tmux_target).await {
                    tracing::warn!("PTY bridge error: {e}");
                }
                // Connection is consumed — no more commands on this stream
                return Ok(());
            }
        }
    }
    Ok(())
}

pub async fn dispatch(
    cmd: Command,
    state: &Arc<AppState>,
    transport: &Transport,
    token_id: &Option<String>,
) -> DispatchResult {
    match cmd {
        Command::Status => DispatchResult::Reply(Response::DaemonStatus(vex_proto::DaemonStatus {
            uptime_secs: state.uptime_secs(),
            connected_clients: state.connected_clients(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })),

        Command::Whoami => DispatchResult::Reply(Response::ClientInfo(vex_proto::ClientInfo {
            token_id: token_id.clone(),
            is_local: matches!(transport, Transport::Unix),
        })),

        Command::PairCreate { label, expire_secs } => {
            if matches!(transport, Transport::Tcp) {
                return DispatchResult::Reply(Response::Error(VexProtoError::LocalOnly));
            }
            let mut store = state.token_store.lock().await;
            match store.generate(label, expire_secs) {
                Ok((token, secret)) => {
                    DispatchResult::Reply(Response::Pair(vex_proto::PairPayload {
                        token_id: token.token_id,
                        token_secret: secret,
                        host: None,
                    }))
                }
                Err(e) => {
                    DispatchResult::Reply(Response::Error(VexProtoError::Internal(e.to_string())))
                }
            }
        }

        Command::PairList => {
            if matches!(transport, Transport::Tcp) {
                return DispatchResult::Reply(Response::Error(VexProtoError::LocalOnly));
            }
            let store = state.token_store.lock().await;
            let clients = store
                .list()
                .iter()
                .map(|t| vex_proto::PairedClient {
                    token_id: t.token_id.clone(),
                    label: t.label.clone(),
                    created_at: t.created_at.to_rfc3339(),
                    expires_at: t.expires_at.map(|dt| dt.to_rfc3339()),
                    last_seen: t.last_seen.map(|dt| dt.to_rfc3339()),
                })
                .collect();
            DispatchResult::Reply(Response::PairedClients(clients))
        }

        Command::PairRevoke { id } => {
            if matches!(transport, Transport::Tcp) {
                return DispatchResult::Reply(Response::Error(VexProtoError::LocalOnly));
            }
            let mut store = state.token_store.lock().await;
            if store.revoke(&id) {
                DispatchResult::Reply(Response::Ok)
            } else {
                DispatchResult::Reply(Response::Error(VexProtoError::NotFound))
            }
        }

        Command::PairRevokeAll => {
            if matches!(transport, Transport::Tcp) {
                return DispatchResult::Reply(Response::Error(VexProtoError::LocalOnly));
            }
            let mut store = state.token_store.lock().await;
            let count = store.revoke_all();
            DispatchResult::Reply(Response::Revoked(count as u32))
        }

        Command::ProjectRegister { name, repo, path } => {
            if matches!(transport, Transport::Tcp) {
                return DispatchResult::Reply(Response::Error(VexProtoError::LocalOnly));
            }
            let mut store = state.project_store.lock().await;
            match store.register(name.clone(), repo, path) {
                Ok(entry) => DispatchResult::Reply(Response::Project(vex_proto::ProjectInfo {
                    name,
                    repo: entry.repo,
                    path: entry.path,
                })),
                Err(e) => {
                    DispatchResult::Reply(Response::Error(VexProtoError::Internal(e.to_string())))
                }
            }
        }

        Command::ProjectUnregister { name } => {
            if matches!(transport, Transport::Tcp) {
                return DispatchResult::Reply(Response::Error(VexProtoError::LocalOnly));
            }
            let mut store = state.project_store.lock().await;
            if store.unregister(&name) {
                DispatchResult::Reply(Response::Ok)
            } else {
                DispatchResult::Reply(Response::Error(VexProtoError::NotFound))
            }
        }

        Command::ProjectList => {
            let store = state.project_store.lock().await;
            let projects = store
                .list()
                .iter()
                .map(|(name, cfg)| vex_proto::ProjectInfo {
                    name: name.clone(),
                    repo: cfg.repo.clone(),
                    path: cfg.path.clone(),
                })
                .collect();
            DispatchResult::Reply(Response::Projects(projects))
        }

        Command::WorkstreamCreate { project_name, name } => {
            let mut store = state.project_store.lock().await;
            match store.create_workstream(&project_name, name.clone()) {
                Ok(()) => DispatchResult::Reply(Response::Workstream(vex_proto::WorkstreamInfo {
                    name,
                    project_name,
                    shell_count: 0,
                })),
                Err(e) => {
                    DispatchResult::Reply(Response::Error(VexProtoError::Internal(e.to_string())))
                }
            }
        }

        Command::WorkstreamList { project_name } => {
            let store = state.project_store.lock().await;
            match store.list_workstreams(&project_name) {
                Ok(ws) => {
                    let shell_store = state.shell_store.lock().await;
                    let items = ws
                        .iter()
                        .map(|w| {
                            let shell_count = shell_store.list(&project_name, &w.name).len() as u32;
                            vex_proto::WorkstreamInfo {
                                name: w.name.clone(),
                                project_name: project_name.clone(),
                                shell_count,
                            }
                        })
                        .collect();
                    DispatchResult::Reply(Response::Workstreams(items))
                }
                Err(e) => {
                    DispatchResult::Reply(Response::Error(VexProtoError::Internal(e.to_string())))
                }
            }
        }

        Command::WorkstreamDelete { project_name, name } => {
            // Clean up shells + tmux session before deleting workstream config
            {
                let mut shell_store = state.shell_store.lock().await;
                shell_store.remove_all(&project_name, &name);
            }
            let session = tmux::session_name(&project_name, &name);
            if tmux::has_session(&session) {
                let _ = tmux::kill_session(&session);
            }

            let mut store = state.project_store.lock().await;
            match store.delete_workstream(&project_name, &name) {
                Ok(true) => DispatchResult::Reply(Response::Ok),
                Ok(false) => DispatchResult::Reply(Response::Error(VexProtoError::NotFound)),
                Err(e) => {
                    DispatchResult::Reply(Response::Error(VexProtoError::Internal(e.to_string())))
                }
            }
        }

        Command::ShellCreate {
            project_name,
            workstream_name,
        } => {
            if matches!(transport, Transport::Tcp) {
                return DispatchResult::Reply(Response::Error(VexProtoError::LocalOnly));
            }

            // Verify workstream exists and get shell program
            let shell_cmd = {
                let store = state.project_store.lock().await;
                match store.list_workstreams(&project_name) {
                    Ok(ws) if ws.iter().any(|w| w.name == workstream_name) => {
                        store.config().defaults.shell.clone()
                    }
                    Ok(_) => {
                        return DispatchResult::Reply(Response::Error(VexProtoError::Internal(
                            format!(
                                "workstream '{workstream_name}' not found in project '{project_name}'"
                            ),
                        )));
                    }
                    Err(e) => {
                        return DispatchResult::Reply(Response::Error(VexProtoError::Internal(
                            e.to_string(),
                        )));
                    }
                }
            };

            let session = tmux::session_name(&project_name, &workstream_name);
            let window_index = if tmux::has_session(&session) {
                match tmux::new_window(&session, &shell_cmd) {
                    Ok(idx) => idx,
                    Err(e) => {
                        return DispatchResult::Reply(Response::Error(VexProtoError::Internal(
                            e.to_string(),
                        )));
                    }
                }
            } else {
                match tmux::new_session(&session, &shell_cmd) {
                    Ok(idx) => idx,
                    Err(e) => {
                        return DispatchResult::Reply(Response::Error(VexProtoError::Internal(
                            e.to_string(),
                        )));
                    }
                }
            };

            let mut shell_store = state.shell_store.lock().await;
            let id = shell_store.add(&project_name, &workstream_name, window_index);

            DispatchResult::Reply(Response::Shell(vex_proto::ShellInfo {
                id,
                project_name,
                workstream_name,
            }))
        }

        Command::ShellList {
            project_name,
            workstream_name,
        } => {
            // Verify workstream exists
            {
                let store = state.project_store.lock().await;
                match store.list_workstreams(&project_name) {
                    Ok(ws) if ws.iter().any(|w| w.name == workstream_name) => {}
                    Ok(_) => {
                        return DispatchResult::Reply(Response::Error(VexProtoError::Internal(
                            format!(
                                "workstream '{workstream_name}' not found in project '{project_name}'"
                            ),
                        )));
                    }
                    Err(e) => {
                        return DispatchResult::Reply(Response::Error(VexProtoError::Internal(
                            e.to_string(),
                        )));
                    }
                }
            }

            let shell_store = state.shell_store.lock().await;
            let shells: Vec<vex_proto::ShellInfo> = shell_store
                .list(&project_name, &workstream_name)
                .iter()
                .map(|e| vex_proto::ShellInfo {
                    id: e.id.clone(),
                    project_name: project_name.clone(),
                    workstream_name: workstream_name.clone(),
                })
                .collect();
            DispatchResult::Reply(Response::Shells(shells))
        }

        Command::ShellDelete {
            project_name,
            workstream_name,
            shell_id,
        } => {
            if matches!(transport, Transport::Tcp) {
                return DispatchResult::Reply(Response::Error(VexProtoError::LocalOnly));
            }

            let (removed, has_remaining) = {
                let mut shell_store = state.shell_store.lock().await;
                let removed = shell_store.remove(&project_name, &workstream_name, &shell_id);
                let has_remaining = shell_store.has_shells(&project_name, &workstream_name);
                (removed, has_remaining)
            };

            match removed {
                Some(entry) => {
                    let session = tmux::session_name(&project_name, &workstream_name);
                    if has_remaining {
                        let _ = tmux::kill_window(&session, entry.tmux_window_index);
                    } else {
                        let _ = tmux::kill_session(&session);
                    }
                    DispatchResult::Reply(Response::Ok)
                }
                None => DispatchResult::Reply(Response::Error(VexProtoError::NotFound)),
            }
        }

        Command::ShellAttach {
            project_name,
            workstream_name,
            shell_id,
        } => {
            // Resolve shell_id — if None, pick the first shell
            let resolved_id = if let Some(id) = shell_id {
                id
            } else {
                let shell_store = state.shell_store.lock().await;
                let shells = shell_store.list(&project_name, &workstream_name);
                match shells.first() {
                    Some(entry) => entry.id.clone(),
                    None => {
                        return DispatchResult::Reply(Response::Error(VexProtoError::Internal(
                            format!(
                                "no shells in workstream '{workstream_name}' of project '{project_name}'"
                            ),
                        )));
                    }
                }
            };

            // Look up the shell's tmux window index
            let window_index = {
                let shell_store = state.shell_store.lock().await;
                let shells = shell_store.list(&project_name, &workstream_name);
                match shells.iter().find(|e| e.id == resolved_id) {
                    Some(entry) => entry.tmux_window_index,
                    None => {
                        return DispatchResult::Reply(Response::Error(VexProtoError::NotFound));
                    }
                }
            };

            let session = tmux::session_name(&project_name, &workstream_name);
            let tmux_target = format!("{session}:{window_index}");

            match transport {
                Transport::Unix => {
                    // Local client will exec tmux attach directly
                    DispatchResult::Reply(Response::ShellAttachReady {
                        tmux_target: tmux_target.clone(),
                    })
                }
                Transport::Tcp => {
                    // Remote: server enters PTY bridge mode
                    DispatchResult::AttachShell {
                        response: Response::ShellAttachReady {
                            tmux_target: tmux_target.clone(),
                        },
                        tmux_target,
                    }
                }
            }
        }
    }
}
