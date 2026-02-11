use std::process::Command;

use crate::config::Config;
use crate::error::VexError;

fn run_tmux(args: &[&str]) -> Result<String, VexError> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .map_err(|e| VexError::TmuxError(format!("failed to run tmux: {e}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(VexError::TmuxError(stderr))
    }
}

pub fn session_name(repo: &str, branch: &str) -> String {
    format!("vex_{repo}_{branch}")
        .replace(['.', '/', ':'], "-")
}

pub fn session_exists(name: &str) -> bool {
    run_tmux(&["has-session", "-t", name]).is_ok()
}

pub fn create_session(
    name: &str,
    working_dir: &str,
    config: &Config,
) -> Result<(), VexError> {
    let hooks = &config.hooks.on_enter;
    let hook_cmds = if hooks.is_empty() {
        String::new()
    } else {
        hooks.join(" && ") + " && "
    };

    // Create session with first window
    let first_window = config.windows.first().ok_or_else(|| {
        VexError::ConfigError("No windows configured".into())
    })?;

    let first_cmd = if first_window.command.is_empty() {
        hook_cmds.clone()
    } else {
        format!("{hook_cmds}{}", first_window.command)
    };

    // Create detached session
    if first_cmd.is_empty() {
        run_tmux(&[
            "new-session",
            "-d",
            "-s",
            name,
            "-c",
            working_dir,
            "-n",
            &first_window.name,
        ])?;
    } else {
        run_tmux(&[
            "new-session",
            "-d",
            "-s",
            name,
            "-c",
            working_dir,
            "-n",
            &first_window.name,
        ])?;
        // Send the command to the first window
        run_tmux(&[
            "send-keys",
            "-t",
            &format!("{name}:{}", first_window.name),
            &first_cmd,
            "Enter",
        ])?;
    }

    // Create additional windows
    for window in config.windows.iter().skip(1) {
        run_tmux(&[
            "new-window",
            "-t",
            name,
            "-n",
            &window.name,
            "-c",
            working_dir,
        ])?;

        let cmd = if window.command.is_empty() {
            hook_cmds.clone()
        } else {
            format!("{hook_cmds}{}", window.command)
        };

        if !cmd.is_empty() {
            run_tmux(&[
                "send-keys",
                "-t",
                &format!("{name}:{}", window.name),
                &cmd,
                "Enter",
            ])?;
        }
    }

    // Select first window
    if let Some(w) = config.windows.first() {
        let _ = run_tmux(&["select-window", "-t", &format!("{name}:{}", w.name)]);
    }

    Ok(())
}

pub fn attach(name: &str) -> Result<(), VexError> {
    // If we're inside tmux, switch client; otherwise attach
    let inside_tmux = std::env::var("TMUX").is_ok();
    if inside_tmux {
        let status = Command::new("tmux")
            .args(["switch-client", "-t", name])
            .status()
            .map_err(|e| VexError::TmuxError(format!("failed to switch: {e}")))?;
        if !status.success() {
            return Err(VexError::TmuxError("failed to switch tmux client".into()));
        }
    } else {
        let status = Command::new("tmux")
            .args(["attach-session", "-t", name])
            .status()
            .map_err(|e| VexError::TmuxError(format!("failed to attach: {e}")))?;
        if !status.success() {
            return Err(VexError::TmuxError("failed to attach tmux session".into()));
        }
    }
    Ok(())
}

pub fn kill_session(name: &str) -> Result<(), VexError> {
    if session_exists(name) {
        run_tmux(&["kill-session", "-t", name])?;
    }
    Ok(())
}

pub fn list_sessions() -> Result<Vec<String>, VexError> {
    match run_tmux(&["list-sessions", "-F", "#{session_name}"]) {
        Ok(output) => Ok(output
            .lines()
            .filter(|l| l.starts_with("vex_"))
            .map(|l| l.to_string())
            .collect()),
        Err(_) => Ok(vec![]),
    }
}
