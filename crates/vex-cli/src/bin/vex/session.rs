use std::io::Write;

use anyhow::{Result, bail};
use tokio::io::{self, AsyncReadExt};
use uuid::Uuid;
use vex_cli::proto::{
    ClientMessage, Frame, ServerMessage, read_frame, send_client_message, write_data,
};

use super::client::{connect, request};

pub async fn session_create(port: u16, shell: Option<String>) -> Result<String> {
    let resp = request(port, &ClientMessage::CreateSession { shell }).await?;
    match resp {
        ServerMessage::SessionCreated { id } => {
            let id_str = id.to_string();
            println!("{}", id_str);
            Ok(id_str)
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}

pub async fn session_list(port: u16) -> Result<()> {
    let resp = request(port, &ClientMessage::ListSessions).await?;
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

pub async fn session_kill(port: u16, id_prefix: &str) -> Result<()> {
    let id = resolve_session_id(port, id_prefix).await?;
    let resp = request(port, &ClientMessage::KillSession { id }).await?;
    match resp {
        ServerMessage::Error { message } => bail!("{}", message),
        _ => {
            println!("killed session {}", id);
            Ok(())
        }
    }
}

pub async fn session_attach(port: u16, id_prefix: &str) -> Result<()> {
    let id = resolve_session_id(port, id_prefix).await?;

    let stream = connect(port).await?;
    let (mut reader, mut writer) = io::split(stream);

    // Detect terminal size for the attach request
    let (cols, rows) = terminal_size::terminal_size()
        .map(|(w, h)| (w.0, h.0))
        .unwrap_or((80, 24));

    // Send attach request with terminal dimensions
    send_client_message(
        &mut writer,
        &ClientMessage::AttachSession { id, cols, rows },
    )
    .await?;

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

    // Spawn frame reader task (read_frame is not cancel-safe in tokio::select!)
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Result<Frame>>(64);
    let frame_handle = tokio::spawn(async move {
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

    // Main loop: multiplex stdin, resize signals, and server frames
    let result: Result<()> = loop {
        tokio::select! {
            result = frame_rx.recv() => {
                match result {
                    Some(Ok(Frame::Data(data))) => {
                        let mut stdout = std::io::stdout().lock();
                        let _ = stdout.write_all(&data);
                        let _ = stdout.flush();
                    }
                    Some(Ok(Frame::Control(data))) => {
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
                    Some(Err(e)) => {
                        break Err(e);
                    }
                    None => {
                        eprintln!("\r\n[server disconnected]\r");
                        break Ok(());
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
    frame_handle.abort();

    // Restore terminal before exiting
    drop(_raw_guard);

    // tokio::io::stdin() uses a blocking thread that can't be interrupted
    // by abort(). Without this, the runtime hangs on shutdown until the
    // user presses another key.
    if let Err(e) = &result {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
    std::process::exit(0);
}

async fn resolve_session_id(port: u16, prefix: &str) -> Result<Uuid> {
    // Try parsing as a full UUID first
    if let Ok(id) = prefix.parse::<Uuid>() {
        return Ok(id);
    }

    // Otherwise, treat as a prefix and list sessions to find a match
    let resp = request(port, &ClientMessage::ListSessions).await?;
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
