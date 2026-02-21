use std::path::Path;
use vex_proto::{Command, Response};

/// Send a single command to the local Unix socket and return the response.
pub async fn send_command(socket_path: &Path, cmd: &Command) -> anyhow::Result<Response> {
    let mut stream = tokio::net::UnixStream::connect(socket_path).await.map_err(|e| {
        anyhow::anyhow!(
            "Could not connect to vexd at {} — is it running? ({})",
            socket_path.display(),
            e
        )
    })?;
    vex_proto::framing::send(&mut stream, cmd).await?;
    let response: Response = vex_proto::framing::recv(&mut stream).await?;
    Ok(response)
}
