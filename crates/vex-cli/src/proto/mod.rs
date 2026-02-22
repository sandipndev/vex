use serde::{Deserialize, Serialize};

/// Default port vexd listens on for TLS TCP connections.
pub const DEFAULT_TCP_PORT: u16 = 7422;

// ── Domain types ──────────────────────────────────────────────────────────────

fn default_branch_fallback() -> String {
    "main".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: String,
    pub name: String,
    /// Absolute path to the git repository on disk
    pub path: String,
    /// Default branch used when creating a workstream without an explicit branch.
    /// Falls back to "main" for repos persisted before this field was added.
    #[serde(default = "default_branch_fallback")]
    pub default_branch: String,
    pub registered_at: u64,
    pub workstreams: Vec<Workstream>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workstream {
    pub id: String,
    pub name: String,
    pub repo_id: String,
    pub branch: String,
    /// Absolute path: `$VEX_HOME/worktrees/<workstream_id>`
    pub worktree_path: String,
    /// Always `"vex-<workstream_id>"`
    pub tmux_session: String,
    pub status: WorkstreamStatus,
    pub agents: Vec<Agent>,
    /// Active and recent shell sessions in this workstream
    #[serde(default)]
    pub shells: Vec<ShellSession>,
    pub created_at: u64,
}

// ── Shell session ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellSession {
    pub id: String,
    pub workstream_id: String,
    /// tmux window index that hosts this shell
    pub tmux_window: u32,
    pub status: ShellStatus,
    pub started_at: u64,
    pub exited_at: Option<u64>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ShellStatus {
    /// Shell is running and accepting PTY I/O
    Active,
    /// Shell is running but no client is currently attached
    Detached,
    /// Shell process has exited
    Exited,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum WorkstreamStatus {
    Idle,
    Running,
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub workstream_id: String,
    /// Window index in the tmux session
    pub tmux_window: u32,
    pub prompt: String,
    pub status: AgentStatus,
    pub exit_code: Option<i32>,
    pub spawned_at: u64,
    pub exited_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentStatus {
    Running,
    Exited,
    Failed,
}

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Command {
    // ── Existing ──────────────────────────────────────────────────────────────
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

    // ── Repos (LocalOnly) ─────────────────────────────────────────────────────
    /// Register a git repository. Unix-socket only (LocalOnly on TCP).
    RepoRegister {
        path: String,
    },
    RepoList,
    RepoUnregister {
        repo_id: String,
    },
    /// Update the default branch stored for a repo. Unix-socket only.
    RepoSetDefaultBranch {
        repo_id: String,
        branch: String,
    },

    // ── Workstreams ───────────────────────────────────────────────────────────
    WorkstreamCreate {
        repo_id: String,
        /// Workstream name. `None` = use the resolved branch name.
        name: Option<String>,
        /// Branch to check out. `None` = use the repo's `default_branch`.
        branch: Option<String>,
        /// Explicit git start point (tag, commit, branch). When set, a new local
        /// branch named `branch` is created from this ref.
        from_ref: Option<String>,
        /// If true, fetch from origin before creating the worktree.
        fetch_latest: bool,
    },
    /// `repo_id = None` means all repos
    WorkstreamList {
        repo_id: Option<String>,
    },
    WorkstreamDelete {
        workstream_id: String,
    },

    // ── Agents ────────────────────────────────────────────────────────────────
    AgentSpawn {
        workstream_id: String,
        prompt: String,
    },
    /// Claim the caller's current tmux window as an agent (in-place conversion).
    /// The daemon registers the agent and returns the command to exec.
    AgentSpawnInPlace {
        workstream_id: String,
        /// The existing tmux window index (the caller's current pane)
        tmux_window: u32,
        /// Optional task description; `None` means run the agent interactively
        prompt: Option<String>,
    },
    AgentKill {
        agent_id: String,
    },
    AgentList {
        workstream_id: String,
    },

    // ── Shells ────────────────────────────────────────────────────────────────
    /// Spawn a new shell window in a workstream (creates a new tmux window).
    ShellSpawn {
        workstream_id: String,
    },
    /// Kill a shell session.
    ShellKill {
        shell_id: String,
    },
    /// List shell sessions for a workstream.
    ShellList {
        workstream_id: String,
    },
    /// Sent by `vex shell` to register itself with vexd after launching.
    ShellRegister {
        workstream_id: String,
        tmux_window: u32,
    },
    /// Attach to a shell session's PTY stream.
    /// After the response `ShellAttached`, the connection switches to PTY
    /// streaming mode: vexd emits `PtyOutput` frames; client sends
    /// `PtyInput` / `PtyResize` frames.
    AttachShell {
        shell_id: String,
    },
    /// Detach the current client from a shell session.
    DetachShell {
        shell_id: String,
    },
    /// Send keyboard input to a shell's PTY (base64-encoded bytes).
    PtyInput {
        shell_id: String,
        /// base64-encoded bytes to write to the PTY master
        data: String,
    },
    /// Resize a shell's PTY.
    PtyResize {
        shell_id: String,
        cols: u16,
        rows: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Response {
    // ── Existing ──────────────────────────────────────────────────────────────
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

    // ── Repos ─────────────────────────────────────────────────────────────────
    RepoRegistered(Repository),
    RepoList(Vec<Repository>),
    RepoUnregistered,
    RepoDefaultBranchSet,

    // ── Workstreams ───────────────────────────────────────────────────────────
    WorkstreamCreated(Workstream),
    /// Full tree: repos → workstreams → agents
    WorkstreamList(Vec<Repository>),
    WorkstreamDeleted,

    // ── Agents ────────────────────────────────────────────────────────────────
    AgentSpawned(Agent),
    /// Returned by `AgentSpawnInPlace`; client should `exec` the given command.
    AgentSpawnedInPlace {
        agent: Agent,
        /// Shell command string to exec (replaces the caller's current process)
        exec_cmd: String,
    },
    AgentKilled,
    AgentList(Vec<Agent>),

    // ── Shells ────────────────────────────────────────────────────────────────
    ShellSpawned(ShellSession),
    ShellKilled,
    ShellList(Vec<ShellSession>),
    /// Sent back to `vex shell` after `ShellRegister`; carries the assigned ID.
    ShellRegistered {
        shell_id: String,
    },
    /// Sent back after `AttachShell`; followed by streaming `PtyOutput` frames.
    ShellAttached,
    ShellDetached,
    /// PTY output from the shell (base64-encoded bytes).
    PtyOutput {
        shell_id: String,
        /// base64-encoded raw terminal bytes
        data: String,
    },
    /// Emitted by vexd when a shell process exits.
    ShellExited {
        shell_id: String,
        code: Option<i32>,
    },
}

// ── PTY streaming ─────────────────────────────────────────────────────────────

/// Bidirectional PTY streaming message.
///
/// Used on two channels:
/// 1. `vex shell` ↔ vexd: supervisor sends `Out`/`Exited`; vexd sends `In`/`Resize`.
/// 2. `vex attach` remote client ↔ vexd: vexd sends `Out`/`Exited`; client sends
///    `In`/`Resize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ShellMsg {
    /// PTY output bytes (base64-encoded) from shell → vexd → attached clients.
    Out { data: String },
    /// PTY input bytes (base64-encoded) from attached client → vexd → shell.
    In { data: String },
    /// Terminal resize.
    Resize { cols: u16, rows: u16 },
    /// Shell process exited.
    Exited { code: Option<i32> },
}

// ── Existing helper types ─────────────────────────────────────────────────────

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

    /// Maximum allowed frame size (16 MiB). Prevents OOM from malicious length prefixes.
    const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

    /// Read a length-prefixed JSON frame from `r`.
    pub async fn recv<R, T>(r: &mut R) -> Result<T, VexFrameError>
    where
        R: AsyncRead + Unpin,
        T: for<'de> Deserialize<'de>,
    {
        let len = r.read_u32().await?;
        if len > MAX_FRAME_SIZE {
            return Err(VexFrameError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame too large: {len} bytes (max {MAX_FRAME_SIZE})"),
            )));
        }
        let mut buf = vec![0u8; len as usize];
        r.read_exact(&mut buf).await?;
        Ok(serde_json::from_slice(&buf)?)
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, Response, ShellMsg, VexProtoError, framing};

    #[tokio::test]
    async fn framing_command_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        framing::send(&mut a, &Command::Status).await.unwrap();
        let recv: Command = framing::recv(&mut b).await.unwrap();
        assert!(matches!(recv, Command::Status));
    }

    #[tokio::test]
    async fn framing_shell_register_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let cmd = Command::ShellRegister {
            workstream_id: "ws_abc123".to_string(),
            tmux_window: 7,
        };
        framing::send(&mut a, &cmd).await.unwrap();
        let recv: Command = framing::recv(&mut b).await.unwrap();
        match recv {
            Command::ShellRegister {
                workstream_id,
                tmux_window,
            } => {
                assert_eq!(workstream_id, "ws_abc123");
                assert_eq!(tmux_window, 7);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn framing_shell_msg_out() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let msg = ShellMsg::Out {
            data: "aGVsbG8=".to_string(),
        };
        framing::send(&mut a, &msg).await.unwrap();
        let recv: ShellMsg = framing::recv(&mut b).await.unwrap();
        match recv {
            ShellMsg::Out { data } => assert_eq!(data, "aGVsbG8="),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn framing_shell_msg_resize() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        framing::send(
            &mut a,
            &ShellMsg::Resize {
                cols: 120,
                rows: 40,
            },
        )
        .await
        .unwrap();
        let recv: ShellMsg = framing::recv(&mut b).await.unwrap();
        assert!(matches!(
            recv,
            ShellMsg::Resize {
                cols: 120,
                rows: 40
            }
        ));
    }

    #[tokio::test]
    async fn framing_shell_msg_exited() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        framing::send(&mut a, &ShellMsg::Exited { code: Some(1) })
            .await
            .unwrap();
        let recv: ShellMsg = framing::recv(&mut b).await.unwrap();
        assert!(matches!(recv, ShellMsg::Exited { code: Some(1) }));
    }

    #[tokio::test]
    async fn framing_response_registered() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let resp = Response::ShellRegistered {
            shell_id: "sh_aabbcc".to_string(),
        };
        framing::send(&mut a, &resp).await.unwrap();
        let recv: Response = framing::recv(&mut b).await.unwrap();
        match recv {
            Response::ShellRegistered { shell_id } => assert_eq!(shell_id, "sh_aabbcc"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn framing_response_error() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        framing::send(&mut a, &Response::Error(VexProtoError::NotFound))
            .await
            .unwrap();
        let recv: Response = framing::recv(&mut b).await.unwrap();
        assert!(matches!(recv, Response::Error(VexProtoError::NotFound)));
    }
}
