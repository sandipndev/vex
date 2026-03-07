use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use tokio::io::{self, AsyncReadExt};
use tokio::net::UnixStream;
use uuid::Uuid;
use vex_cli::proto::{
    ClientMessage, Frame, ServerMessage, read_frame, send_client_message, write_data,
};

async fn connect(socket_path: &Path) -> Result<UnixStream> {
    let stream = UnixStream::connect(socket_path).await?;
    Ok(stream)
}

/// Send a control message and read back a single control response.
async fn request(socket_path: &Path, msg: &ClientMessage) -> Result<ServerMessage> {
    let stream = connect(socket_path).await?;
    let (mut reader, mut writer) = io::split(stream);
    send_client_message(&mut writer, msg).await?;

    match read_frame(&mut reader).await? {
        Some(Frame::Control(data)) => {
            let resp: ServerMessage = serde_json::from_slice(&data)?;
            Ok(resp)
        }
        Some(Frame::Data(_)) => bail!("unexpected data frame"),
        None => bail!("server closed connection"),
    }
}

pub async fn session_create(socket_path: &Path, shell: Option<String>) -> Result<()> {
    let resp = request(socket_path, &ClientMessage::CreateSession { shell }).await?;
    match resp {
        ServerMessage::SessionCreated { id } => {
            println!("{}", id);
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn session_list(socket_path: &Path) -> Result<()> {
    let resp = request(socket_path, &ClientMessage::ListSessions).await?;
    match resp {
        ServerMessage::Sessions { sessions } => {
            if sessions.is_empty() {
                println!("no active sessions");
            } else {
                println!(
                    "{:<36}  {:>4} x {:<4}  {:>7}  CREATED",
                    "ID", "COLS", "ROWS", "CLIENTS"
                );
                for s in sessions {
                    println!(
                        "{:<36}  {:>4} x {:<4}  {:>7}  {}",
                        s.id,
                        s.cols,
                        s.rows,
                        s.client_count,
                        s.created_at.format("%Y-%m-%d %H:%M:%S")
                    );
                }
            }
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn session_kill(socket_path: &Path, id_prefix: &str) -> Result<()> {
    let id = resolve_session_id(socket_path, id_prefix).await?;
    let resp = request(socket_path, &ClientMessage::KillSession { id }).await?;
    match resp {
        ServerMessage::Error { message } => bail!("{}", message),
        _ => {
            println!("killed session {}", id);
            Ok(())
        }
    }
}

pub async fn session_attach(socket_path: &Path, id_prefix: &str) -> Result<()> {
    let id = resolve_session_id(socket_path, id_prefix).await?;

    let stream = connect(socket_path).await?;
    let (mut reader, mut writer) = io::split(stream);

    // Send attach request
    send_client_message(&mut writer, &ClientMessage::AttachSession { id }).await?;

    // Wait for Attached confirmation
    match read_frame(&mut reader).await? {
        Some(Frame::Control(data)) => {
            let resp: ServerMessage = serde_json::from_slice(&data)?;
            match resp {
                ServerMessage::Attached { id: _ } => {}
                ServerMessage::Error { message } => bail!("{}", message),
                other => bail!("unexpected response: {:?}", other),
            }
        }
        _ => bail!("unexpected response from server"),
    }

    // Send current terminal size
    if let Some((terminal_size::Width(cols), terminal_size::Height(rows))) =
        terminal_size::terminal_size()
    {
        send_client_message(
            &mut writer,
            &ClientMessage::ResizeSession { id, cols, rows },
        )
        .await?;
    }

    // Enter raw mode
    let _raw_guard = RawModeGuard::enter()?;

    eprintln!("\r\n[attached to session {}; press Ctrl+] to detach]\r", id);

    // Spawn stdin reader task
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let stdin_handle = tokio::spawn(async move {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 1024];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if stdin_tx.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Spawn SIGWINCH handler
    let (resize_tx, mut resize_rx) = tokio::sync::mpsc::channel::<(u16, u16)>(4);
    let sigwinch_handle = tokio::spawn(async move {
        let mut sig =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).unwrap();
        loop {
            sig.recv().await;
            if let Some((terminal_size::Width(cols), terminal_size::Height(rows))) =
                terminal_size::terminal_size()
                && resize_tx.send((cols, rows)).await.is_err()
            {
                break;
            }
        }
    });

    // Main loop: multiplex stdin, resize signals, and server frames
    let result: Result<()> = loop {
        tokio::select! {
            frame = read_frame(&mut reader) => {
                match frame {
                    Ok(Some(Frame::Data(data))) => {
                        let mut stdout = std::io::stdout().lock();
                        let _ = stdout.write_all(&data);
                        let _ = stdout.flush();
                    }
                    Ok(Some(Frame::Control(data))) => {
                        let msg: ServerMessage = serde_json::from_slice(&data)?;
                        match msg {
                            ServerMessage::Detached => {
                                eprintln!("\r\n[detached]\r");
                                break Ok(());
                            }
                            ServerMessage::SessionEnded { id, exit_code } => {
                                eprintln!("\r\n[session {} ended (exit code: {:?})]\r", id, exit_code);
                                break Ok(());
                            }
                            ServerMessage::Error { message } => {
                                eprintln!("\r\n[error: {}]\r", message);
                                break Ok(());
                            }
                            _ => {}
                        }
                    }
                    Ok(None) => {
                        eprintln!("\r\n[server disconnected]\r");
                        break Ok(());
                    }
                    Err(e) => {
                        break Err(e);
                    }
                }
            }
            Some(data) = stdin_rx.recv() => {
                // Check for Ctrl+] (0x1D)
                if data.contains(&0x1D) {
                    send_client_message(&mut writer, &ClientMessage::DetachSession).await?;
                    // Don't break yet — wait for the Detached response
                } else {
                    write_data(&mut writer, &data).await?;
                }
            }
            Some((cols, rows)) = resize_rx.recv() => {
                send_client_message(
                    &mut writer,
                    &ClientMessage::ResizeSession { id, cols, rows },
                ).await?;
            }
        }
    };

    stdin_handle.abort();
    sigwinch_handle.abort();

    result
}

async fn resolve_session_id(socket_path: &Path, prefix: &str) -> Result<Uuid> {
    // Try parsing as a full UUID first
    if let Ok(id) = prefix.parse::<Uuid>() {
        return Ok(id);
    }

    // Otherwise, treat as a prefix and list sessions to find a match
    let resp = request(socket_path, &ClientMessage::ListSessions).await?;
    match resp {
        ServerMessage::Sessions { sessions } => {
            let matches: Vec<_> = sessions
                .iter()
                .filter(|s| s.id.to_string().starts_with(prefix))
                .collect();
            match matches.len() {
                0 => bail!("no session matching prefix '{}'", prefix),
                1 => Ok(matches[0].id),
                n => bail!("ambiguous prefix '{}' matches {} sessions", prefix, n),
            }
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

/// RAII guard that enters raw terminal mode and restores on drop.
struct RawModeGuard {
    original: nix::sys::termios::Termios,
}

impl RawModeGuard {
    fn enter() -> Result<Self> {
        use nix::sys::termios;
        use std::os::fd::AsFd;

        let stdin = std::io::stdin();
        let fd = stdin.as_fd();
        let original = termios::tcgetattr(fd)?;
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(fd, termios::SetArg::TCSANOW, &raw)?;

        Ok(Self { original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        use nix::sys::termios;
        use std::os::fd::AsFd;

        let stdin = std::io::stdin();
        let fd = stdin.as_fd();
        let _ = termios::tcsetattr(fd, termios::SetArg::TCSANOW, &self.original);
    }
}
