use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, bail};
use chrono::Utc;
use pty_process::Size;
use tokio::io::AsyncReadExt;
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;
use vex_cli::proto::SessionInfo;

pub struct SessionHandle {
    pub id: Uuid,
    pub cols: u16,
    pub rows: u16,
    pub created_at: chrono::DateTime<Utc>,
    pub pty_writer: Arc<Mutex<pty_process::OwnedWritePty>>,
    pub output_tx: broadcast::Sender<Vec<u8>>,
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

        let id = Uuid::new_v4();
        let handle = SessionHandle {
            id,
            cols,
            rows,
            created_at: Utc::now(),
            pty_writer: Arc::new(Mutex::new(write_pty)),
            output_tx: output_tx.clone(),
        };

        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(id, handle);
        }

        // PTY reader task
        let output_tx_clone = output_tx;
        tokio::spawn(async move {
            let mut read_pty = read_pty;
            let mut buf = [0u8; 4096];
            loop {
                match read_pty.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let _ = output_tx_clone.send(buf[..n].to_vec());
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

    pub async fn subscribe_output(&self, id: Uuid) -> Result<broadcast::Receiver<Vec<u8>>> {
        let sessions = self.sessions.lock().await;
        match sessions.get(&id) {
            Some(h) => Ok(h.output_tx.subscribe()),
            None => bail!("session not found: {}", id),
        }
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

    pub async fn resize_session(&self, id: Uuid, cols: u16, rows: u16) -> Result<()> {
        let pty_writer = {
            let mut sessions = self.sessions.lock().await;
            match sessions.get_mut(&id) {
                Some(h) => {
                    h.cols = cols;
                    h.rows = rows;
                    Arc::clone(&h.pty_writer)
                }
                None => bail!("session not found: {}", id),
            }
        };
        let writer = pty_writer.lock().await;
        writer
            .resize(Size::new(rows, cols))
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(())
    }

    pub async fn kill_session(&self, id: Uuid) -> Result<()> {
        let sessions = self.sessions.lock().await;
        match sessions.get(&id) {
            Some(_) => {
                // Send SIGHUP to the session's PTY master side
                // Dropping the pty writer will cause the child to get SIGHUP
                // But we need to explicitly signal — for now just drop the writer
                // The child waiter task will clean up
                drop(sessions);
                let pty_writer = {
                    let sessions = self.sessions.lock().await;
                    match sessions.get(&id) {
                        Some(h) => Arc::clone(&h.pty_writer),
                        None => return Ok(()),
                    }
                };
                // Closing the writer end will send SIGHUP to the child
                let mut writer = pty_writer.lock().await;
                use tokio::io::AsyncWriteExt;
                let _ = writer.shutdown().await;
                Ok(())
            }
            None => bail!("session not found: {}", id),
        }
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

    pub async fn session_exists(&self, id: Uuid) -> bool {
        let sessions = self.sessions.lock().await;
        sessions.contains_key(&id)
    }
}
