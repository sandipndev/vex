use anyhow::{Context, Result};
use std::process::Command;

/// Build a sanitized tmux session name: `vex_<project>_<workstream>`.
pub fn session_name(project: &str, workstream: &str) -> String {
    let sanitize = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!("vex_{}_{}", sanitize(project), sanitize(workstream))
}

/// Check whether a tmux session exists.
pub fn has_session(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a new tmux session (detached) running `shell_cmd`.
/// Returns the first window index (usually 0).
pub fn new_session(session: &str, shell_cmd: &str) -> Result<u32> {
    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            session,
            "-P",
            "-F",
            "#{window_index}",
            shell_cmd,
        ])
        .output()
        .context("failed to run tmux new-session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-session failed: {stderr}");
    }

    let idx_str = String::from_utf8_lossy(&output.stdout);
    idx_str
        .trim()
        .parse::<u32>()
        .context("failed to parse window index from tmux new-session")
}

/// Create a new window in an existing tmux session.
/// Returns the new window index.
pub fn new_window(session: &str, shell_cmd: &str) -> Result<u32> {
    let output = Command::new("tmux")
        .args([
            "new-window",
            "-t",
            session,
            "-P",
            "-F",
            "#{window_index}",
            shell_cmd,
        ])
        .output()
        .context("failed to run tmux new-window")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-window failed: {stderr}");
    }

    let idx_str = String::from_utf8_lossy(&output.stdout);
    idx_str
        .trim()
        .parse::<u32>()
        .context("failed to parse window index from tmux new-window")
}

/// Kill a specific window in a tmux session.
pub fn kill_window(session: &str, window_index: u32) -> Result<()> {
    let target = format!("{session}:{window_index}");
    let output = Command::new("tmux")
        .args(["kill-window", "-t", &target])
        .output()
        .context("failed to run tmux kill-window")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux kill-window failed: {stderr}");
    }
    Ok(())
}

/// Kill an entire tmux session.
pub fn kill_session(session: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .output()
        .context("failed to run tmux kill-session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux kill-session failed: {stderr}");
    }
    Ok(())
}

/// List live window indices for a tmux session.
pub fn list_windows(session: &str) -> Result<Vec<u32>> {
    let output = Command::new("tmux")
        .args(["list-windows", "-t", session, "-F", "#{window_index}"])
        .output()
        .context("failed to run tmux list-windows")?;

    if !output.status.success() {
        // Session may have been destroyed externally
        return Ok(vec![]);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let indices = stdout
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect();
    Ok(indices)
}
