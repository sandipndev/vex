use anyhow::Result;
use vex_cli as vex_proto;

/// Outcome of a PTY attach session.
pub enum PtyResult {
    /// The remote shell exited with an optional exit code.
    Exited(Option<i32>),
    /// The user pressed the detach key.
    Detached,
}

/// Guard that closes a pipe write-end on drop, unblocking the stdin poll thread.
struct PipeGuard(std::os::fd::RawFd);

impl Drop for PipeGuard {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// Stream PTY data between a remote shell and the local terminal.
///
/// Expects the handshake (`AttachShell` / `ShellAttached`) to already be done.
/// Puts the terminal in raw mode for the duration and restores it on exit.
///
/// If `detach_byte` is `Some(b)`, pressing that byte in stdin will cleanly
/// detach and return `PtyResult::Detached` instead of waiting for shell exit.
pub async fn pty_attach<R, W>(
    mut net_read: R,
    mut net_write: W,
    detach_byte: Option<u8>,
) -> Result<PtyResult>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use base64::Engine;
    use tokio::io::AsyncWriteExt;

    let b64 = base64::engine::general_purpose::STANDARD;

    // Send our current terminal size so the remote PTY resizes to match
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    vex_proto::framing::send(&mut net_write, &vex_proto::ShellMsg::Resize { cols, rows }).await?;

    // Create a pipe so we can cancel the stdin-reading thread on drop.
    // The thread polls on both stdin AND the read-end of this pipe;
    // when we drop `_cancel_guard` the write-end closes, poll returns
    // POLLHUP on the pipe fd, and the thread exits.
    let mut pipe_fds = [0i32; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
        return Err(anyhow::anyhow!(
            "pipe() failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let pipe_read_fd = pipe_fds[0];
    let _cancel_guard = PipeGuard(pipe_fds[1]); // close write-end on drop → unblocks poll

    // Stdin reader thread → mpsc channel (raw bytes)
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let stdin_fd = libc::STDIN_FILENO;
        let mut buf = [0u8; 1024];

        loop {
            let mut pollfds = [
                libc::pollfd {
                    fd: stdin_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pipe_read_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let ret = unsafe { libc::poll(pollfds.as_mut_ptr(), 2, -1) };
            if ret <= 0 {
                break;
            }
            // Cancel pipe signalled
            if pollfds[1].revents != 0 {
                break;
            }
            // Stdin ready
            if pollfds[0].revents & libc::POLLIN != 0 {
                let n = unsafe { libc::read(stdin_fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                if stdin_tx.blocking_send(buf[..n as usize].to_vec()).is_err() {
                    break;
                }
            }
        }
        unsafe { libc::close(pipe_read_fd) };
    });

    // SIGWINCH → terminal resize events
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;

    // Put terminal in raw mode; best-effort restore on all exit paths
    crossterm::terminal::enable_raw_mode()?;

    let mut stdout = tokio::io::stdout();
    let result: Result<PtyResult>;

    macro_rules! cleanup_return {
        ($e:expr) => {{
            crossterm::terminal::disable_raw_mode().ok();
            return $e;
        }};
    }

    loop {
        tokio::select! {
            biased;

            // Output / exit from the remote shell
            msg_result = vex_proto::framing::recv::<_, vex_proto::ShellMsg>(&mut net_read) => {
                match msg_result {
                    Err(e) => cleanup_return!(Err(e.into())),
                    Ok(vex_proto::ShellMsg::Out { data }) => {
                        match b64.decode(&data) {
                            Ok(bytes) => {
                                if let Err(e) = stdout.write_all(&bytes).await {
                                    cleanup_return!(Err(e.into()));
                                }
                                let _ = stdout.flush().await;
                            }
                            Err(e) => cleanup_return!(Err(e.into())),
                        }
                    }
                    Ok(vex_proto::ShellMsg::Exited { code }) => {
                        result = Ok(PtyResult::Exited(code));
                        break;
                    }
                    Ok(_) => {}
                }
            }

            // Keyboard input → remote shell (with optional detach-key check)
            Some(bytes) = stdin_rx.recv() => {
                if let Some(det) = detach_byte
                    && bytes.contains(&det)
                {
                    result = Ok(PtyResult::Detached);
                    break;
                }
                let encoded = b64.encode(&bytes);
                if let Err(e) = vex_proto::framing::send(
                    &mut net_write,
                    &vex_proto::ShellMsg::In { data: encoded },
                ).await {
                    cleanup_return!(Err(e.into()));
                }
            }

            // Terminal resize (SIGWINCH)
            _ = sigwinch.recv() => {
                if let Ok((c, r)) = crossterm::terminal::size() {
                    let _ = vex_proto::framing::send(
                        &mut net_write,
                        &vex_proto::ShellMsg::Resize { cols: c, rows: r },
                    ).await;
                }
            }
        }
    }

    crossterm::terminal::disable_raw_mode().ok();
    result
}
