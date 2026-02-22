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
