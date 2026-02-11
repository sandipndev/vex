use std::fs;
use std::process::Command;

/// Helper to run git commands in a directory
fn git(args: &[&str], cwd: &str) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Helper to run the vex binary with VEX_HOME set
fn vex(args: &[&str], cwd: &str, vex_home: &str) -> std::process::Output {
    let bin = env!("CARGO_BIN_EXE_vex");
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .env("VEX_HOME", vex_home)
        .output()
        .unwrap_or_else(|e| panic!("vex {args:?} failed to run: {e}"))
}

fn vex_ok(args: &[&str], cwd: &str, vex_home: &str) -> String {
    let output = vex(args, cwd, vex_home);
    assert!(
        output.status.success(),
        "vex {args:?} failed (exit {:?}):\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Create a temporary git repo with a single commit on "main"
fn setup_git_repo(dir: &std::path::Path) -> String {
    let repo_path = dir.join("test-repo");
    fs::create_dir_all(&repo_path).unwrap();
    let rp = repo_path.to_str().unwrap();

    git(&["init", "-b", "main"], rp);
    git(&["config", "user.email", "test@test.com"], rp);
    git(&["config", "user.name", "Test"], rp);
    git(&["config", "commit.gpgsign", "false"], rp);
    fs::write(repo_path.join("README.md"), "# Test Repo").unwrap();
    git(&["add", "."], rp);
    git(&["commit", "-m", "initial commit"], rp);

    rp.to_string()
}

/// Manually register a repo with vex by writing the YAML config
fn register_repo(vex_home: &str, repo_path: &str) {
    let repos_dir = std::path::Path::new(vex_home).join("repos");
    fs::create_dir_all(&repos_dir).unwrap();
    let config =
        format!("name: test-repo\npath: {repo_path}\ndefault_branch: main\nworkstreams: []\n");
    fs::write(repos_dir.join("test-repo.yml"), config).unwrap();
}

#[test]
fn test_new_auto_registers_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let vex_home = tmp.path().join("vex-home");
    let repos_dir = tmp.path().join("repos");
    fs::create_dir_all(&repos_dir).unwrap();

    let repo_path = setup_git_repo(&repos_dir);
    let vh = vex_home.to_str().unwrap();

    // vex new should auto-register the repo
    let output = vex(&["new", "feat-test"], &repo_path, vh);

    // Check if repo config was created (auto-init)
    let repo_config = vex_home.join("repos").join("test-repo.yml");
    assert!(
        repo_config.exists(),
        "repo config file should exist after auto-init"
    );

    let contents = fs::read_to_string(&repo_config).unwrap();
    assert!(contents.contains("name: test-repo"));
    assert!(contents.contains("default_branch: main"));

    // Check if worktree dir was created regardless of tmux status
    let worktree_dir = vex_home
        .join("worktrees")
        .join("test-repo")
        .join("feat-test");

    if output.status.success() {
        assert!(worktree_dir.exists());
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if worktree_dir.exists() {
            assert!(
                stderr.contains("tmux") || stderr.contains("hook"),
                "failure should be tmux or hook-related, got: {stderr}"
            );
        }
    }
}

#[test]
fn test_list_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let vex_home = tmp.path().join("vex-home");
    let repos_dir = tmp.path().join("repos");
    fs::create_dir_all(&repos_dir).unwrap();

    let repo_path = setup_git_repo(&repos_dir);
    let vh = vex_home.to_str().unwrap();

    register_repo(vh, &repo_path);

    let output = vex_ok(&["list"], &repo_path, vh);
    assert!(output.contains("No workstreams"));
}

#[test]
fn test_config_creates_default() {
    let tmp = tempfile::tempdir().unwrap();
    let vex_home = tmp.path().join("vex-home");
    let repos_dir = tmp.path().join("repos");
    fs::create_dir_all(&repos_dir).unwrap();

    let repo_path = setup_git_repo(&repos_dir);
    let vh = vex_home.to_str().unwrap();

    // vex new triggers config creation
    let _ = vex(&["new", "feat-cfg"], &repo_path, vh);

    let config_path = vex_home.join("config.yml");
    assert!(config_path.exists());

    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("nvim"));
    assert!(contents.contains("claude"));
    assert!(contents.contains("zsh"));
    assert!(contents.contains("direnv allow"));
}

#[test]
fn test_completions_zsh() {
    let tmp = tempfile::tempdir().unwrap();
    let vh = tmp.path().to_str().unwrap();

    let output = vex_ok(&["completions", "zsh"], "/tmp", vh);
    assert!(output.contains("compdef"));
    assert!(output.contains("vex"));
}

#[test]
fn test_completions_bash() {
    let tmp = tempfile::tempdir().unwrap();
    let vh = tmp.path().to_str().unwrap();

    let output = vex_ok(&["completions", "bash"], "/tmp", vh);
    assert!(output.contains("vex"));
}
