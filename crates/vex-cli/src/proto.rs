use std::path::PathBuf;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

const TAG_CONTROL: u8 = 0x01;
const TAG_DATA: u8 = 0x02;
const MAX_FRAME_SIZE: usize = 1_048_576; // 1 MiB

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ClientMessage {
    CreateSession {
        shell: Option<String>,
        repo: Option<String>,
    },
    ListSessions,
    AttachSession {
        id: Uuid,
        cols: u16,
        rows: u16,
    },
    DetachSession,
    ResizeSession {
        id: Uuid,
        cols: u16,
        rows: u16,
    },
    KillSession {
        id: Uuid,
    },
    AgentList,
    AgentNotifications,
    AgentWatch {
        session_id: Uuid,
    },
    AgentPrompt {
        session_id: Uuid,
        text: String,
    },
    AgentSpawn {
        repo: String,
        workstream: Option<String>,
    },
    WorkstreamCreate {
        repo: String,
        name: String,
    },
    WorkstreamList {
        repo: Option<String>,
    },
    WorkstreamRemove {
        repo: String,
        name: String,
    },
    RepoAdd {
        name: String,
        path: PathBuf,
    },
    RepoRemove {
        name: String,
    },
    RepoList,
    RepoIntrospectPath {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ServerMessage {
    SessionCreated {
        id: Uuid,
    },
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    Attached {
        id: Uuid,
    },
    Detached,
    SessionEnded {
        id: Uuid,
        exit_code: Option<i32>,
    },
    ClientJoined {
        session_id: Uuid,
        client_id: Uuid,
    },
    ClientLeft {
        session_id: Uuid,
        client_id: Uuid,
    },
    Error {
        message: String,
    },
    AgentListResponse {
        agents: Vec<AgentEntry>,
    },
    AgentPromptSent {
        session_id: Uuid,
    },
    AgentConversationLine {
        session_id: Uuid,
        line: String,
    },
    AgentWatchEnd {
        session_id: Uuid,
    },
    RepoAdded {
        name: String,
        path: PathBuf,
    },
    RepoRemoved {
        name: String,
    },
    Repos {
        repos: Vec<RepoEntry>,
    },
    RepoIntrospected {
        suggested_name: String,
        path: PathBuf,
        git_remote: Option<String>,
        git_branch: Option<String>,
    },
    WorkstreamCreated {
        repo: String,
        name: String,
        worktree_path: PathBuf,
    },
    WorkstreamRemoved {
        repo: String,
        name: String,
    },
    Workstreams {
        workstreams: Vec<WorkstreamInfo>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: Uuid,
    pub cols: u16,
    pub rows: u16,
    pub created_at: DateTime<Utc>,
    pub client_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEntry {
    pub vex_session_id: Uuid,
    pub claude_session_id: String,
    pub claude_pid: u32,
    pub cwd: PathBuf,
    pub detected_at: DateTime<Utc>,
    pub needs_intervention: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoEntry {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkstreamInfo {
    pub repo: String,
    pub name: String,
    pub worktree_path: PathBuf,
    pub branch: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum Frame {
    Control(Vec<u8>),
    Data(Vec<u8>),
}

pub async fn write_control<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> Result<()> {
    let len = (1 + payload.len()) as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_u8(TAG_CONTROL).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

pub async fn write_data<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> Result<()> {
    let len = (1 + payload.len()) as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_u8(TAG_DATA).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Frame>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        bail!("invalid frame: zero length");
    }
    if len > MAX_FRAME_SIZE {
        bail!("frame too large: {} bytes (max {})", len, MAX_FRAME_SIZE);
    }
    let tag = {
        let mut tag_buf = [0u8; 1];
        r.read_exact(&mut tag_buf).await?;
        tag_buf[0]
    };
    let payload_len = len - 1;
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        r.read_exact(&mut payload).await?;
    }
    match tag {
        TAG_CONTROL => Ok(Some(Frame::Control(payload))),
        TAG_DATA => Ok(Some(Frame::Data(payload))),
        other => bail!("unknown frame tag: 0x{:02x}", other),
    }
}

/// Convenience: serialize a ClientMessage and write as a control frame.
pub async fn send_client_message<W: AsyncWrite + Unpin>(
    w: &mut W,
    msg: &ClientMessage,
) -> Result<()> {
    let json = serde_json::to_vec(msg)?;
    write_control(w, &json).await
}

/// Convenience: serialize a ServerMessage and write as a control frame.
pub async fn send_server_message<W: AsyncWrite + Unpin>(
    w: &mut W,
    msg: &ServerMessage,
) -> Result<()> {
    let json = serde_json::to_vec(msg)?;
    write_control(w, &json).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_round_trip_client() {
        let msgs = vec![
            ClientMessage::CreateSession {
                shell: Some("bash".into()),
                repo: None,
            },
            ClientMessage::ListSessions,
            ClientMessage::AttachSession {
                id: Uuid::nil(),
                cols: 120,
                rows: 40,
            },
            ClientMessage::DetachSession,
            ClientMessage::ResizeSession {
                id: Uuid::nil(),
                cols: 80,
                rows: 24,
            },
            ClientMessage::KillSession { id: Uuid::nil() },
            ClientMessage::AgentList,
            ClientMessage::AgentNotifications,
            ClientMessage::AgentWatch {
                session_id: Uuid::nil(),
            },
            ClientMessage::AgentPrompt {
                session_id: Uuid::nil(),
                text: "hello".into(),
            },
            ClientMessage::AgentSpawn {
                repo: "vex".into(),
                workstream: None,
            },
            ClientMessage::AgentSpawn {
                repo: "vex".into(),
                workstream: Some("feature-x".into()),
            },
            ClientMessage::WorkstreamCreate {
                repo: "vex".into(),
                name: "feature-x".into(),
            },
            ClientMessage::WorkstreamList { repo: None },
            ClientMessage::WorkstreamList {
                repo: Some("vex".into()),
            },
            ClientMessage::WorkstreamRemove {
                repo: "vex".into(),
                name: "feature-x".into(),
            },
            ClientMessage::RepoAdd {
                name: "vex".into(),
                path: PathBuf::from("/tmp/vex"),
            },
            ClientMessage::RepoRemove { name: "vex".into() },
            ClientMessage::RepoList,
            ClientMessage::RepoIntrospectPath {
                path: PathBuf::from("/tmp"),
            },
        ];
        for msg in msgs {
            let json = serde_json::to_string(&msg).unwrap();
            let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn serde_round_trip_server() {
        let msgs = vec![
            ServerMessage::SessionCreated { id: Uuid::nil() },
            ServerMessage::Sessions {
                sessions: vec![SessionInfo {
                    id: Uuid::nil(),
                    cols: 80,
                    rows: 24,
                    created_at: Utc::now(),
                    client_count: 2,
                }],
            },
            ServerMessage::Attached { id: Uuid::nil() },
            ServerMessage::Detached,
            ServerMessage::SessionEnded {
                id: Uuid::nil(),
                exit_code: Some(0),
            },
            ServerMessage::ClientJoined {
                session_id: Uuid::nil(),
                client_id: Uuid::nil(),
            },
            ServerMessage::ClientLeft {
                session_id: Uuid::nil(),
                client_id: Uuid::nil(),
            },
            ServerMessage::Error {
                message: "fail".into(),
            },
            ServerMessage::AgentListResponse {
                agents: vec![AgentEntry {
                    vex_session_id: Uuid::nil(),
                    claude_session_id: "abc123".into(),
                    claude_pid: 1234,
                    cwd: PathBuf::from("/tmp"),
                    detected_at: Utc::now(),
                    needs_intervention: true,
                }],
            },
            ServerMessage::AgentPromptSent {
                session_id: Uuid::nil(),
            },
            ServerMessage::AgentConversationLine {
                session_id: Uuid::nil(),
                line: "test line".into(),
            },
            ServerMessage::AgentWatchEnd {
                session_id: Uuid::nil(),
            },
            ServerMessage::RepoAdded {
                name: "vex".into(),
                path: PathBuf::from("/tmp/vex"),
            },
            ServerMessage::RepoRemoved { name: "vex".into() },
            ServerMessage::Repos {
                repos: vec![RepoEntry {
                    name: "vex".into(),
                    path: PathBuf::from("/tmp/vex"),
                }],
            },
            ServerMessage::RepoIntrospected {
                suggested_name: "vex".into(),
                path: PathBuf::from("/tmp/vex"),
                git_remote: Some("git@github.com:user/vex.git".into()),
                git_branch: Some("main".into()),
            },
            ServerMessage::WorkstreamCreated {
                repo: "vex".into(),
                name: "feature-x".into(),
                worktree_path: PathBuf::from("/tmp/workstreams/vex/feature-x"),
            },
            ServerMessage::WorkstreamRemoved {
                repo: "vex".into(),
                name: "feature-x".into(),
            },
            ServerMessage::Workstreams {
                workstreams: vec![WorkstreamInfo {
                    repo: "vex".into(),
                    name: "feature-x".into(),
                    worktree_path: PathBuf::from("/tmp/workstreams/vex/feature-x"),
                    branch: "feature-x".into(),
                    created_at: Utc::now(),
                }],
            },
        ];
        for msg in msgs {
            let json = serde_json::to_string(&msg).unwrap();
            let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[tokio::test]
    async fn frame_round_trip_control() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let payload = b"hello control";
        write_control(&mut client, payload).await.unwrap();
        drop(client);
        let frame = read_frame(&mut server).await.unwrap().unwrap();
        match frame {
            Frame::Control(data) => assert_eq!(data, payload),
            Frame::Data(_) => panic!("expected control frame"),
        }
    }

    #[tokio::test]
    async fn frame_round_trip_data() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let payload = b"hello data";
        write_data(&mut client, payload).await.unwrap();
        drop(client);
        let frame = read_frame(&mut server).await.unwrap().unwrap();
        match frame {
            Frame::Data(data) => assert_eq!(data, payload),
            Frame::Control(_) => panic!("expected data frame"),
        }
    }

    #[tokio::test]
    async fn frame_eof_returns_none() {
        let (client, mut server) = tokio::io::duplex(1024);
        drop(client);
        let frame = read_frame(&mut server).await.unwrap();
        assert!(frame.is_none());
    }

    #[tokio::test]
    async fn frame_bad_tag() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        // Write a frame with tag 0xFF
        let len: u32 = 2; // 1 byte tag + 1 byte payload
        client.write_all(&len.to_be_bytes()).await.unwrap();
        client.write_u8(0xFF).await.unwrap();
        client.write_u8(0x00).await.unwrap();
        drop(client);
        let result = read_frame(&mut server).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown frame tag")
        );
    }

    #[tokio::test]
    async fn frame_too_large() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        // Write a frame header claiming 2 MiB payload
        let len: u32 = 2 * 1024 * 1024;
        client.write_all(&len.to_be_bytes()).await.unwrap();
        drop(client);
        let result = read_frame(&mut server).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("frame too large"));
    }

    #[tokio::test]
    async fn send_client_message_round_trip() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let msg = ClientMessage::CreateSession {
            shell: Some("zsh".into()),
            repo: None,
        };
        send_client_message(&mut client, &msg).await.unwrap();
        drop(client);
        let frame = read_frame(&mut server).await.unwrap().unwrap();
        match frame {
            Frame::Control(data) => {
                let decoded: ClientMessage = serde_json::from_slice(&data).unwrap();
                assert_eq!(decoded, msg);
            }
            Frame::Data(_) => panic!("expected control frame"),
        }
    }
}
