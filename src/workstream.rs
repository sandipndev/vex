use std::fs;
use std::process::Command;

use crate::config::{Config, repo_worktree_dir};
use crate::error::VexError;
use crate::repo::{self, RepoMetadata};
use crate::tmux;
use crate::{git, println_info, println_ok};

fn detect_workstream(
    repo_name: Option<&str>,
    branch: Option<&str>,
) -> Result<(RepoMetadata, String), VexError> {
    // If both are specified, resolve directly
    if let Some(branch) = branch {
        let repo_meta = repo::resolve_repo(repo_name)?;
        if !repo_meta.has_workstream(branch) {
            return Err(VexError::WorkstreamNotFound {
                repo: repo_meta.name.clone(),
                branch: branch.into(),
            });
        }
        return Ok((repo_meta, branch.to_string()));
    }

    // Try to detect from cwd
    let cwd = std::env::current_dir()
        .map_err(|e| VexError::ConfigError(format!("cannot get cwd: {e}")))?;
    let cwd_str = cwd.to_string_lossy();

    // Check if cwd is inside a vex worktree
    let worktrees_base = crate::config::worktrees_dir()?;
    if let Some(worktrees_str) = worktrees_base.to_str()
        && let Some(suffix) = cwd_str.strip_prefix(worktrees_str)
    {
        // Path is ~/.vex/worktrees/<repo>/<branch>/...
        let relative = suffix.trim_start_matches('/');
        let parts: Vec<&str> = relative.splitn(3, '/').collect();
        if parts.len() >= 2 {
            let detected_repo = parts[0];
            let detected_branch = parts[1];
            if let Ok(repo_meta) = repo::resolve_repo(Some(detected_repo))
                && repo_meta.has_workstream(detected_branch)
            {
                return Ok((repo_meta, detected_branch.to_string()));
            }
        }
    }

    // Fall back to git current branch
    let current = git::current_branch(&cwd_str)
        .map_err(|_| VexError::ConfigError("not in a vex workstream".into()))?;

    // Search repos for a workstream with this branch
    let repos = if let Some(name) = repo_name {
        vec![repo::resolve_repo(Some(name))?]
    } else {
        repo::list_repos()?
    };

    for repo_meta in repos {
        if repo_meta.has_workstream(&current) {
            return Ok((repo_meta, current));
        }
    }

    Err(VexError::ConfigError(format!(
        "no workstream found for branch '{current}'"
    )))
}

fn run_hooks(hooks: &[String], working_dir: &str) -> Result<(), VexError> {
    for hook in hooks {
        println_info!("  $ {hook}");
        let status = Command::new("sh")
            .args(["-c", hook])
            .current_dir(working_dir)
            .status()
            .map_err(|e| VexError::ConfigError(format!("failed to run hook '{hook}': {e}")))?;
        if !status.success() {
            return Err(VexError::ConfigError(format!(
                "hook '{hook}' exited with status {}",
                status.code().unwrap_or(-1)
            )));
        }
    }
    Ok(())
}

pub fn create(repo_name: Option<&str>, branch: &str) -> Result<(), VexError> {
    let mut repo_meta = match repo::resolve_repo(repo_name) {
        Ok(meta) => meta,
        Err(VexError::RepoNotInitialized(_) | VexError::NotAGitRepo) if repo_name.is_none() => {
            println_info!("Registering repo with vex...");
            repo::init_repo()?
        }
        Err(e) => return Err(e),
    };
    let config = Config::load_or_create()?;

    if repo_meta.has_workstream(branch) {
        return Err(VexError::WorkstreamAlreadyExists {
            repo: repo_meta.name.clone(),
            branch: branch.into(),
        });
    }

    // Set up worktree directory
    let worktree_base = repo_worktree_dir(&repo_meta.name)?;
    fs::create_dir_all(&worktree_base).map_err(|e| VexError::io(&worktree_base, e))?;
    let worktree_path = worktree_base.join(branch);
    let worktree_str = worktree_path.to_string_lossy().to_string();

    // Create worktree off local default branch
    println_info!(
        "Creating new branch '{branch}' off '{}'...",
        repo_meta.default_branch
    );
    git::worktree_add_new(
        &repo_meta.path,
        &worktree_str,
        branch,
        &repo_meta.default_branch,
    )?;

    // Run on_create hooks in the worktree directory
    if !config.hooks.on_create.is_empty() {
        println_info!("Running on_create hooks...");
        run_hooks(&config.hooks.on_create, &worktree_str)?;
    }

    // Record workstream
    repo_meta.add_workstream(branch, None);
    repo_meta.save()?;

    // Create tmux session
    let session = tmux::session_name(&repo_meta.name, branch);
    println_info!("Creating tmux session '{session}'...");
    tmux::create_session(&session, &worktree_str, &config)?;

    println_ok!("Workstream '{branch}' ready for repo '{}'", repo_meta.name);

    // Attach
    tmux::attach(&session)?;

    Ok(())
}

