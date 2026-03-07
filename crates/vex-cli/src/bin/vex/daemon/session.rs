use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, bail};
use chrono::Utc;
use pty_process::Size;
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;
use vex_cli::proto::SessionInfo;

const MAX_SCROLLBACK: usize = 64 * 1024;

pub struct SessionHandle {
    pub id: Uuid,
    pub cols: u16,
    pub rows: u16,
    pub created_at: chrono::DateTime<Utc>,
    pub pty_writer: Arc<Mutex<pty_process::OwnedWritePty>>,
    pub output_tx: broadcast::Sender<Vec<u8>>,
    pub scrollback: Arc<Mutex<Vec<u8>>>,
    /// Tracks attached clients and their terminal dimensions.
    pub clients: HashMap<Uuid, (u16, u16)>,
}

pub struct SessionManager {
    sessions: Arc<Mutex<HashMap<Uuid, SessionHandle>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn create_session(
        &self,
        shell: Option<String>,
        cols: u16,
        rows: u16,
    ) -> Result<Uuid> {
        let shell = shell
            .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()));

        let (pty, pts) = pty_process::open().map_err(|e| anyhow::anyhow!("{}", e))?;
        pty.resize(Size::new(rows, cols))
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let cmd = pty_process::Command::new(&shell);
        let child = cmd.spawn(pts).map_err(|e| anyhow::anyhow!("{}", e))?;

        let (read_pty, write_pty) = pty.into_split();
        let (output_tx, _) = broadcast::channel(256);
        let scrollback = Arc::new(Mutex::new(Vec::new()));

        let id = Uuid::new_v4();
        let handle = SessionHandle {
            id,
            cols,
            rows,
            created_at: Utc::now(),
            pty_writer: Arc::new(Mutex::new(write_pty)),
            output_tx: output_tx.clone(),
            scrollback: Arc::clone(&scrollback),
            clients: HashMap::new(),
        };

        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(id, handle);
        }

        // PTY reader task: append to scrollback and broadcast under the same
        // lock so that attach_session can atomically snapshot + subscribe.
        tokio::spawn(async move {
            let mut read_pty = read_pty;
            let mut buf = [0u8; 4096];
            loop {
                match read_pty.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        let mut sb = scrollback.lock().await;
                        sb.extend_from_slice(chunk);
                        if sb.len() > MAX_SCROLLBACK {
                            let drain = sb.len() - MAX_SCROLLBACK;
                            sb.drain(..drain);
                        }
                        let _ = output_tx.send(chunk.to_vec());
                    }
                    Err(_) => break,
                }
            }
        });

        // Child waiter task
        let sessions = Arc::clone(&self.sessions);
        tokio::spawn(async move {
            let mut child = child;
            let _ = child.wait().await;

            let mut sessions = sessions.lock().await;
            sessions.remove(&id);
        });

        Ok(id)
    }

    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.lock().await;
        sessions
            .values()
            .map(|h| SessionInfo {
                id: h.id,
                cols: h.cols,
                rows: h.rows,
                created_at: h.created_at,
            })
            .collect()
    }

    /// Atomically snapshot the scrollback buffer and subscribe to live output.
    /// This guarantees no gaps or duplicates between the replay and the stream.
    pub async fn attach_session(
        &self,
        id: Uuid,
    ) -> Result<(Vec<u8>, broadcast::Receiver<Vec<u8>>)> {
        let sessions = self.sessions.lock().await;
        match sessions.get(&id) {
            Some(h) => {
                let sb = h.scrollback.lock().await;
                let buffer = sb.clone();
                let rx = h.output_tx.subscribe();
                Ok((buffer, rx))
            }
            None => bail!("session not found: {}", id),
        }
    }

    /// Register a client as attached to a session and recalculate PTY size.
    pub async fn client_attach(
        &self,
        session_id: Uuid,
        client_id: Uuid,
        cols: u16,
        rows: u16,
    ) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let h = sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow::anyhow!("session not found: {}", session_id))?;
        h.clients.insert(client_id, (cols, rows));
        Self::recalculate_size(h).await
    }

    /// Unregister a client from a session and recalculate PTY size.
    pub async fn client_detach(&self, session_id: Uuid, client_id: Uuid) {
        let mut sessions = self.sessions.lock().await;
        if let Some(h) = sessions.get_mut(&session_id) {
            h.clients.remove(&client_id);
            let _ = Self::recalculate_size(h).await;
        }
    }

    /// Update a client's terminal dimensions and recalculate PTY size.
    pub async fn client_resize(
        &self,
        session_id: Uuid,
        client_id: Uuid,
        cols: u16,
        rows: u16,
    ) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        let h = sessions
            .get_mut(&session_id)
            .ok_or_else(|| anyhow::anyhow!("session not found: {}", session_id))?;
        h.clients.insert(client_id, (cols, rows));
        Self::recalculate_size(h).await
    }

    /// Set PTY size to min(cols), min(rows) across all attached clients.
    /// If no clients are attached, keep the current size.
    async fn recalculate_size(h: &mut SessionHandle) -> Result<()> {
        if h.clients.is_empty() {
            return Ok(());
        }
        let cols = h.clients.values().map(|(c, _)| *c).min().unwrap();
        let rows = h.clients.values().map(|(_, r)| *r).min().unwrap();
        if cols == h.cols && rows == h.rows {
            return Ok(());
        }
        h.cols = cols;
        h.rows = rows;
        let writer = h.pty_writer.lock().await;
        writer
            .resize(Size::new(rows, cols))
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(())
    }

    pub async fn write_input(&self, id: Uuid, data: &[u8]) -> Result<()> {
        let pty_writer = {
            let sessions = self.sessions.lock().await;
            match sessions.get(&id) {
                Some(h) => Arc::clone(&h.pty_writer),
                None => bail!("session not found: {}", id),
            }
        };
        use tokio::io::AsyncWriteExt;
        let mut writer = pty_writer.lock().await;
        writer.write_all(data).await?;
        writer.flush().await?;
        Ok(())
    }

    pub async fn kill_session(&self, id: Uuid) -> Result<()> {
        let handle = {
            let mut sessions = self.sessions.lock().await;
            sessions
                .remove(&id)
                .ok_or_else(|| anyhow::anyhow!("session not found: {}", id))?
        };

        // Shutting down the writer closes the master side of the PTY,
        // which sends SIGHUP to the child process.
        let mut writer = handle.pty_writer.lock().await;
        use tokio::io::AsyncWriteExt;
        let _ = writer.shutdown().await;
        Ok(())
    }

    pub async fn kill_all(&self) {
        let ids: Vec<Uuid> = {
            let sessions = self.sessions.lock().await;
            sessions.keys().copied().collect()
        };
        for id in ids {
            let _ = self.kill_session(id).await;
        }
    }
}
