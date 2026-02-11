use std::fs;

use crate::config::{repo_worktree_dir, Config};
use crate::error::VexError;
use crate::github;
use crate::repo::{self, RepoMetadata};
use crate::tmux;
use crate::{git, println_info, println_ok};

/// Resolve a branch spec which may be "#123" (PR number) or a plain branch name.
/// Returns (branch_name, optional_pr_number).
fn resolve_branch_spec(
    repo_meta: &RepoMetadata,
    spec: &str,
) -> Result<(String, Option<u64>), VexError> {
    if let Some(num_str) = spec.strip_prefix('#') {
        let pr_number: u64 = num_str
            .parse()
            .map_err(|_| VexError::GitHubError(format!("Invalid PR number: {spec}")))?;
        let pr = github::get_pr(&repo_meta.path, pr_number)?;
        println_info!("PR #{}: {} ({})", pr.number, pr.title, pr.url);
        Ok((pr.branch, Some(pr.number)))
    } else {
        // Check if this branch already has a PR
        let pr = github::find_pr_for_branch(&repo_meta.path, spec);
        if let Some(ref pr) = pr {
            println_info!("Found PR #{}: {} ({})", pr.number, pr.title, pr.url);
        }
        Ok((spec.to_string(), pr.map(|p| p.number)))
    }
}

pub fn create(repo_name: Option<&str>, branch_spec: &str) -> Result<(), VexError> {
    let mut repo_meta = repo::resolve_repo(repo_name)?;
    let config = Config::load_or_create()?;

    let (branch, pr_number) = resolve_branch_spec(&repo_meta, branch_spec)?;

    if repo_meta.has_workstream(&branch) {
        return Err(VexError::WorkstreamAlreadyExists {
            repo: repo_meta.name.clone(),
            branch: branch.clone(),
        });
    }

    // Set up worktree directory
    let worktree_base = repo_worktree_dir(&repo_meta.name)?;
    fs::create_dir_all(&worktree_base)
        .map_err(|e| VexError::io(&worktree_base, e))?;
    let worktree_path = worktree_base.join(&branch);
    let worktree_str = worktree_path.to_string_lossy().to_string();

    // Fetch latest
    println_info!("Fetching origin...");
    git::fetch(&repo_meta.path)?;

    // Create worktree
    if git::remote_branch_exists(&repo_meta.path, &branch)? {
        println_info!("Branch '{branch}' exists on origin, tracking it...");
        git::worktree_add_existing(&repo_meta.path, &worktree_str, &branch)?;
    } else {
        println_info!(
            "Creating new branch '{branch}' off '{}'...",
            repo_meta.default_branch
        );
        git::worktree_add_new(
            &repo_meta.path,
            &worktree_str,
            &branch,
            &repo_meta.default_branch,
        )?;
    }

    // Record workstream
    repo_meta.add_workstream(&branch, pr_number);
    repo_meta.save()?;

    // Create tmux session
    let session = tmux::session_name(&repo_meta.name, &branch);
    println_info!("Creating tmux session '{session}'...");
    tmux::create_session(&session, &worktree_str, &config)?;

    println_ok!("Workstream '{branch}' ready for repo '{}'", repo_meta.name);

    // Attach
    tmux::attach(&session)?;

    Ok(())
}

pub fn attach(repo_name: Option<&str>, branch: &str) -> Result<(), VexError> {
    let repo_meta = repo::resolve_repo(repo_name)?;

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
            println_info!("No repos registered. Run `vex init` in a git repo.");
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
                println!("  {} {}{}{}", ws.branch, ws.created_at.format("%Y-%m-%d"), pr, active);
            }
        }
    }
    Ok(())
}
