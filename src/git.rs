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

pub fn push_and_track(worktree_path: &str, branch: &str) -> Result<(), VexError> {
    run_git(&["push", "-u", "origin", branch], Some(worktree_path))?;
    Ok(())
}

pub fn worktree_remove(repo_root: &str, worktree_path: &str) -> Result<(), VexError> {
    run_git(
        &["worktree", "remove", "--force", worktree_path],
        Some(repo_root),
    )?;
    Ok(())
}

pub fn current_branch(cwd: &str) -> Result<String, VexError> {
    run_git(&["rev-parse", "--abbrev-ref", "HEAD"], Some(cwd))
}

pub fn status_short(cwd: &str) -> Result<String, VexError> {
    run_git(&["status", "--short"], Some(cwd))
}

pub fn rename_branch(repo_root: &str, old: &str, new: &str) -> Result<(), VexError> {
    run_git(&["branch", "-m", old, new], Some(repo_root))?;
    Ok(())
}

pub fn worktree_move(repo_root: &str, old_path: &str, new_path: &str) -> Result<(), VexError> {
    run_git(&["worktree", "move", old_path, new_path], Some(repo_root))?;
    Ok(())
}

pub fn list_branches(repo_root: &str) -> Result<Vec<String>, VexError> {
    let output = run_git(
        &["branch", "-a", "--format=%(refname:short)"],
        Some(repo_root),
    )?;
    Ok(output
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && *l != "origin/HEAD")
        .map(|l| l.to_string())
        .collect())
}

pub fn fetch(repo_root: &str) -> Result<(), VexError> {
    run_git(&["fetch", "--prune"], Some(repo_root))?;
    Ok(())
}

pub fn delete_branch(repo_root: &str, branch: &str) -> Result<(), VexError> {
    // Best-effort: don't fail if branch can't be deleted
    let _ = run_git(&["branch", "-D", branch], Some(repo_root));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_name_from_path() {
        assert_eq!(repo_name("/home/user/projects/myrepo").unwrap(), "myrepo");
        assert_eq!(repo_name("/tmp/test-repo").unwrap(), "test-repo");
    }

    #[test]
    fn repo_name_root_fails() {
        assert!(repo_name("/").is_err());
    }

    #[test]
    fn default_branch_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_path = tmp.path().to_str().unwrap();

        // Create a git repo with a "main" branch
        run_git(&["init", "-b", "main"], Some(repo_path)).unwrap();
        run_git(&["config", "user.email", "test@test.com"], Some(repo_path)).unwrap();
        run_git(&["config", "user.name", "Test"], Some(repo_path)).unwrap();
        run_git(&["config", "commit.gpgsign", "false"], Some(repo_path)).unwrap();
        std::fs::write(tmp.path().join("README"), "hello").unwrap();
        run_git(&["add", "."], Some(repo_path)).unwrap();
        run_git(&["commit", "-m", "init"], Some(repo_path)).unwrap();

        let branch = default_branch(repo_path).unwrap();
        assert_eq!(branch, "main");
    }
}
