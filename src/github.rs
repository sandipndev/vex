use std::process::Command;

use serde::Deserialize;

use crate::error::VexError;

/// Returns a list of (branch_name, pr_number) for open PRs in the repo.
/// Returns Ok(vec![]) if `gh` is unavailable or the repo isn't on GitHub.
pub fn list_prs(repo_path: &str) -> Result<Vec<(String, u64)>, VexError> {
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,headRefName",
            "--jq",
            r#".[] | "\(.headRefName)\t\(.number)""#,
        ])
        .current_dir(repo_path)
        .output();

    let output = match output {
        Ok(o) => o,
        Err(_) => return Ok(vec![]),
    };

    if !output.status.success() {
        return Ok(vec![]);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((branch, num_str)) = line.split_once('\t')
            && let Ok(num) = num_str.parse::<u64>()
        {
            results.push((branch.to_string(), num));
        }
    }
    Ok(results)
}

// --- Structured JSON-based PR data ---

#[derive(Deserialize)]
pub struct PrViewJson {
    pub title: String,
    pub number: u64,
    pub body: String,
    pub url: String,
    pub state: String,
    #[serde(default)]
    pub comments: Vec<PrCommentJson>,
    #[serde(default)]
    pub reviews: Vec<PrReviewJson>,
}

#[derive(Deserialize)]
pub struct PrCommentJson {
    pub author: PrAuthor,
    pub body: String,
}

#[derive(Deserialize)]
pub struct PrReviewJson {
    pub author: PrAuthor,
    pub body: String,
    pub state: String,
}

#[derive(Deserialize)]
pub struct PrAuthor {
    pub login: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct PrCheckJson {
    pub name: String,
    pub state: String,
    pub conclusion: String,
}

/// Fetch structured PR data via `gh pr view --json`.
pub fn pr_view_json(repo_path: &str, pr_number: u64) -> Result<PrViewJson, VexError> {
    let num_str = pr_number.to_string();
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &num_str,
            "--json",
            "title,number,body,url,state,comments,reviews",
        ])
        .current_dir(repo_path)
        .output()
        .map_err(|e| VexError::GitError(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(VexError::GitError(format!("gh pr view failed: {stderr}")));
    }

    let json = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&json)
        .map_err(|e| VexError::GitError(format!("failed to parse PR JSON: {e}")))
}

/// Fetch PR checks via `gh pr checks --json`.
pub fn pr_checks_json(repo_path: &str, pr_number: u64) -> Result<Vec<PrCheckJson>, VexError> {
    let num_str = pr_number.to_string();
    let output = Command::new("gh")
        .args(["pr", "checks", &num_str, "--json", "name,state,conclusion"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| VexError::GitError(format!("failed to run gh: {e}")))?;

    if !output.status.success() {
        // Checks may not be available — return empty
        return Ok(vec![]);
    }

    let json = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&json)
        .map_err(|e| VexError::GitError(format!("failed to parse checks JSON: {e}")))
}

/// Combined structured PR fetch: view + checks.
pub fn pr_view_structured(
    repo_path: &str,
    pr_number: u64,
) -> Result<(PrViewJson, Vec<PrCheckJson>), VexError> {
    let view = pr_view_json(repo_path, pr_number)?;
    let checks = pr_checks_json(repo_path, pr_number).unwrap_or_default();
    Ok((view, checks))
}
