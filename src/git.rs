use std::path::Path;
use std::process::Command;

use crate::error::VexError;

fn run_git(args: &[&str], cwd: Option<&str>) -> Result<String, VexError> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let output = cmd
        .output()
        .map_err(|e| VexError::GitError(format!("failed to run git: {e}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(VexError::GitError(stderr))
    }
}

pub fn repo_root() -> Result<String, VexError> {
    run_git(&["rev-parse", "--show-toplevel"], None).map_err(|_| VexError::NotAGitRepo)
}

pub fn repo_name(repo_root: &str) -> Result<String, VexError> {
    let path = Path::new(repo_root);
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| VexError::GitError(format!("cannot determine repo name from {repo_root}")))
}

pub fn default_branch(repo_root: &str) -> Result<String, VexError> {
    // Try to detect from origin HEAD
    if let Ok(branch) = run_git(
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
        Some(repo_root),
    ) {
        // Returns "origin/main" -> strip prefix
        if let Some(name) = branch.strip_prefix("origin/") {
            return Ok(name.to_string());
        }
        return Ok(branch);
    }
    // Fallback: check for common branch names
    for candidate in &["main", "master"] {
        if run_git(
            &["show-ref", "--verify", &format!("refs/heads/{candidate}")],
            Some(repo_root),
        )
        .is_ok()
        {
            return Ok((*candidate).to_string());
        }
    }
    // Last resort: current branch
    run_git(&["branch", "--show-current"], Some(repo_root))
}

pub fn fetch(repo_root: &str) -> Result<(), VexError> {
    run_git(&["fetch", "origin"], Some(repo_root))?;
    Ok(())
}

pub fn remote_branch_exists(repo_root: &str, branch: &str) -> Result<bool, VexError> {
    let result = run_git(
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
        Some(repo_root),
    )?;
    Ok(!result.is_empty())
}

pub fn worktree_add_existing(
    repo_root: &str,
    worktree_path: &str,
    branch: &str,
) -> Result<(), VexError> {
    // Create worktree tracking the remote branch
    run_git(
        &[
            "worktree",
            "add",
            "--track",
            "-b",
            branch,
            worktree_path,
            &format!("origin/{branch}"),
        ],
        Some(repo_root),
    )?;
    Ok(())
}

pub fn worktree_add_new(
    repo_root: &str,
    worktree_path: &str,
    branch: &str,
    base: &str,
) -> Result<(), VexError> {
    run_git(
        &["worktree", "add", "-b", branch, worktree_path, base],
        Some(repo_root),
    )?;
    Ok(())
}

pub fn worktree_remove(repo_root: &str, worktree_path: &str) -> Result<(), VexError> {
    run_git(
        &["worktree", "remove", "--force", worktree_path],
        Some(repo_root),
    )?;
    Ok(())
}

pub fn delete_branch(repo_root: &str, branch: &str) -> Result<(), VexError> {
    // Best-effort: don't fail if branch can't be deleted
    let _ = run_git(&["branch", "-D", branch], Some(repo_root));
    Ok(())
}

