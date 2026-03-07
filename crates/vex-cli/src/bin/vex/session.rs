use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::{Result, bail};
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::{TcpStream, UnixStream};
use uuid::Uuid;
use vex_cli::proto::{
    ClientMessage, Frame, ServerMessage, read_frame, send_client_message, write_data,
};

#[derive(Clone)]
pub enum Target {
    Unix(PathBuf),
    Tcp(SocketAddr),
}

enum VexStream {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl AsyncRead for VexStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            VexStream::Unix(s) => Pin::new(s).poll_read(cx, buf),
            VexStream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for VexStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            VexStream::Unix(s) => Pin::new(s).poll_write(cx, buf),
            VexStream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            VexStream::Unix(s) => Pin::new(s).poll_flush(cx),
            VexStream::Tcp(s) => Pin::new(s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            VexStream::Unix(s) => Pin::new(s).poll_shutdown(cx),
            VexStream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}

async fn connect(target: &Target) -> Result<VexStream> {
    match target {
        Target::Unix(path) => Ok(VexStream::Unix(UnixStream::connect(path).await?)),
        Target::Tcp(addr) => Ok(VexStream::Tcp(TcpStream::connect(addr).await?)),
    }
}

async fn authenticate<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    token: &str,
) -> Result<()> {
    send_client_message(
        writer,
        &ClientMessage::Authenticate {
            token: token.to_string(),
        },
    )
    .await?;
    match read_frame(reader).await? {
        Some(Frame::Control(data)) => {
            let resp: ServerMessage = serde_json::from_slice(&data)?;
            match resp {
                ServerMessage::Authenticated => Ok(()),
                ServerMessage::Error { message } => bail!("{}", message),
                other => bail!("unexpected response: {:?}", other),
            }
        }
        Some(Frame::Data(_)) => bail!("unexpected data frame"),
        None => bail!("server closed connection"),
    }
}

/// Send a control message and read back a single control response.
async fn request(target: &Target, token: &str, msg: &ClientMessage) -> Result<ServerMessage> {
    let stream = connect(target).await?;
    let (mut reader, mut writer) = io::split(stream);

    authenticate(&mut reader, &mut writer, token).await?;
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

pub async fn session_create(target: &Target, token: &str, shell: Option<String>) -> Result<String> {
    let resp = request(target, token, &ClientMessage::CreateSession { shell }).await?;
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

pub async fn session_list(target: &Target, token: &str) -> Result<()> {
    let resp = request(target, token, &ClientMessage::ListSessions).await?;
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

pub async fn session_kill(target: &Target, token: &str, id_prefix: &str) -> Result<()> {
    let id = resolve_session_id(target, token, id_prefix).await?;
    let resp = request(target, token, &ClientMessage::KillSession { id }).await?;
    match resp {
        ServerMessage::Error { message } => bail!("{}", message),
        _ => {
            println!("killed session {}", id);
            Ok(())
        }
    }
}

pub async fn session_attach(target: &Target, token: &str, id_prefix: &str) -> Result<()> {
    let id = resolve_session_id(target, token, id_prefix).await?;

    let stream = connect(target).await?;
    let (mut reader, mut writer) = io::split(stream);

    // Authenticate first
    authenticate(&mut reader, &mut writer, token).await?;

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

async fn resolve_session_id(target: &Target, token: &str, prefix: &str) -> Result<Uuid> {
    // Try parsing as a full UUID first
    if let Ok(id) = prefix.parse::<Uuid>() {
        return Ok(id);
    }

    // Otherwise, treat as a prefix and list sessions to find a match
    let resp = request(target, token, &ClientMessage::ListSessions).await?;
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
