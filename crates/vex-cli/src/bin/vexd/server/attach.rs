use vex_cli::attach_frame;

use pty_process::Pty;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Run the PTY bridge: spawn `tmux attach-session -t <target>` on a PTY and
/// shuttle bytes between the PTY and the network stream using binary attach
/// framing.
pub async fn run_pty_bridge<S>(stream: S, tmux_target: &str) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let pty = Pty::new()?;
    pty.resize(pty_process::Size::new(24, 80))?;

    let pts = pty.pts()?;
    let mut cmd = pty_process::Command::new("tmux");
    cmd.args(["attach-session", "-t", tmux_target]);
    let mut child = cmd.spawn(&pts)?;

    // Split the Pty: OwnedWritePty has resize(), OwnedReadPty has AsyncRead.
    let (mut pty_read, mut pty_write) = pty.into_split();
    let (mut net_read, mut net_write) = tokio::io::split(stream);

    let mut pty_buf = vec![0u8; 4096];

    loop {
        tokio::select! {
            // PTY → network
            result = pty_read.read(&mut pty_buf) => {
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if attach_frame::send_data(&mut net_write, &pty_buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
            // Network → PTY
            frame = attach_frame::recv(&mut net_read) => {
                match frame {
                    Ok(Some(attach_frame::Frame::Data(data))) => {
                        if pty_write.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    Ok(Some(attach_frame::Frame::Resize { cols, rows })) => {
                        let _ = pty_write.resize(pty_process::Size::new(rows, cols));
                    }
                    Ok(Some(attach_frame::Frame::Close)) | Ok(None) | Err(_) => break,
                }
            }
            // tmux exited (user detached or session killed)
            status = child.wait() => {
                tracing::debug!("tmux attach exited: {status:?}");
                break;
            }
        }
    }

    // Best-effort close frame
    let _ = attach_frame::send_close(&mut net_write).await;

    // Ensure child is cleaned up
    let _ = child.kill().await;

    Ok(())
}
