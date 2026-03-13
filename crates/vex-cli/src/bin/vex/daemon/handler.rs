use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::broadcast;
use tracing::{info, warn};
use uuid::Uuid;
use vex_cli::proto::{
    ClientMessage, Frame, ServerMessage, read_frame, send_server_message, write_data,
};

use super::agent::{AgentConfig, AgentManager};
use super::session::SessionManager;

struct AttachState {
    session_id: Uuid,
    output_rx: broadcast::Receiver<Vec<u8>>,
    event_rx: broadcast::Receiver<ServerMessage>,
}

pub async fn handle_connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    session_manager: Arc<SessionManager>,
    agent_manager: Arc<AgentManager>,
) {
    if let Err(e) = handle_connection_inner(stream, &session_manager, &agent_manager).await {
        warn!("connection handler error: {}", e);
    }
}

async fn handle_connection_inner<S: AsyncRead + AsyncWrite + Unpin + Send>(
    stream: S,
    session_manager: &SessionManager,
    agent_manager: &AgentManager,
) -> Result<()> {
    let client_id = Uuid::new_v4();
    let (mut reader, mut writer) = tokio::io::split(stream);

    let mut attached: Option<AttachState> = None;
    let result = connection_loop(
        client_id,
        &mut reader,
        &mut writer,
        &mut attached,
        session_manager,
        agent_manager,
    )
    .await;

    // Ensure we unregister the client on any exit path
    if let Some(state) = attached {
        session_manager
            .client_detach(state.session_id, client_id)
            .await;
    }

    result
}

async fn connection_loop<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    client_id: Uuid,
    reader: &mut R,
    writer: &mut W,
    attached: &mut Option<AttachState>,
    session_manager: &SessionManager,
    agent_manager: &AgentManager,
) -> Result<()> {
    loop {
        if let Some(state) = attached {
            let session_id = state.session_id;
            // Attached state: select on client frames, session output, or events
            tokio::select! {
                frame = read_frame(reader) => {
                    match frame? {
                        None => {
                            info!("client {} disconnected while attached to {}", client_id, session_id);
                            break;
                        }
                        Some(Frame::Data(data)) => {
                            if let Err(e) = session_manager.write_input(session_id, &data).await {
                                warn!("write_input error: {}", e);
                                send_server_message(
                                    writer,
                                    &ServerMessage::Error {
                                        message: format!("session write error: {}", e),
                                    },
                                ).await?;
                                session_manager.client_detach(session_id, client_id).await;
                                *attached = None;
                            }
                        }
                        Some(Frame::Control(data)) => {
                            let msg: ClientMessage = serde_json::from_slice(&data)?;
                            match msg {
                                ClientMessage::DetachSession => {
                                    info!("client {} detaching from session {}", client_id, session_id);
                                    session_manager.client_detach(session_id, client_id).await;
                                    send_server_message(writer, &ServerMessage::Detached).await?;
                                    *attached = None;
                                }
                                ClientMessage::ResizeSession { id, cols, rows } => {
                                    if let Err(e) = session_manager.client_resize(id, client_id, cols, rows).await {
                                        send_server_message(writer, &ServerMessage::Error {
                                            message: format!("resize error: {}", e),
                                        }).await?;
                                    }
                                }
                                ClientMessage::KillSession { id } => {
                                    if id == session_id {
                                        session_manager.client_detach(session_id, client_id).await;
                                        *attached = None;
                                    }
                                    if let Err(e) = session_manager.kill_session(id).await {
                                        send_server_message(writer, &ServerMessage::Error {
                                            message: format!("kill error: {}", e),
                                        }).await?;
                                    } else {
                                        send_server_message(writer, &ServerMessage::SessionEnded {
                                            id,
                                            exit_code: None,
                                        }).await?;
                                    }
                                }
                                other => {
                                    handle_control_idle(other, session_manager, agent_manager, writer).await?;
                                }
                            }
                        }
                    }
                }
                output = state.output_rx.recv() => {
                    match output {
                        Ok(data) => {
                            write_data(writer, &data).await?;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("session {} output closed", session_id);
                            session_manager.client_detach(session_id, client_id).await;
                            send_server_message(writer, &ServerMessage::SessionEnded {
                                id: session_id,
                                exit_code: None,
                            }).await?;
                            *attached = None;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("output lagged by {} messages for session {}", n, session_id);
                        }
                    }
                }
                event = state.event_rx.recv() => {
                    match event {
                        Ok(msg) => {
                            send_server_message(writer, &msg).await?;
                        }
                        Err(broadcast::error::RecvError::Closed) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                    }
                }
            }
        } else {
            // Idle state: only read client frames
            match read_frame(reader).await? {
                None => {
                    info!("client {} disconnected", client_id);
                    break;
                }
                Some(Frame::Control(data)) => {
                    let msg: ClientMessage = serde_json::from_slice(&data)?;
                    if let ClientMessage::AttachSession { id, cols, rows } = msg {
                        match session_manager.attach_session(id).await {
                            Ok((scrollback, output_rx)) => {
                                let event_rx = session_manager.subscribe_events(id).await?;
                                let _ = session_manager
                                    .client_attach(id, client_id, cols, rows)
                                    .await;
                                send_server_message(writer, &ServerMessage::Attached { id })
                                    .await?;
                                if !scrollback.is_empty() {
                                    write_data(writer, &scrollback).await?;
                                }
                                *attached = Some(AttachState {
                                    session_id: id,
                                    output_rx,
                                    event_rx,
                                });
                            }
                            Err(e) => {
                                send_server_message(
                                    writer,
                                    &ServerMessage::Error {
                                        message: e.to_string(),
                                    },
                                )
                                .await?;
                            }
                        }
                    } else if let ClientMessage::AgentPrompt { id, prompt } = msg {
                        handle_agent_prompt(id, prompt, agent_manager, reader, writer).await?;
                    } else {
                        handle_control_idle(msg, session_manager, agent_manager, writer).await?;
                    }
                }
                Some(Frame::Data(_)) => {
                    send_server_message(
                        writer,
                        &ServerMessage::Error {
                            message: "not attached to any session".into(),
                        },
                    )
                    .await?;
                }
            }
        }
    }

    Ok(())
}

