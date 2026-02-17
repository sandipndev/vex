use std::process::Command;

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

/// Returns the full PR view (body + checks) for a given PR number.
/// Gracefully handles partial failures.
pub fn pr_view_full(repo_path: &str, pr_number: u64) -> Result<String, VexError> {
    let num_str = pr_number.to_string();
    let mut sections = Vec::new();

    // PR view with comments
    if let Ok(output) = Command::new("gh")
        .args(["pr", "view", &num_str, "--comments"])
        .current_dir(repo_path)
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            sections.push(text);
        }
    }

    // PR checks
    if let Ok(output) = Command::new("gh")
        .args(["pr", "checks", &num_str])
        .current_dir(repo_path)
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            sections.push(format!("─── Checks ───\n\n{text}"));
        }
    }

    if sections.is_empty() {
        Ok(format!("Could not fetch PR #{pr_number} details"))
    } else {
        Ok(sections.join("\n\n"))
    }
}
