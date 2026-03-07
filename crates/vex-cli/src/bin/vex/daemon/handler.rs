use std::sync::Arc;

use anyhow::Result;
use tokio::io::WriteHalf;
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
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

pub async fn handle_connection(
    stream: UnixStream,
    manager: Arc<SessionManager>,
    token: Arc<String>,
) {
    if let Err(e) = handle_connection_inner(stream, &manager, &token).await {
        warn!("connection handler error: {}", e);
    }
}

async fn handle_connection_inner(
    stream: UnixStream,
    manager: &SessionManager,
    token: &str,
) -> Result<()> {
    let client_id = Uuid::new_v4();
    let (mut reader, mut writer) = tokio::io::split(stream);

    // First frame must be Authenticate
    match read_frame(&mut reader).await? {
        Some(Frame::Control(data)) => {
            let msg: ClientMessage = serde_json::from_slice(&data)?;
            match msg {
                ClientMessage::Authenticate {
                    token: client_token,
                } => {
                    if client_token != token {
                        send_server_message(
                            &mut writer,
                            &ServerMessage::Error {
                                message: "invalid token".into(),
                            },
                        )
                        .await?;
                        return Ok(());
                    }
                    send_server_message(&mut writer, &ServerMessage::Authenticated).await?;
                }
                _ => {
                    send_server_message(
                        &mut writer,
                        &ServerMessage::Error {
                            message: "authentication required".into(),
                        },
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
        Some(Frame::Data(_)) => {
            send_server_message(
                &mut writer,
                &ServerMessage::Error {
                    message: "authentication required".into(),
                },
            )
            .await?;
            return Ok(());
        }
        None => return Ok(()),
    }

    let mut attached: Option<AttachState> = None;
    let result = connection_loop(client_id, &mut reader, &mut writer, &mut attached, manager).await;

    // Ensure we unregister the client on any exit path
    if let Some(state) = attached {
        manager.client_detach(state.session_id, client_id).await;
    }

    result
}

async fn connection_loop(
    client_id: Uuid,
    reader: &mut tokio::io::ReadHalf<UnixStream>,
    writer: &mut WriteHalf<UnixStream>,
    attached: &mut Option<AttachState>,
    manager: &SessionManager,
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
                        Some(Frame::Control(data)) => {
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
                                        send_server_message(writer, &ServerMessage::SessionEnded {
                                            id,
                                            exit_code: None,
                                        }).await?;
                                    }
                                }
                                other => {
                                    handle_control_idle(other, manager, writer).await?;
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
            match read_frame(reader).await? {
                None => {
                    info!("client {} disconnected", client_id);
                    break;
                }
                Some(Frame::Control(data)) => {
                    let msg: ClientMessage = serde_json::from_slice(&data)?;
                    if let ClientMessage::AttachSession { id } = msg {
                        match manager.attach_session(id).await {
                            Ok((scrollback, output_rx)) => {
                                let event_rx = manager.subscribe_events(id).await?;
                                // Register client with default size; the client
                                // sends a ResizeSession immediately after attach.
                                let _ = manager.client_attach(id, client_id, 80, 24).await;
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
                        handle_control_idle(msg, manager, writer).await?;
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

async fn handle_control_idle(
    msg: ClientMessage,
    manager: &SessionManager,
    writer: &mut WriteHalf<UnixStream>,
) -> Result<()> {
    match msg {
        ClientMessage::CreateSession { shell } => {
            match manager.create_session(shell, 80, 24).await {
                Ok(id) => {
                    info!("created session {}", id);
                    send_server_message(writer, &ServerMessage::SessionCreated { id }).await?;
                }
                Err(e) => {
                    error!("create session error: {}", e);
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
        ClientMessage::Authenticate { .. } => {
            send_server_message(
                writer,
                &ServerMessage::Error {
                    message: "already authenticated".into(),
                },
            )
            .await?;
        }
    }
    Ok(())
}