pub fn switch(repo_name: Option<&str>, branch: &str) -> Result<(), VexError> {
    // If no repo given, try detect from cwd (handles being inside a worktree)
    let repo_meta = match repo::resolve_repo(repo_name) {
        Ok(meta) => meta,
        Err(_) if repo_name.is_none() => {
            // Try all repos to find one with this branch
            let repos = repo::list_repos()?;
            repos
                .into_iter()
                .find(|r| r.has_workstream(branch))
                .ok_or_else(|| VexError::WorkstreamNotFound {
                    repo: "(any)".into(),
                    branch: branch.into(),
                })?
        }
        Err(e) => return Err(e),
    };

    if !repo_meta.has_workstream(branch) {
        return Err(VexError::WorkstreamNotFound {
            repo: repo_meta.name.clone(),
            branch: branch.into(),
        });
    }

    let session = tmux::session_name(&repo_meta.name, branch);
    if !tmux::session_exists(&session) {
        // Session was killed externally, recreate it
        let config = Config::load_or_create()?;
        let worktree_path = repo_worktree_dir(&repo_meta.name)?.join(branch);
        let worktree_str = worktree_path.to_string_lossy().to_string();
        println_info!("Recreating tmux session '{session}'...");
        tmux::create_session(&session, &worktree_str, &config)?;
    }

    tmux::attach(&session)?;
    Ok(())
}

pub fn remove(repo_name: Option<&str>, branch: &str) -> Result<(), VexError> {
    let mut repo_meta = repo::resolve_repo(repo_name)?;

    if !repo_meta.has_workstream(branch) {
        return Err(VexError::WorkstreamNotFound {
            repo: repo_meta.name.clone(),
            branch: branch.into(),
        });
    }

    // Kill tmux session
    let session = tmux::session_name(&repo_meta.name, branch);
    tmux::kill_session(&session)?;

    // Remove worktree
    let worktree_path = repo_worktree_dir(&repo_meta.name)?.join(branch);
    let worktree_str = worktree_path.to_string_lossy().to_string();
    if worktree_path.exists() {
        git::worktree_remove(&repo_meta.path, &worktree_str)?;
    }

    // Clean up local branch
    git::delete_branch(&repo_meta.path, branch)?;

    // Update metadata
    repo_meta.remove_workstream(branch);
    repo_meta.save()?;

    println_ok!(
        "Removed workstream '{branch}' from repo '{}'",
        repo_meta.name
    );
    Ok(())
}

pub fn list(repo_name: Option<&str>) -> Result<(), VexError> {
    let repos = if let Some(name) = repo_name {
        vec![repo::resolve_repo(Some(name))?]
    } else {
        let all = repo::list_repos()?;
        if all.is_empty() {
            println_info!("No repos registered. Run `vex new <branch>` in a git repo.");
            return Ok(());
        }
        all
    };

    let active_sessions = tmux::list_sessions().unwrap_or_default();

    for repo_meta in &repos {
        println!("\n{} ({})", repo_meta.name, repo_meta.path);
        if repo_meta.workstreams.is_empty() {
            println!("  No workstreams");
        } else {
            for ws in &repo_meta.workstreams {
                let session = tmux::session_name(&repo_meta.name, &ws.branch);
                let active = if active_sessions.contains(&session) {
                    " [active]"
                } else {
                    ""
                };
                let pr = ws
                    .pr_number
                    .map(|n| format!(" (PR #{n})"))
                    .unwrap_or_default();
                println!(
                    "  {} {}{}{}",
                    ws.branch,
                    ws.created_at.format("%Y-%m-%d"),
                    pr,
                    active
                );
            }
        }
    }
    Ok(())
}

pub fn status(repo_name: Option<&str>, branch: Option<&str>) -> Result<(), VexError> {
    let (repo_meta, branch) = detect_workstream(repo_name, branch)?;

    let worktree_path = repo_worktree_dir(&repo_meta.name)?.join(&branch);
    let worktree_str = worktree_path.to_string_lossy().to_string();

    println!("Repo:     {}", repo_meta.name);
    println!("Branch:   {branch}");
    println!(
        "Worktree: {}",
        if worktree_path.exists() {
            &worktree_str
        } else {
            "(missing)"
        }
    );

    // Tmux session status
    let session = tmux::session_name(&repo_meta.name, &branch);
    let active_sessions = tmux::list_sessions().unwrap_or_default();
    let tmux_status = if active_sessions.contains(&session) {
        "active"
    } else {
        "inactive"
    };
    println!("Tmux:     {tmux_status}");

    // Git status summary
    if worktree_path.exists() {
        match git::status_short(&worktree_str) {
            Ok(output) if output.is_empty() => println!("Status:   clean"),
            Ok(output) => {
                let count = output.lines().count();
                println!("Status:   {count} changed file(s)");
            }
            Err(_) => {}
        }
    }

    Ok(())
}

pub fn exit() -> Result<(), VexError> {
    tmux::detach()
}

pub fn rth(repo_name: Option<&str>, branch: Option<&str>) -> Result<(), VexError> {
    let (repo_meta, _branch) = detect_workstream(repo_name, branch)?;
    print!("{}", repo_meta.path);
    Ok(())
}
