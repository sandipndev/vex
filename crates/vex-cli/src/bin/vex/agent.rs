use anyhow::{Result, bail};
use tokio::io;
use tokio::net::TcpStream;
use vex_cli::proto::{ClientMessage, Frame, ServerMessage, read_frame, send_client_message};

async fn connect(port: u16) -> Result<TcpStream> {
    TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| {
        anyhow::anyhow!(
            "could not connect to daemon on port {}: {} (is the daemon running?)",
            port,
            e
        )
    })
}

async fn request(port: u16, msg: &ClientMessage) -> Result<ServerMessage> {
    let stream = connect(port).await?;
    let (mut reader, mut writer) = io::split(stream);

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

pub async fn agent_list(port: u16) -> Result<()> {
    let resp = request(port, &ClientMessage::ListAgents).await?;
    match resp {
        ServerMessage::AgentSessions { sessions } => {
            if sessions.is_empty() {
                println!("no claude sessions detected");
            } else {
                println!(
                    "{:<36}  {:>5}  {:>7}  {:<4}  CLAUDE SESSION",
                    "SESSION", "PID", "CLIENTS", "BELL"
                );
                for s in sessions {
                    println!(
                        "{:<36}  {:>5}  {:>7}  {:<4}  {}",
                        s.session_id,
                        s.claude.pid,
                        s.client_count,
                        if s.bell_rang { "yes" } else { "no" },
                        s.claude.claude_session_id.as_deref().unwrap_or("(none)"),
                    );
                }
            }
            Ok(())
        }
        ServerMessage::Error { message } => bail!("{}", message),
        other => bail!("unexpected response: {:?}", other),
    }
}