async fn handle_control_idle<W: AsyncWrite + Unpin>(
    msg: ClientMessage,
    session_manager: &SessionManager,
    agent_manager: &AgentManager,
    writer: &mut W,
) -> Result<()> {
    match msg {
        ClientMessage::CreateSession { shell } => {
            match session_manager.create_session(shell, 80, 24).await {
                Ok(id) => {
                    info!("created session {}", id);
                    send_server_message(writer, &ServerMessage::SessionCreated { id }).await?;
                }
                Err(e) => {
                    tracing::error!("create session error: {}", e);
                    send_server_message(
                        writer,
                        &ServerMessage::Error {
                            message: format!("failed to create session: {}", e),
                        },
                    )
                    .await?;
                }
            }
        }
        ClientMessage::ListSessions => {
            let sessions = session_manager.list_sessions().await;
            send_server_message(writer, &ServerMessage::Sessions { sessions }).await?;
        }
        ClientMessage::ResizeSession { .. } => {
            send_server_message(
                writer,
                &ServerMessage::Error {
                    message: "not attached to any session".into(),
                },
            )
            .await?;
        }
        ClientMessage::KillSession { id } => {
            if let Err(e) = session_manager.kill_session(id).await {
                send_server_message(
                    writer,
                    &ServerMessage::Error {
                        message: format!("kill error: {}", e),
                    },
                )
                .await?;
            } else {
                send_server_message(
                    writer,
                    &ServerMessage::SessionEnded {
                        id,
                        exit_code: None,
                    },
                )
                .await?;
            }
        }
        ClientMessage::DetachSession => {
            send_server_message(
                writer,
                &ServerMessage::Error {
                    message: "not attached".into(),
                },
            )
            .await?;
        }
        ClientMessage::AttachSession { .. } => {
            // Handled in the main loop
        }
        ClientMessage::CreateAgent {
            model,
            permission_mode,
            allowed_tools,
            max_turns,
            cwd,
        } => {
            let config = AgentConfig {
                model,
                permission_mode,
                allowed_tools,
                max_turns,
                cwd,
            };
            let id = agent_manager.create_agent(config).await;
            send_server_message(writer, &ServerMessage::AgentCreated { id }).await?;
        }
        ClientMessage::AgentPrompt { .. } => {
            // Handled in the main loop (needs streaming)
        }
        ClientMessage::AgentStatus { id } => match agent_manager.get_status_full(id).await {
            Ok((info, claude_session_id)) => {
                send_server_message(
                    writer,
                    &ServerMessage::AgentStatusResponse {
                        id: info.id,
                        status: info.status,
                        claude_session_id,
                        model: info.model,
                        turn_count: info.turn_count,
                    },
                )
                .await?;
            }
            Err(e) => {
                send_server_message(
                    writer,
                    &ServerMessage::Error {
                        message: e.to_string(),
                    },
                )
                .await?;
            }
        },
        ClientMessage::ListAgents => {
            let agents = agent_manager.list_agents().await;
            send_server_message(writer, &ServerMessage::Agents { agents }).await?;
        }
        ClientMessage::KillAgent { id } => {
            if let Err(e) = agent_manager.kill_agent(id).await {
                send_server_message(
                    writer,
                    &ServerMessage::Error {
                        message: e.to_string(),
                    },
                )
                .await?;
            } else {
                send_server_message(
                    writer,
                    &ServerMessage::AgentCreated { id }, // ack
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn handle_agent_prompt<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    id: Uuid,
    prompt: String,
    agent_manager: &AgentManager,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    let mut rx = match agent_manager.send_prompt(id, prompt).await {
        Ok(rx) => rx,
        Err(e) => {
            send_server_message(
                writer,
                &ServerMessage::Error {
                    message: e.to_string(),
                },
            )
            .await?;
            return Ok(());
        }
    };

    // Stream events to client until prompt is done or client disconnects
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(server_msg) => {
                        let is_done = matches!(server_msg, ServerMessage::AgentPromptDone { .. });
                        send_server_message(writer, &server_msg).await?;
                        if is_done {
                            break;
                        }
                    }
                    None => {
                        // Channel closed unexpectedly
                        break;
                    }
                }
            }
            frame = read_frame(reader) => {
                match frame? {
                    None => {
                        // Client disconnected; claude process continues in background
                        info!("client disconnected during agent prompt {}", id);
                        return Ok(());
                    }
                    Some(_) => {
                        // Ignore any client messages during streaming
                    }
                }
            }
        }
    }

    Ok(())
}
