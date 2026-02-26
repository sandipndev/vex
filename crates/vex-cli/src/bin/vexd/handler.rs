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

pub async fn handle_connection(stream: UnixStream, manager: Arc<SessionManager>) {
    if let Err(e) = handle_connection_inner(stream, &manager).await {
        warn!("connection handler error: {}", e);
    }
}

async fn handle_connection_inner(stream: UnixStream, manager: &SessionManager) -> Result<()> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut attached: Option<(Uuid, broadcast::Receiver<Vec<u8>>)> = None;

    loop {
        if let Some((session_id, ref mut output_rx)) = attached {
            // Attached state: select on client frames OR session output
            tokio::select! {
                frame = read_frame(&mut reader) => {
                    match frame? {
                        None => {
                            info!("client disconnected while attached to {}", session_id);
                            break;
                        }
                        Some(Frame::Data(data)) => {
                            if let Err(e) = manager.write_input(session_id, &data).await {
                                warn!("write_input error: {}", e);
                                send_server_message(
                                    &mut writer,
                                    &ServerMessage::Error {
                                        message: format!("session write error: {}", e),
                                    },
                                ).await?;
                                attached = None;
                            }
                        }
                        Some(Frame::Control(data)) => {
                            let msg: ClientMessage = serde_json::from_slice(&data)?;
                            match msg {
                                ClientMessage::DetachSession => {
                                    info!("client detaching from session {}", session_id);
                                    send_server_message(&mut writer, &ServerMessage::Detached).await?;
                                    attached = None;
                                }
                                ClientMessage::ResizeSession { id, cols, rows } => {
                                    if let Err(e) = manager.resize_session(id, cols, rows).await {
                                        send_server_message(&mut writer, &ServerMessage::Error {
                                            message: format!("resize error: {}", e),
                                        }).await?;
                                    }
                                }
                                ClientMessage::KillSession { id } => {
                                    if id == session_id {
                                        attached = None;
                                    }
                                    if let Err(e) = manager.kill_session(id).await {
                                        send_server_message(&mut writer, &ServerMessage::Error {
                                            message: format!("kill error: {}", e),
                                        }).await?;
                                    }
                                }
                                other => {
                                    handle_control_idle(other, manager, &mut writer).await?;
                                }
                            }
                        }
                    }
                }
                output = output_rx.recv() => {
                    match output {
                        Ok(data) => {
                            write_data(&mut writer, &data).await?;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            // Session ended
                            info!("session {} output closed", session_id);
                            send_server_message(&mut writer, &ServerMessage::SessionEnded {
                                id: session_id,
                                exit_code: None,
                            }).await?;
                            attached = None;
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("output lagged by {} messages for session {}", n, session_id);
                        }
                    }
                }
            }
        } else {
            // Idle state: only read client frames
            match read_frame(&mut reader).await? {
                None => {
                    info!("client disconnected");
                    break;
                }
                Some(Frame::Control(data)) => {
                    let msg: ClientMessage = serde_json::from_slice(&data)?;
                    if let ClientMessage::AttachSession { id } = msg {
                        if !manager.session_exists(id).await {
                            send_server_message(
                                &mut writer,
                                &ServerMessage::Error {
                                    message: format!("session not found: {}", id),
                                },
                            )
                            .await?;
                        } else {
                            let output_rx = manager.subscribe_output(id).await?;
                            send_server_message(&mut writer, &ServerMessage::Attached { id })
                                .await?;
                            attached = Some((id, output_rx));
                        }
                    } else {
                        handle_control_idle(msg, manager, &mut writer).await?;
                    }
                }
                Some(Frame::Data(_)) => {
                    send_server_message(
                        &mut writer,
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
        ClientMessage::ResizeSession { id, cols, rows } => {
            if let Err(e) = manager.resize_session(id, cols, rows).await {
                send_server_message(
                    writer,
                    &ServerMessage::Error {
                        message: format!("resize error: {}", e),
                    },
                )
                .await?;
            }
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
    }
    Ok(())
}
