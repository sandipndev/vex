use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};
use uuid::Uuid;
use vex_cli::proto::{
    ClientMessage, Frame, ServerMessage, read_frame, send_server_message, write_data,
};

use super::session::SessionManager;

struct AttachState {
    session_id: Uuid,
    output_rx: broadcast::Receiver<Vec<u8>>,
    event_rx: broadcast::Receiver<ServerMessage>,
}

pub async fn handle_connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    session_manager: Arc<SessionManager>,
) {
    if let Err(e) = handle_connection_inner(stream, &session_manager).await {
        warn!("connection handler error: {}", e);
    }
}

async fn handle_connection_inner<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    stream: S,
    session_manager: &SessionManager,
) -> Result<()> {
    let client_id = Uuid::new_v4();
    let (reader, mut writer) = tokio::io::split(stream);

    // Spawn a dedicated frame-reader task so that read_frame is never
    // cancelled by tokio::select!.  read_exact is NOT cancellation-safe:
    // if select! drops it mid-read, consumed bytes are lost and the frame
    // protocol desynchronises.  Channel recv IS cancellation-safe.
    let (frame_tx, mut frame_rx) = mpsc::channel::<Result<Frame>>(64);
    let reader_task = tokio::spawn(async move {
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
        session_manager,
    )
    .await;

    reader_task.abort();

    // Ensure we unregister the client on any exit path
    if let Some(state) = attached {
        session_manager
            .client_detach(state.session_id, client_id)
            .await;
    }

    result
}

async fn connection_loop<W: AsyncWrite + Unpin>(
    client_id: Uuid,
    frame_rx: &mut mpsc::Receiver<Result<Frame>>,
    writer: &mut W,
    attached: &mut Option<AttachState>,
    session_manager: &SessionManager,
) -> Result<()> {
    loop {
        if let Some(state) = attached {
            let session_id = state.session_id;
            // Attached state: select on client frames, session output, or events
            tokio::select! {
                msg = frame_rx.recv() => {
                    match msg {
                        Some(Ok(Frame::Data(data))) => {
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
                        Some(Ok(Frame::Control(data))) => {
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
                                    handle_control_idle(other, session_manager, writer).await?;
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
            // Idle state: only read client frames (no select, no cancellation issue)
            match frame_rx.recv().await {
                Some(Ok(Frame::Control(data))) => {
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
                    } else {
                        handle_control_idle(msg, session_manager, writer).await?;
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
    session_manager: &SessionManager,
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
        ClientMessage::ListAgents => {
            let sessions = session_manager.list_agent_sessions().await;
            send_server_message(writer, &ServerMessage::AgentSessions { sessions }).await?;
        }
    }
    Ok(())
}
