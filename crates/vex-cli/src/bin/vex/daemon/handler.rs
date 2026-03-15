use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};
use uuid::Uuid;
use vex_cli::proto::{
    ClientMessage, Frame, ServerMessage, read_frame, send_server_message, write_data,
};

use super::agent::AgentStore;
use super::session::SessionManager;

struct AttachState {
    session_id: Uuid,
    output_rx: broadcast::Receiver<Vec<u8>>,
    event_rx: broadcast::Receiver<ServerMessage>,
}

pub async fn handle_connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    manager: Arc<SessionManager>,
    agent_store: AgentStore,
) {
    if let Err(e) = handle_connection_inner(stream, &manager, &agent_store).await {
        warn!("connection handler error: {}", e);
    }
}

async fn handle_connection_inner<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    manager: &SessionManager,
    agent_store: &AgentStore,
) -> Result<()> {
    let client_id = Uuid::new_v4();
    let (reader, mut writer) = tokio::io::split(stream);

    // Spawn frame reader task (read_frame is not cancel-safe in tokio::select!)
    let (frame_tx, mut frame_rx) = mpsc::channel::<Result<Frame>>(64);
    let frame_handle = tokio::spawn(async move {
        let mut reader = reader;
        loop {
            match read_frame(&mut reader).await {
                Ok(Some(frame)) => {
                    if frame_tx.send(Ok(frame)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = frame_tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });

    let mut attached: Option<AttachState> = None;
    let result = connection_loop(
        client_id,
        &mut frame_rx,
        &mut writer,
        &mut attached,
        manager,
        agent_store,
    )
    .await;

    frame_handle.abort();

    // Ensure we unregister the client on any exit path
    if let Some(state) = attached {
        manager.client_detach(state.session_id, client_id).await;
    }

    result
}

async fn connection_loop<W: AsyncWrite + Unpin>(
    client_id: Uuid,
    frame_rx: &mut mpsc::Receiver<Result<Frame>>,
    writer: &mut W,
    attached: &mut Option<AttachState>,
    manager: &SessionManager,
    agent_store: &AgentStore,
) -> Result<()> {
    loop {
        if let Some(state) = attached {
            let session_id = state.session_id;
            // Attached state: select on client frames, session output, or events
            tokio::select! {
                result = frame_rx.recv() => {
                    match result {
                        Some(Ok(Frame::Data(data))) => {
                            if let Err(e) = manager.write_input(session_id, &data).await {
                                warn!("write_input error: {}", e);
                                send_server_message(
                                    writer,
                                    &ServerMessage::Error {
                                        message: format!("session write error: {}", e),
                                    },
                                ).await?;
                                manager.client_detach(session_id, client_id).await;
                                *attached = None;
                            }
                        }
                        Some(Ok(Frame::Control(data))) => {
                            let msg: ClientMessage = serde_json::from_slice(&data)?;
                            match msg {
                                ClientMessage::DetachSession => {
                                    info!("client {} detaching from session {}", client_id, session_id);
                                    manager.client_detach(session_id, client_id).await;
                                    send_server_message(writer, &ServerMessage::Detached).await?;
                                    *attached = None;
                                }
                                ClientMessage::ResizeSession { id, cols, rows } => {
                                    if let Err(e) = manager.client_resize(id, client_id, cols, rows).await {
                                        send_server_message(writer, &ServerMessage::Error {
                                            message: format!("resize error: {}", e),
                                        }).await?;
                                    }
                                }
                                ClientMessage::KillSession { id } => {
                                    if id == session_id {
                                        manager.client_detach(session_id, client_id).await;
                                        *attached = None;
                                    }
                                    if let Err(e) = manager.kill_session(id).await {
                                        send_server_message(writer, &ServerMessage::Error {
                                            message: format!("kill error: {}", e),
                                        }).await?;
                                    } else {
                                        agent_store.lock().await.remove(&id);
                                        send_server_message(writer, &ServerMessage::SessionEnded {
                                            id,
                                            exit_code: None,
                                        }).await?;
                                    }
                                }
                                other => {
                                    handle_control_idle(other, manager, agent_store, writer).await?;
                                }
                            }
                        }
                        Some(Err(e)) => return Err(e),
                        None => {
                            info!("client {} disconnected while attached to {}", client_id, session_id);
                            break;
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
                            manager.client_detach(session_id, client_id).await;
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
            match frame_rx.recv().await {
                Some(Ok(Frame::Control(data))) => {
                    let msg: ClientMessage = serde_json::from_slice(&data)?;
                    if let ClientMessage::AttachSession { id, cols, rows } = msg {
                        match manager.attach_session(id).await {
                            Ok((scrollback, output_rx)) => {
                                let event_rx = manager.subscribe_events(id).await?;
                                let _ = manager.client_attach(id, client_id, cols, rows).await;
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
                    } else {
                        handle_control_idle(msg, manager, agent_store, writer).await?;
                    }
                }
                Some(Ok(Frame::Data(_))) => {
                    send_server_message(
                        writer,
                        &ServerMessage::Error {
                            message: "not attached to any session".into(),
                        },
                    )
                    .await?;
                }
                Some(Err(e)) => return Err(e),
                None => {
                    info!("client {} disconnected", client_id);
                    break;
                }
            }
        }
    }

    Ok(())
}

async fn handle_control_idle<W: AsyncWrite + Unpin>(
    msg: ClientMessage,
    manager: &SessionManager,
    agent_store: &AgentStore,
    writer: &mut W,
) -> Result<()> {
    match msg {
        ClientMessage::CreateSession { shell } => {
            match manager.create_session(shell, 80, 24).await {
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
            let sessions = manager.list_sessions().await;
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
            if let Err(e) = manager.kill_session(id).await {
                send_server_message(
                    writer,
                    &ServerMessage::Error {
                        message: format!("kill error: {}", e),
                    },
                )
                .await?;
            } else {
                // Immediately remove any agent linked to this session
                agent_store.lock().await.remove(&id);
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
        ClientMessage::AgentList => {
            let agents = agent_store.lock().await;
            let entries = agents.values().map(|a| a.to_entry()).collect();
            send_server_message(
                writer,
                &ServerMessage::AgentListResponse { agents: entries },
            )
            .await?;
        }
        ClientMessage::AgentNotifications => {
            let agents = agent_store.lock().await;
            let entries = agents
                .values()
                .filter(|a| a.needs_intervention)
                .map(|a| a.to_entry())
                .collect();
            send_server_message(
                writer,
                &ServerMessage::AgentListResponse { agents: entries },
            )
            .await?;
        }
        ClientMessage::AgentWatch { session_id } => {
            handle_agent_watch(session_id, agent_store, writer, false).await?;
        }
        ClientMessage::AgentPrompt { session_id, text } => {
            // Write the prompt text + carriage return to the vex session's PTY
            // PTYs in raw mode expect \r, not \n, to submit input
            let input = format!("{}\r", text);
            if let Err(e) = manager.write_input(session_id, input.as_bytes()).await {
                send_server_message(
                    writer,
                    &ServerMessage::Error {
                        message: format!("agent prompt error: {}", e),
                    },
                )
                .await?;
            } else {
                send_server_message(writer, &ServerMessage::AgentPromptSent { session_id }).await?;
                // Stream conversation lines until the agent finishes its turn
                handle_agent_watch(session_id, agent_store, writer, true).await?;
            }
        }
    }
    Ok(())
}

async fn handle_agent_watch<W: AsyncWrite + Unpin>(
    session_id: Uuid,
    agent_store: &AgentStore,
    writer: &mut W,
    until_turn_complete: bool,
) -> Result<()> {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let jsonl_path = {
        let agents = agent_store.lock().await;
        match agents.get(&session_id) {
            Some(info) => info.jsonl_path.clone(),
            None => {
                send_server_message(
                    writer,
                    &ServerMessage::Error {
                        message: format!("no agent found for session {}", session_id),
                    },
                )
                .await?;
                return Ok(());
            }
        }
    };

    // Open the JSONL file, replay last 50 lines, then tail
    let file = match std::fs::File::open(&jsonl_path) {
        Ok(f) => f,
        Err(e) => {
            send_server_message(
                writer,
                &ServerMessage::Error {
                    message: format!("cannot open conversation file: {}", e),
                },
            )
            .await?;
            return Ok(());
        }
    };

    let mut reader = BufReader::new(file);

    // Read all existing lines to replay the last 50
    let mut lines: Vec<String> = Vec::new();
    let mut line_buf = String::new();
    while reader.read_line(&mut line_buf)? > 0 {
        let trimmed = line_buf.trim_end().to_string();
        if !trimmed.is_empty() {
            lines.push(trimmed);
        }
        line_buf.clear();
    }

    let replay_start = lines.len().saturating_sub(50);
    for line in &lines[replay_start..] {
        send_server_message(
            writer,
            &ServerMessage::AgentConversationLine {
                session_id,
                line: line.clone(),
            },
        )
        .await?;
    }

    // Now tail the file using inotify
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel(64);

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && matches!(event.kind, EventKind::Modify(_))
        {
            let _ = notify_tx.blocking_send(());
        }
    })?;

    watcher.watch(jsonl_path.as_ref(), RecursiveMode::NonRecursive)?;

    // Current file position is at end from reading lines
    let mut pos = reader.stream_position()?;

    // For turn completion detection: track whether we've seen a non-assistant
    // line in new (non-replayed) lines. The sequence is:
    // 1. Agent was idle (last line = "assistant")
    // 2. Prompt sent → "user" line appears → seen_non_assistant = true
    // 3. Agent responds → "assistant" line appears → turn complete
    let mut seen_non_assistant = false;

    loop {
        // Check if agent is still alive
        {
            let agents = agent_store.lock().await;
            if !agents.contains_key(&session_id) {
                send_server_message(writer, &ServerMessage::AgentWatchEnd { session_id }).await?;
                return Ok(());
            }
        }

        // Wait for file modification with a timeout for periodic liveness checks
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), notify_rx.recv()).await;

        // Read new lines
        reader.seek(SeekFrom::Start(pos))?;
        line_buf.clear();
        while reader.read_line(&mut line_buf)? > 0 {
            let trimmed = line_buf.trim_end().to_string();
            if !trimmed.is_empty() {
                send_server_message(
                    writer,
                    &ServerMessage::AgentConversationLine {
                        session_id,
                        line: trimmed.clone(),
                    },
                )
                .await?;

                if until_turn_complete
                    && let Ok(v) = serde_json::from_str::<serde_json::Value>(&trimmed)
                    && let Some(t) = v.get("type").and_then(|t| t.as_str())
                {
                    if t != "assistant" {
                        seen_non_assistant = true;
                    } else if seen_non_assistant {
                        // Agent finished its turn
                        send_server_message(writer, &ServerMessage::AgentWatchEnd { session_id })
                            .await?;
                        return Ok(());
                    }
                }
            }
            line_buf.clear();
        }
        pos = reader.stream_position()?;
    }
}
