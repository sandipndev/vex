pub mod app;
mod ui;

pub use app::App;

use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::DefaultTerminal;

use vex_cli::{Command, Response};

use crate::connect::Connection;

// ── TUI entry ─────────────────────────────────────────────────────────────────

pub async fn run(mut conn: Connection, conn_label: String) -> Result<()> {
    let is_local = conn.is_unix();
    let mut app = App::new(is_local, conn_label);

    // Initial data load
    refresh_repos(&mut conn, &mut app).await;

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &mut conn).await;
    ratatui::restore();
    result
}

// ── Event loop ────────────────────────────────────────────────────────────────

async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    conn: &mut Connection,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui::render(f, app))?;

        // Auto-refresh every 2 seconds
        if app.last_refresh.elapsed() > Duration::from_secs(2) {
            refresh_repos(conn, app).await;
        }

        // Poll for events with a short timeout so we can still auto-refresh
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if handle_key(terminal, app, conn, key.code).await? {
                break; // quit
            }
        }
    }
    Ok(())
}

// ── Key handling ──────────────────────────────────────────────────────────────

/// Returns `true` if the TUI should quit.
async fn handle_key(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    conn: &mut Connection,
    key: KeyCode,
) -> Result<bool> {
    use app::Mode;

    match &app.mode.clone() {
        Mode::Normal => match key {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Up => app.move_up(),
            KeyCode::Down => app.move_down(),
            KeyCode::Char('r') => {
                refresh_repos(conn, app).await;
            }
            KeyCode::Enter => {
                // Attach to full tmux session
                if let Some(session) = app.selected_tmux_session() {
                    if app.is_local {
                        attach_tmux(terminal, &session, None).await?;
                        refresh_repos(conn, app).await;
                    } else {
                        app.status_msg = Some(
                            "Remote connection — tmux attach not available from here".to_string(),
                        );
                    }
                }
            }
            KeyCode::Char('s') => {
                // Attach to window 0 (shell)
                if let Some(session) = app.selected_tmux_session() {
                    if app.is_local {
                        attach_tmux(terminal, &session, Some(0)).await?;
                        refresh_repos(conn, app).await;
                    } else {
                        app.status_msg = Some(
                            "Remote connection — tmux attach not available from here".to_string(),
                        );
                    }
                }
            }
            KeyCode::Char('S') => {
                // Spawn a new shell in the selected workstream
                if let Some(ws_id) = app.selected_ws_id() {
                    match spawn_shell(conn, ws_id).await {
                        Ok(shell) => {
                            app.status_msg = Some(format!("Spawned shell {}", shell.id));
                            refresh_repos(conn, app).await;
                        }
                        Err(e) => {
                            app.status_msg = Some(format!("Error: {e}"));
                        }
                    }
                }
            }
            KeyCode::Char('a') => {
                if app.selected().is_some() {
                    app.mode = Mode::SpawnInput;
                    app.spawn_input.clear();
                    app.status_msg = None;
                }
            }
            KeyCode::Char('c') => {
                app.status_msg = None;
                if app.repos.is_empty() {
                    app.status_msg =
                        Some("No repos registered. Run 'vexd repo register <path>'.".to_string());
                } else if app.repos.len() == 1 {
                    app.create_input.clear();
                    app.mode = Mode::CreateNameInput {
                        repo_id: app.repos[0].id.clone(),
                        repo_name: app.repos[0].name.clone(),
                        default_branch: app.repos[0].default_branch.clone(),
                    };
                } else {
                    app.mode = Mode::CreateSelectRepo { selected: 0 };
                }
            }
            KeyCode::Char('d') => {
                if app.selected().is_some() {
                    app.mode = Mode::ConfirmDelete;
                }
            }
            _ => {}
        },

        Mode::SpawnInput => match key {
            KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.spawn_input.clear();
            }
            KeyCode::Enter => {
                let prompt = app.spawn_input.trim().to_string();
                if prompt.is_empty() {
                    app.mode = Mode::Normal;
                    return Ok(false);
                }
                let ws_id = app.selected_ws_id().unwrap_or_default();
                app.mode = Mode::Normal;
                app.spawn_input.clear();

                match spawn_agent(conn, ws_id.clone(), prompt).await {
                    Ok(agent) => {
                        app.status_msg = Some(format!("Spawned {}", agent.id));
                        refresh_repos(conn, app).await;
                        app.mode = Mode::ConfirmAttach {
                            ws_id,
                            window_index: agent.tmux_window,
                        };
                    }
                    Err(e) => {
                        app.status_msg = Some(format!("Error: {e}"));
                    }
                }
            }
            KeyCode::Backspace => {
                app.spawn_input.pop();
            }
            KeyCode::Char(c) => {
                app.spawn_input.push(c);
            }
            _ => {}
        },

        Mode::ConfirmAttach {
            ws_id,
            window_index,
        } => {
            let ws_id = ws_id.clone();
            let window_index = *window_index;
            match key {
                KeyCode::Char('y') | KeyCode::Enter => {
                    app.mode = Mode::Normal;
                    // Find tmux session for this workstream
                    let session = app
                        .repos
                        .iter()
                        .flat_map(|r| &r.workstreams)
                        .find(|ws| ws.id == ws_id)
                        .map(|ws| ws.tmux_session.clone());

                    if let Some(session) = session
                        && app.is_local
                    {
                        attach_tmux(terminal, &session, Some(window_index)).await?;
                        refresh_repos(conn, app).await;
                    }
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    app.mode = Mode::Normal;
                }
                _ => {}
            }
        }

        Mode::ConfirmDelete => match key {
            KeyCode::Char('y') => {
                app.mode = Mode::Normal;
                if let Some(ws_id) = app.selected_ws_id() {
                    match delete_workstream(conn, ws_id).await {
                        Ok(()) => {
                            app.status_msg = Some("Workstream deleted.".to_string());
                            refresh_repos(conn, app).await;
                        }
                        Err(e) => {
                            app.status_msg = Some(format!("Error: {e}"));
                        }
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                app.mode = Mode::Normal;
            }
            _ => {}
        },

        Mode::CreateSelectRepo { selected } => {
            let n_repos = app.repos.len();
            let sel = *selected;
            match key {
                KeyCode::Esc => app.mode = Mode::Normal,
                KeyCode::Up => {
                    if let Mode::CreateSelectRepo { selected } = &mut app.mode
                        && *selected > 0
                    {
                        *selected -= 1;
                    }
                }
                KeyCode::Down => {
                    if let Mode::CreateSelectRepo { selected } = &mut app.mode
                        && *selected + 1 < n_repos
                    {
                        *selected += 1;
                    }
                }
                KeyCode::Enter => {
                    let repo = app.repos[sel].clone();
                    app.create_input.clear();
                    app.mode = Mode::CreateNameInput {
                        repo_id: repo.id,
                        repo_name: repo.name.clone(),
                        default_branch: repo.default_branch,
                    };
                }
                _ => {}
            }
        }

        Mode::CreateNameInput {
            repo_id,
            repo_name,
            default_branch,
        } => {
            let repo_id = repo_id.clone();
            let repo_name = repo_name.clone();
            let default_branch = default_branch.clone();
            match key {
                KeyCode::Esc => {
                    app.mode = Mode::Normal;
                    app.create_input.clear();
                }
                KeyCode::Enter => {
                    let name = app.create_input.trim().to_string();
                    if name.is_empty() {
                        return Ok(false);
                    }
                    app.create_input.clear();
                    app.mode = Mode::CreateBranchInput {
                        repo_id,
                        repo_name,
                        name,
                        default_branch,
                    };
                }
                KeyCode::Backspace => {
                    app.create_input.pop();
                }
                KeyCode::Char(c) => {
                    app.create_input.push(c);
                }
                _ => {}
            }
        }

        Mode::CreateBranchInput {
            repo_id,
            repo_name,
            name,
            default_branch,
        } => {
            let repo_id = repo_id.clone();
            let repo_name = repo_name.clone();
            let name = name.clone();
            let default_branch = default_branch.clone();
            match key {
                KeyCode::Esc => {
                    // Go back to name step, pre-populate create_input with the name
                    app.create_input = name.clone();
                    app.mode = Mode::CreateNameInput {
                        repo_id,
                        repo_name,
                        default_branch,
                    };
                }
                KeyCode::Enter => {
                    let branch_raw = app.create_input.trim().to_string();
                    let branch = if branch_raw.is_empty() {
                        None
                    } else {
                        Some(branch_raw)
                    };
                    app.create_input.clear();
                    app.mode = Mode::CreateConfirmFetch {
                        repo_id,
                        name,
                        branch,
                    };
                }
                KeyCode::Backspace => {
                    app.create_input.pop();
                }
                KeyCode::Char(c) => {
                    app.create_input.push(c);
                }
                _ => {}
            }
        }

        Mode::CreateConfirmFetch {
            repo_id,
            name,
            branch,
        } => {
            let repo_id = repo_id.clone();
            let name = name.clone();
            let branch = branch.clone();
            match key {
                KeyCode::Char('y') => {
                    app.mode = Mode::Normal;
                    match create_workstream(conn, repo_id, Some(name), branch, true).await {
                        Ok(ws) => {
                            app.status_msg =
                                Some(format!("Created workstream '{}' (fetched)", ws.name));
                            refresh_repos(conn, app).await;
                        }
                        Err(e) => {
                            app.status_msg = Some(format!("Error: {e}"));
                        }
                    }
                }
                KeyCode::Char('n') | KeyCode::Enter => {
                    app.mode = Mode::Normal;
                    match create_workstream(conn, repo_id, Some(name), branch, false).await {
                        Ok(ws) => {
                            app.status_msg = Some(format!("Created workstream '{}'", ws.name));
                            refresh_repos(conn, app).await;
                        }
                        Err(e) => {
                            app.status_msg = Some(format!("Error: {e}"));
                        }
                    }
                }
                KeyCode::Esc => {
                    // Go back to branch step; restore create_input to whatever branch was
                    app.create_input = branch.unwrap_or_default();
                    // We lost repo_name and default_branch, get from repos list
                    let (rname, db) = app
                        .repos
                        .iter()
                        .find(|r| r.id == repo_id)
                        .map(|r| (r.name.clone(), r.default_branch.clone()))
                        .unwrap_or_default();
                    app.mode = Mode::CreateBranchInput {
                        repo_id,
                        repo_name: rname,
                        name,
                        default_branch: db,
                    };
                }
                _ => {}
            }
        }
    }

    Ok(false)
}

// ── Tmux attach ───────────────────────────────────────────────────────────────

async fn attach_tmux(
    terminal: &mut DefaultTerminal,
    session: &str,
    window: Option<u32>,
) -> Result<()> {
    // Suspend ratatui / restore terminal
    ratatui::restore();

    let target = match window {
        Some(w) => format!("{session}:{w}"),
        None => session.to_string(),
    };

    let _ = std::process::Command::new("tmux")
        .arg("attach-session")
        .arg("-t")
        .arg(&target)
        .status();

    // Re-initialise ratatui
    *terminal = ratatui::init();
    Ok(())
}

// ── vexd calls ────────────────────────────────────────────────────────────────

async fn refresh_repos(conn: &mut Connection, app: &mut App) {
    match fetch_repos(conn).await {
        Ok(repos) => app.update_repos(repos),
        Err(e) => {
            app.status_msg = Some(format!("Refresh failed: {e}"));
        }
    }
}

async fn fetch_repos(conn: &mut Connection) -> Result<Vec<vex_cli::Repository>> {
    conn.send(&Command::WorkstreamList { repo_id: None })
        .await?;
    let resp: Response = conn.recv().await?;
    match resp {
        Response::WorkstreamList(repos) => Ok(repos),
        Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

async fn spawn_agent(
    conn: &mut Connection,
    workstream_id: String,
    prompt: String,
) -> Result<vex_cli::Agent> {
    conn.send(&Command::AgentSpawn {
        workstream_id,
        prompt,
    })
    .await?;
    let resp: Response = conn.recv().await?;
    match resp {
        Response::AgentSpawned(agent) => Ok(agent),
        Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

async fn create_workstream(
    conn: &mut Connection,
    repo_id: String,
    name: Option<String>,
    branch: Option<String>,
    fetch_latest: bool,
) -> Result<vex_cli::Workstream> {
    conn.send(&Command::WorkstreamCreate {
        repo_id,
        name,
        branch,
        fetch_latest,
    })
    .await?;
    let resp: Response = conn.recv().await?;
    match resp {
        Response::WorkstreamCreated(ws) => Ok(ws),
        Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

async fn spawn_shell(
    conn: &mut Connection,
    workstream_id: String,
) -> Result<vex_cli::ShellSession> {
    conn.send(&Command::ShellSpawn { workstream_id }).await?;
    let resp: Response = conn.recv().await?;
    match resp {
        Response::ShellSpawned(shell) => Ok(shell),
        Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

async fn delete_workstream(conn: &mut Connection, workstream_id: String) -> Result<()> {
    conn.send(&Command::WorkstreamDelete { workstream_id })
        .await?;
    let resp: Response = conn.recv().await?;
    match resp {
        Response::WorkstreamDeleted => Ok(()),
        Response::Error(e) => anyhow::bail!("{e:?}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}
