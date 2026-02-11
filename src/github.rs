use std::process::Command;

use serde::Deserialize;

use crate::error::VexError;

#[derive(Debug, Deserialize)]
struct PrInfo {
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    number: u64,
    title: String,
    url: String,
}

#[derive(Debug)]
pub struct PullRequest {
    pub number: u64,
    pub branch: String,
    pub title: String,
    pub url: String,
}

pub fn get_pr(repo_root: &str, pr_number: u64) -> Result<PullRequest, VexError> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "headRefName,number,title,url",
        ])
        .current_dir(repo_root)
        .output()
        .map_err(|e| VexError::GitHubError(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(VexError::GitHubError(format!(
            "gh pr view #{pr_number} failed: {stderr}"
        )));
    }

    let info: PrInfo = serde_json::from_slice(&output.stdout)
        .map_err(|e| VexError::GitHubError(format!("failed to parse gh output: {e}")))?;

    Ok(PullRequest {
        number: info.number,
        branch: info.head_ref_name,
        title: info.title,
        url: info.url,
    })
}

/// Given a branch name and repo root, check if there's a PR for it and return the number
pub fn find_pr_for_branch(repo_root: &str, branch: &str) -> Option<PullRequest> {
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--json",
            "headRefName,number,title,url",
            "--limit",
            "1",
        ])
        .current_dir(repo_root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let infos: Vec<PrInfo> = serde_json::from_slice(&output.stdout).ok()?;
    let info = infos.into_iter().next()?;

    Some(PullRequest {
        number: info.number,
        branch: info.head_ref_name,
        title: info.title,
        url: info.url,
    })
}
