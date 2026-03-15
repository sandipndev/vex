use anyhow::{Result, bail};
use tokio::io;
use tokio::net::TcpStream;
use vex_cli::proto::{ClientMessage, Frame, ServerMessage, read_frame, send_client_message};

pub async fn connect(port: u16) -> Result<TcpStream> {
    TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| {
        anyhow::anyhow!(
            "could not connect to daemon on port {}: {} (is the daemon running?)",
            port,
            e
        )
    })
}

pub async fn request(port: u16, msg: &ClientMessage) -> Result<ServerMessage> {
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
