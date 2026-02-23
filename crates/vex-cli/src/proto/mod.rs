use serde::{Deserialize, Serialize};

/// Default port vexd listens on for TLS TCP connections.
pub const DEFAULT_TCP_PORT: u16 = 7422;

/// Default port vexd listens on for HTTPS (HTTP API) connections.
pub const DEFAULT_HTTP_PORT: u16 = 7423;

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Command {
    Status,
    Whoami,
    PairCreate {
        label: Option<String>,
        /// Expiry in seconds from now
        expire_secs: Option<u64>,
    },
    PairList,
    PairRevoke {
        id: String,
    },
    PairRevokeAll,
    ProjectRegister {
        name: String,
        repo: String,
        path: String,
    },
    ProjectUnregister {
        name: String,
    },
    ProjectList,
    WorkstreamCreate {
        project_name: String,
        name: String,
    },
    WorkstreamList {
        project_name: String,
    },
    WorkstreamDelete {
        project_name: String,
        name: String,
    },
    ShellCreate {
        project_name: String,
        workstream_name: String,
    },
    ShellList {
        project_name: String,
        workstream_name: String,
    },
    ShellDelete {
        project_name: String,
        workstream_name: String,
        shell_id: String,
    },
    ShellAttach {
        project_name: String,
        workstream_name: String,
        /// If omitted, attaches to the first shell in the workstream.
        shell_id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkstreamInfo {
    pub name: String,
    pub project_name: String,
    pub shell_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellInfo {
    pub id: String,
    pub project_name: String,
    pub workstream_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Response {
    Pong,
    Ok,
    DaemonStatus(DaemonStatus),
    ClientInfo(ClientInfo),
    /// Returned after PairCreate; contains the plaintext secret (one-time)
    Pair(PairPayload),
    PairedClient(PairedClient),
    PairedClients(Vec<PairedClient>),
    /// Returned by PairRevoke / PairRevokeAll, carrying the revoked count.
    Revoked(u32),
    Project(ProjectInfo),
    Projects(Vec<ProjectInfo>),
    Workstream(WorkstreamInfo),
    Workstreams(Vec<WorkstreamInfo>),
    Shell(ShellInfo),
    Shells(Vec<ShellInfo>),
    ShellAttachReady {
        tmux_target: String,
    },
    Error(VexProtoError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub uptime_secs: u64,
    pub connected_clients: u32,
    pub version: String,
}

/// Returned by PairCreate — contains the plaintext secret for the new token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairPayload {
    pub token_id: String,
    pub token_secret: String,
    /// Optional TCP host for encoding into a QR pairing string
    pub host: Option<String>,
}

impl PairPayload {
    /// Returns the pairing string in `<token_id>:<token_secret>` format.
    pub fn pairing_string(&self) -> String {
        format!("{}:{}", self.token_id, self.token_secret)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedClient {
    pub token_id: String,
    pub label: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
    pub last_seen: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub repo: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub token_id: Option<String>,
    pub is_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Unix,
    Tcp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "code", content = "message")]
pub enum VexProtoError {
    Unauthorized,
    LocalOnly,
    NotFound,
    Internal(String),
}

/// Sent by the client at the start of every TCP connection before any Command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthToken {
    pub token_id: String,
    /// Plaintext hex-encoded 32-byte secret
    pub token_secret: String,
}

// ── Framing ───────────────────────────────────────────────────────────────────

pub mod framing {
    use serde::{Deserialize, Serialize};
    use std::io;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    #[derive(Debug)]
    pub enum VexFrameError {
        Io(io::Error),
        Json(serde_json::Error),
    }

    impl std::fmt::Display for VexFrameError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                VexFrameError::Io(e) => write!(f, "IO error: {e}"),
                VexFrameError::Json(e) => write!(f, "JSON error: {e}"),
            }
        }
    }

    impl std::error::Error for VexFrameError {}

    impl From<io::Error> for VexFrameError {
        fn from(e: io::Error) -> Self {
            VexFrameError::Io(e)
        }
    }

    impl From<serde_json::Error> for VexFrameError {
        fn from(e: serde_json::Error) -> Self {
            VexFrameError::Json(e)
        }
    }

    /// Write a length-prefixed JSON frame to `w`.
    pub async fn send<W, T>(w: &mut W, msg: &T) -> Result<(), VexFrameError>
    where
        W: AsyncWrite + Unpin,
        T: Serialize,
    {
        let body = serde_json::to_vec(msg)?;
        w.write_u32(body.len() as u32).await?;
        w.write_all(&body).await?;
        Ok(())
    }

    /// Read a length-prefixed JSON frame from `r`.
    pub async fn recv<R, T>(r: &mut R) -> Result<T, VexFrameError>
    where
        R: AsyncRead + Unpin,
        T: for<'de> Deserialize<'de>,
    {
        let len = r.read_u32().await?;
        let mut buf = vec![0u8; len as usize];
        r.read_exact(&mut buf).await?;
        Ok(serde_json::from_slice(&buf)?)
    }
}

// ── Binary attach framing ────────────────────────────────────────────────────
//
// After the JSON ShellAttachReady response is sent, both sides switch to this
// binary frame format for PTY passthrough:
//   [1-byte tag][4-byte BE length][payload]
// Tags: 0x00=Data, 0x01=Resize (4 bytes: 2×u16 BE cols+rows), 0x02=Close (empty payload)

pub mod attach_frame {
    use std::io;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

    const TAG_DATA: u8 = 0x00;
    const TAG_RESIZE: u8 = 0x01;
    const TAG_CLOSE: u8 = 0x02;

    #[derive(Debug)]
    pub enum Frame {
        Data(Vec<u8>),
        Resize { cols: u16, rows: u16 },
        Close,
    }

    /// Send a data frame.
    pub async fn send_data<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> io::Result<()> {
        w.write_u8(TAG_DATA).await?;
        w.write_u32(data.len() as u32).await?;
        w.write_all(data).await?;
        w.flush().await
    }

    /// Send a resize frame.
    pub async fn send_resize<W: AsyncWrite + Unpin>(
        w: &mut W,
        cols: u16,
        rows: u16,
    ) -> io::Result<()> {
        w.write_u8(TAG_RESIZE).await?;
        w.write_u32(4).await?;
        w.write_u16(cols).await?;
        w.write_u16(rows).await?;
        w.flush().await
    }

    /// Send a close frame.
    pub async fn send_close<W: AsyncWrite + Unpin>(w: &mut W) -> io::Result<()> {
        w.write_u8(TAG_CLOSE).await?;
        w.write_u32(0).await?;
        w.flush().await
    }

    /// Receive the next frame. Returns `None` on EOF.
    pub async fn recv<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Frame>> {
        let tag = match r.read_u8().await {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        };
        let len = r.read_u32().await?;

        match tag {
            TAG_DATA => {
                let mut buf = vec![0u8; len as usize];
                r.read_exact(&mut buf).await?;
                Ok(Some(Frame::Data(buf)))
            }
            TAG_RESIZE => {
                let cols = r.read_u16().await?;
                let rows = r.read_u16().await?;
                Ok(Some(Frame::Resize { cols, rows }))
            }
            TAG_CLOSE => Ok(Some(Frame::Close)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown attach frame tag: 0x{tag:02x}"),
            )),
        }
    }
}
