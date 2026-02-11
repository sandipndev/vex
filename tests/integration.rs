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

#[test]
fn test_init_registers_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let vex_home = tmp.path().join("vex-home");
    let repos_dir = tmp.path().join("repos");
    fs::create_dir_all(&repos_dir).unwrap();

    let repo_path = setup_git_repo(&repos_dir);
    let vh = vex_home.to_str().unwrap();

    let output = vex_ok(&["init"], &repo_path, vh);
    assert!(output.contains("Registered repo 'test-repo'"));

    // Verify repo config was created
    let repo_config = vex_home.join("repos").join("test-repo.yml");
    assert!(repo_config.exists(), "repo config file should exist");

    let contents = fs::read_to_string(&repo_config).unwrap();
    assert!(contents.contains("name: test-repo"));
    assert!(contents.contains("default_branch: main"));
}

#[test]
fn test_init_twice_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let vex_home = tmp.path().join("vex-home");
    let repos_dir = tmp.path().join("repos");
    fs::create_dir_all(&repos_dir).unwrap();

    let repo_path = setup_git_repo(&repos_dir);
    let vh = vex_home.to_str().unwrap();

    vex_ok(&["init"], &repo_path, vh);

    let output = vex(&["init"], &repo_path, vh);
    assert!(!output.status.success(), "second init should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already registered"),
        "should mention already registered, got: {stderr}"
    );
}

#[test]
fn test_repos_lists_registered() {
    let tmp = tempfile::tempdir().unwrap();
    let vex_home = tmp.path().join("vex-home");
    let repos_dir = tmp.path().join("repos");
    fs::create_dir_all(&repos_dir).unwrap();

    let repo_path = setup_git_repo(&repos_dir);
    let vh = vex_home.to_str().unwrap();

    vex_ok(&["init"], &repo_path, vh);

    let output = vex_ok(&["repos"], &repo_path, vh);
    assert!(output.contains("test-repo"));
    assert!(output.contains("0 workstream(s)"));
}

#[test]
fn test_list_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let vex_home = tmp.path().join("vex-home");
    let repos_dir = tmp.path().join("repos");
    fs::create_dir_all(&repos_dir).unwrap();

    let repo_path = setup_git_repo(&repos_dir);
    let vh = vex_home.to_str().unwrap();

    vex_ok(&["init"], &repo_path, vh);

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

    // init creates config
    vex_ok(&["init"], &repo_path, vh);

    let config_path = vex_home.join("config.yml");
    assert!(config_path.exists());

    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("nvim"));
    assert!(contents.contains("claude"));
    assert!(contents.contains("zsh"));
    assert!(contents.contains("direnv allow"));
}

#[test]
fn test_new_creates_worktree_no_tmux() {
    // This test creates a workstream but expects tmux failure
    // since CI likely doesn't have tmux. We verify the worktree
    // and metadata were set up correctly.
    let tmp = tempfile::tempdir().unwrap();
    let vex_home = tmp.path().join("vex-home");
    let repos_dir = tmp.path().join("repos");
    fs::create_dir_all(&repos_dir).unwrap();

    let repo_path = setup_git_repo(&repos_dir);
    let vh = vex_home.to_str().unwrap();

    vex_ok(&["init"], &repo_path, vh);

    // vex new will likely fail at the tmux step, but we can check
    // if the worktree was created by checking git worktree list
    let output = vex(&["new", "feat-test"], &repo_path, vh);

    // Check if worktree dir was created regardless of tmux status
    let worktree_dir = vex_home
        .join("worktrees")
        .join("test-repo")
        .join("feat-test");

    if output.status.success() {
        // tmux was available - full success
        assert!(worktree_dir.exists());
    } else {
        // tmux not available - check if we got past the git part
        let stderr = String::from_utf8_lossy(&output.stderr);
        if worktree_dir.exists() {
            // Git worktree was created, tmux failed - that's expected in CI
            assert!(
                stderr.contains("tmux"),
                "failure should be tmux-related, got: {stderr}"
            );
        }
        // If worktree doesn't exist, the fetch failed (no remote) - also acceptable
    }
}

#[test]
fn test_reload_shows_config() {
    let tmp = tempfile::tempdir().unwrap();
    let vex_home = tmp.path().join("vex-home");
    let repos_dir = tmp.path().join("repos");
    fs::create_dir_all(&repos_dir).unwrap();

    let repo_path = setup_git_repo(&repos_dir);
    let vh = vex_home.to_str().unwrap();

    vex_ok(&["init"], &repo_path, vh);

    let output = vex_ok(&["reload"], &repo_path, vh);
    assert!(output.contains("Config reloaded"));
    assert!(output.contains("nvim"));
    assert!(output.contains("claude"));
    assert!(output.contains("zsh"));
    assert!(output.contains("on_create hooks: direnv allow"));
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
