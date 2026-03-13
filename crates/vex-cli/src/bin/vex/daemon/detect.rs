use std::fs;

pub struct ClaudeProcess {
    pub pid: u32,
    pub cmdline: Vec<String>,
}

/// BFS from `shell_pid` through /proc/<pid>/task/<tid>/children.
/// Returns processes whose comm is "claude". Depth limit 10.
pub fn find_claude_descendants(shell_pid: u32) -> Vec<ClaudeProcess> {
    let mut result = Vec::new();
    let mut queue = vec![shell_pid];
    let mut depth = 0;

    while !queue.is_empty() && depth < 10 {
        let mut next = Vec::new();
        for pid in &queue {
            // Check if this pid itself is a claude process (skip the root shell)
            if *pid != shell_pid
                && let Some(comm) = read_comm(*pid)
                && comm == "claude"
                && let Some(cmdline) = read_cmdline(*pid)
            {
                result.push(ClaudeProcess { pid: *pid, cmdline });
            }
            // Enumerate children via /proc/<pid>/task/<tid>/children
            next.extend(get_children(*pid));
        }
        queue = next;
        depth += 1;
    }

    result
}

/// Extract `--session-id` or `--resume` value from cmdline args.
pub fn extract_claude_session_id(cmdline: &[String]) -> Option<String> {
    let mut iter = cmdline.iter();
    while let Some(arg) = iter.next() {
        if arg == "--session-id" || arg == "--resume" {
            return iter.next().cloned();
        }
        if let Some(val) = arg.strip_prefix("--session-id=") {
            return Some(val.to_string());
        }
        if let Some(val) = arg.strip_prefix("--resume=") {
            return Some(val.to_string());
        }
    }
    None
}

fn read_comm(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{}/comm", pid))
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_cmdline(pid: u32) -> Option<Vec<String>> {
    let data = fs::read(format!("/proc/{}/cmdline", pid)).ok()?;
    if data.is_empty() {
        return None;
    }
    Some(
        data.split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect(),
    )
}

fn get_children(pid: u32) -> Vec<u32> {
    let mut children = Vec::new();
    let task_dir = format!("/proc/{}/task", pid);
    let Ok(entries) = fs::read_dir(&task_dir) else {
        return children;
    };
    for entry in entries.flatten() {
        let tid = entry.file_name();
        let children_path = format!("{}/{}/children", task_dir, tid.to_string_lossy());
        if let Ok(content) = fs::read_to_string(&children_path) {
            for token in content.split_whitespace() {
                if let Ok(child_pid) = token.parse::<u32>() {
                    children.push(child_pid);
                }
            }
        }
    }
    children
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_session_id_with_resume() {
        let args = vec![
            "claude".into(),
            "-p".into(),
            "--resume".into(),
            "sess-abc".into(),
            "hello".into(),
        ];
        assert_eq!(
            extract_claude_session_id(&args),
            Some("sess-abc".to_string())
        );
    }

    #[test]
    fn extract_session_id_with_session_id_flag() {
        let args = vec!["claude".into(), "--session-id".into(), "sess-xyz".into()];
        assert_eq!(
            extract_claude_session_id(&args),
            Some("sess-xyz".to_string())
        );
    }

    #[test]
    fn extract_session_id_with_equals() {
        let args = vec!["claude".into(), "--resume=sess-123".into()];
        assert_eq!(
            extract_claude_session_id(&args),
            Some("sess-123".to_string())
        );
    }

    #[test]
    fn extract_session_id_with_session_id_equals() {
        let args = vec!["claude".into(), "--session-id=sess-456".into()];
        assert_eq!(
            extract_claude_session_id(&args),
            Some("sess-456".to_string())
        );
    }

    #[test]
    fn extract_session_id_none() {
        let args = vec!["claude".into(), "-p".into(), "hello".into()];
        assert_eq!(extract_claude_session_id(&args), None);
    }

    #[test]
    fn extract_session_id_empty() {
        let args: Vec<String> = vec![];
        assert_eq!(extract_claude_session_id(&args), None);
    }
}
