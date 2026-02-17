use std::collections::HashMap;
use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

use ansi_to_tui::IntoText as _;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::config::Config;
use crate::error::VexError;
use crate::git;
use crate::github;
use crate::repo;
use crate::tmux;
use crate::workstream;

struct WorkstreamItem {
    repo_name: String,
    branch: String,
    session: String,
    active: bool,
    pr_number: Option<u64>,
    repo_path: String,
}

impl WorkstreamItem {
    fn display(&self) -> String {
        let pr = match self.pr_number {
            Some(n) => format!(" #{n}"),
            None => String::new(),
        };
        let active_marker = if self.active { " [active]" } else { "" };
        format!("{}/{}{}{}", self.repo_name, self.branch, pr, active_marker)
    }
}

#[derive(Clone, PartialEq)]
enum PanelView {
    DefaultWindow,
    GitHubPr,
}

#[derive(PartialEq)]
enum InputMode {
    Normal,
    Interact,
    CreateNew,
    SelectBaseBranch,
    Rename,
    ConfirmDelete,
}

struct App {
    workstreams: Vec<WorkstreamItem>,
    list_state: ListState,
    input_mode: InputMode,
    input_buffer: String,
    input_cursor: usize,
    preview_content: String,
    config: Config,
    should_switch: Option<String>,
    status_message: Option<(String, Instant)>,
    should_quit: bool,
    // Branch picker state for two-step create flow
    pending_new_branch: String,
    branch_candidates: Vec<String>,
    branch_filter: String,
    branch_list_state: ListState,
    // Panel cycling and scroll
    panel_view: PanelView,
    github_content: String,
    panel_scroll: u16,
    pr_cache: HashMap<String, Option<u64>>,
    // Pane resize tracking
    preview_inner_size: (u16, u16),
    /// (session:window target, original_width, original_height)
    resized_session: Option<(String, u16, u16)>,
}

impl App {
    fn new(config: Config) -> Self {
        let mut app = App {
            workstreams: Vec::new(),
            list_state: ListState::default(),
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            input_cursor: 0,
            preview_content: String::new(),
            config,
            should_switch: None,
            status_message: None,
            should_quit: false,
            pending_new_branch: String::new(),
            branch_candidates: Vec::new(),
            branch_filter: String::new(),
            branch_list_state: ListState::default(),
            panel_view: PanelView::DefaultWindow,
            github_content: String::new(),
            panel_scroll: 0,
            pr_cache: HashMap::new(),
            preview_inner_size: (0, 0),
            resized_session: None,
        };
        app.refresh_workstreams();
        app.load_pr_cache();
        if !app.workstreams.is_empty() {
            app.list_state.select(Some(0));
            app.update_preview();
        }
        app
    }

    fn load_pr_cache(&mut self) {
        // Collect unique repo paths
        let mut repo_paths: Vec<String> = self
            .workstreams
            .iter()
            .map(|w| w.repo_path.clone())
            .collect();
        repo_paths.sort();
        repo_paths.dedup();

        // One gh call per repo
        for path in &repo_paths {
            if let Ok(prs) = github::list_prs(path) {
                for (branch, num) in prs {
                    self.pr_cache.insert(format!("{path}/{branch}"), Some(num));
                }
            }
        }

        // Assign pr_number to each workstream item
        for ws in &mut self.workstreams {
            let key = format!("{}/{}", ws.repo_path, ws.branch);
            ws.pr_number = self.pr_cache.get(&key).copied().flatten();
        }
    }

    fn refresh_workstreams(&mut self) {
        let repos = repo::list_repos().unwrap_or_default();
        let active_sessions = tmux::list_sessions().unwrap_or_default();

        let old_selected = self.selected_session();

        let mut items = Vec::new();
        for repo_meta in &repos {
            for ws in &repo_meta.workstreams {
                let session = tmux::session_name(&repo_meta.name, &ws.branch);
                let active = active_sessions.contains(&session);
                // Reuse cached PR number
                let key = format!("{}/{}", repo_meta.path, ws.branch);
                let pr_number = self.pr_cache.get(&key).copied().flatten();
                items.push(WorkstreamItem {
                    repo_name: repo_meta.name.clone(),
                    branch: ws.branch.clone(),
                    session,
                    active,
                    pr_number,
                    repo_path: repo_meta.path.clone(),
                });
            }
        }
        self.workstreams = items;

        // Try to preserve selection
        if let Some(old_session) = old_selected
            && let Some(idx) = self
                .workstreams
                .iter()
                .position(|w| w.session == old_session)
        {
            self.list_state.select(Some(idx));
            return;
        }
        // Fallback: clamp selection
        if self.workstreams.is_empty() {
            self.list_state.select(None);
        } else {
            let idx = self.list_state.selected().unwrap_or(0);
            self.list_state
                .select(Some(idx.min(self.workstreams.len() - 1)));
        }
    }

    fn selected(&self) -> Option<&WorkstreamItem> {
        self.list_state
            .selected()
            .and_then(|i| self.workstreams.get(i))
    }

    fn selected_session(&self) -> Option<String> {
        self.selected().map(|w| w.session.clone())
    }

    fn update_preview(&mut self) {
        match self.panel_view {
            PanelView::DefaultWindow => {
                // Extract needed values to avoid borrow conflicts
                let ws_info = self.selected().map(|ws| (ws.session.clone(), ws.active));
                let default_window = self.config.default_window.clone();

                if let Some((session, active)) = ws_info {
                    if active {
                        let (pw, ph) = self.preview_inner_size;
                        if pw > 0 && ph > 0 {
                            self.ensure_pane_resized(&session, &default_window, pw, ph);
                        }
                        self.preview_content = tmux::capture_pane_ansi(&session, &default_window);
                    } else {
                        self.preview_content = "(session not active)".into();
                    }
                } else {
                    self.preview_content = "(no workstreams)".into();
                }
            }
            PanelView::GitHubPr => {
                // Skip — fetched on demand when Tab toggles to it
            }
        }
    }

    fn ensure_pane_resized(&mut self, session: &str, window: &str, width: u16, height: u16) {
        let target = format!("{session}:{window}");

        if let Some((ref t, _, _)) = self.resized_session
            && *t != target
        {
            // Different session — restore the old one before resizing the new one
            self.restore_pane_size();
        }

        if self.resized_session.as_ref().is_none_or(|r| r.0 != target) {
            // Save original size before first resize of this session
            if let Some((orig_w, orig_h)) = tmux::window_size(session, window) {
                self.resized_session = Some((target, orig_w, orig_h));
            }
        }

        tmux::resize_window(session, window, width, height);
    }

    fn restore_pane_size(&mut self) {
        if let Some((target, orig_w, orig_h)) = self.resized_session.take() {
            // Parse session:window from the target
            if let Some((session, window)) = target.split_once(':') {
                tmux::resize_window(session, window, orig_w, orig_h);
            }
        }
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), Instant::now()));
    }

    fn move_up(&mut self) {
        if self.workstreams.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) if i > 0 => i - 1,
            Some(_) => self.workstreams.len() - 1,
            None => 0,
        };
        self.list_state.select(Some(i));
        self.panel_scroll = 0;
        if self.panel_view == PanelView::GitHubPr
            && let Some(ws) = self.selected()
        {
            self.github_content = format!("Press Tab to load PR for '{}'", ws.branch);
        }
        self.update_preview();
    }

    fn move_down(&mut self) {
        if self.workstreams.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) if i < self.workstreams.len() - 1 => i + 1,
            Some(_) => 0,
            None => 0,
        };
        self.list_state.select(Some(i));
        self.panel_scroll = 0;
        if self.panel_view == PanelView::GitHubPr
            && let Some(ws) = self.selected()
        {
            self.github_content = format!("Press Tab to load PR for '{}'", ws.branch);
        }
        self.update_preview();
    }

    fn scroll_up(&mut self) {
        self.panel_scroll = self.panel_scroll.saturating_sub(1);
    }

    fn scroll_down(&mut self) {
        self.panel_scroll = self.panel_scroll.saturating_add(1);
    }

    fn toggle_panel(&mut self) {
        self.panel_scroll = 0;
        match self.panel_view {
            PanelView::DefaultWindow => {
                self.panel_view = PanelView::GitHubPr;
                if let Some(ws) = self.selected() {
                    if let Some(pr_num) = ws.pr_number {
                        let repo_path = ws.repo_path.clone();
                        match github::pr_view_full(&repo_path, pr_num) {
                            Ok(content) => self.github_content = content,
                            Err(e) => self.github_content = format!("Error fetching PR: {e}"),
                        }
                    } else {
                        self.github_content = format!("No PR found for branch '{}'", ws.branch);
                    }
                } else {
                    self.github_content = "(no workstream selected)".into();
                }
            }
            PanelView::GitHubPr => {
                self.panel_view = PanelView::DefaultWindow;
                self.update_preview();
            }
        }
    }

    fn enter_interact(&mut self) {
        if self.panel_view != PanelView::DefaultWindow {
            return;
        }
        if let Some(ws) = self.selected() {
            if !ws.active {
                self.set_status("Cannot interact: session not active");
                return;
            }
        } else {
            return;
        }
        self.input_mode = InputMode::Interact;
    }

    fn handle_open(&mut self) {
        if let Some(ws) = self.selected() {
            let session = ws.session.clone();
            // If session isn't active, recreate it
            if !ws.active {
                let repo_name = ws.repo_name.clone();
                let branch = ws.branch.clone();
                if let Ok(worktree_dir) = crate::config::repo_worktree_dir(&repo_name) {
                    let worktree_path = worktree_dir.join(&branch);
                    let worktree_str = worktree_path.to_string_lossy().to_string();
                    if let Err(e) = tmux::create_session(&session, &worktree_str, &self.config) {
                        self.set_status(format!("Error recreating session: {e}"));
                        return;
                    }
                }
            }
            // Restore pane size before attaching
            self.restore_pane_size();
            self.should_switch = Some(session);
            self.should_quit = true;
        }
    }

    fn send_key_to_tmux(&self, key_str: &str) {
        if let Some(ws) = self.selected() {
            let target = format!("{}:{}", ws.session, self.config.default_window);
            let _ = Command::new("tmux")
                .args(["send-keys", "-t", &target, key_str])
                .output();
        }
    }

    fn start_create(&mut self) {
        self.input_mode = InputMode::CreateNew;
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    fn confirm_create(&mut self) {
        let branch = self.input_buffer.trim().to_string();
        if branch.is_empty() {
            self.input_mode = InputMode::Normal;
            self.set_status("Cancelled: empty branch name");
            return;
        }
        // Save branch name and transition to base branch picker
        self.pending_new_branch = branch;
        self.branch_filter.clear();

        // Determine repo path for listing branches
        let repo_path = repo::list_repos()
            .ok()
            .and_then(|repos| repos.into_iter().next())
            .map(|r| r.path)
            .or_else(|| git::repo_root().ok());

        let Some(path) = repo_path else {
            // No repo found, just create with default base
            self.do_create_workstream(&self.pending_new_branch.clone(), None);
            return;
        };

        self.branch_candidates = git::list_branches(&path).unwrap_or_default();
        self.branch_list_state
            .select(if self.branch_candidates.is_empty() {
                None
            } else {
                Some(0)
            });
        self.input_mode = InputMode::SelectBaseBranch;
    }

    fn filtered_branches(&self) -> Vec<(usize, &str)> {
        self.branch_candidates
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                self.branch_filter.is_empty()
                    || b.to_lowercase()
                        .contains(&self.branch_filter.to_lowercase())
            })
            .map(|(i, b)| (i, b.as_str()))
            .collect()
    }

    fn confirm_base_branch(&mut self) {
        let filtered = self.filtered_branches();
        let selected = self
            .branch_list_state
            .selected()
            .and_then(|i| filtered.get(i))
            .map(|(_, b)| b.to_string());

        let branch = self.pending_new_branch.clone();
        self.do_create_workstream(&branch, selected.as_deref());
    }

    fn do_create_workstream(&mut self, branch: &str, base: Option<&str>) {
        self.input_mode = InputMode::Normal;
        match workstream::create_no_attach(None, branch, base) {
            Ok(_session) => {
                self.set_status(format!("Created workstream '{branch}'"));
                self.refresh_workstreams();
                if let Some(idx) = self.workstreams.iter().position(|w| w.branch == branch) {
                    self.list_state.select(Some(idx));
                }
                self.update_preview();
            }
            Err(e) => self.set_status(format!("Error: {e}")),
        }
    }

    fn start_delete(&mut self) {
        if self.selected().is_some() {
            self.input_mode = InputMode::ConfirmDelete;
        }
    }

    fn confirm_delete(&mut self) {
        if let Some(ws) = self.selected() {
            let repo_name = ws.repo_name.clone();
            let branch = ws.branch.clone();
            self.input_mode = InputMode::Normal;
            match workstream::remove(Some(&repo_name), &branch) {
                Ok(()) => {
                    self.set_status(format!("Deleted workstream '{branch}'"));
                    self.refresh_workstreams();
                    self.update_preview();
                }
                Err(e) => self.set_status(format!("Error: {e}")),
            }
        } else {
            self.input_mode = InputMode::Normal;
        }
    }

    fn start_rename(&mut self) {
        let branch = self.selected().map(|ws| ws.branch.clone());
        if let Some(branch) = branch {
            self.input_mode = InputMode::Rename;
            self.input_buffer = branch;
            self.input_cursor = self.input_buffer.len();
        }
    }

    fn confirm_rename(&mut self) {
        let new_branch = self.input_buffer.trim().to_string();
        self.input_mode = InputMode::Normal;
        if new_branch.is_empty() {
            self.set_status("Cancelled: empty branch name");
            return;
        }
        if let Some(ws) = self.selected() {
            let repo_name = ws.repo_name.clone();
            let old_branch = ws.branch.clone();
            if new_branch == old_branch {
                self.set_status("Cancelled: same name");
                return;
            }
            match workstream::rename(Some(&repo_name), Some(&old_branch), &new_branch) {
                Ok(()) => {
                    self.set_status(format!("Renamed '{old_branch}' -> '{new_branch}'"));
                    self.refresh_workstreams();
                    self.update_preview();
                }
                Err(e) => self.set_status(format!("Error: {e}")),
            }
        }
    }

    fn handle_input_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(c) => {
                self.input_buffer.insert(self.input_cursor, c);
                self.input_cursor += 1;
            }
            KeyCode::Backspace => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                    self.input_buffer.remove(self.input_cursor);
                }
            }
            KeyCode::Left => {
                if self.input_cursor > 0 {
                    self.input_cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.input_cursor < self.input_buffer.len() {
                    self.input_cursor += 1;
                }
            }
            KeyCode::Home => self.input_cursor = 0,
            KeyCode::End => self.input_cursor = self.input_buffer.len(),
            _ => {}
        }
    }
}

fn key_to_tmux_send(code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
    if modifiers.contains(KeyModifiers::CONTROL) {
        return match code {
            KeyCode::Char(c) => Some(format!("C-{c}")),
            _ => None,
        };
    }
    match code {
        KeyCode::Char(c) => Some(c.to_string()),
        KeyCode::Enter => Some("Enter".into()),
        KeyCode::Backspace => Some("BSpace".into()),
        KeyCode::Tab => Some("Tab".into()),
        KeyCode::Up => Some("Up".into()),
        KeyCode::Down => Some("Down".into()),
        KeyCode::Left => Some("Left".into()),
        KeyCode::Right => Some("Right".into()),
        KeyCode::Delete => Some("DC".into()),
        KeyCode::Home => Some("Home".into()),
        KeyCode::End => Some("End".into()),
        KeyCode::PageUp => Some("PPage".into()),
        KeyCode::PageDown => Some("NPage".into()),
        _ => None,
    }
}

pub fn run() -> Result<(), VexError> {
    let config = Config::load_or_create()?;
    let mut app = App::new(config);

    // Setup terminal
    terminal::enable_raw_mode()
        .map_err(|e| VexError::ConfigError(format!("failed to enable raw mode: {e}")))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| VexError::ConfigError(format!("failed to enter alternate screen: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .map_err(|e| VexError::ConfigError(format!("failed to create terminal: {e}")))?;

    let mut last_refresh = Instant::now();
    let refresh_interval = Duration::from_secs(2);

    let result = run_loop(&mut terminal, &mut app, &mut last_refresh, refresh_interval);

    // Restore pane size before exiting
    app.restore_pane_size();

    // Restore terminal
    terminal::disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result?;

    // If user chose to switch, attach after terminal is restored
    if let Some(session) = app.should_switch {
        tmux::attach(&session)?;
    }

    Ok(())
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    last_refresh: &mut Instant,
    refresh_interval: Duration,
) -> Result<(), VexError> {
    loop {
        terminal
            .draw(|f| ui(f, app))
            .map_err(|e| VexError::ConfigError(format!("draw error: {e}")))?;

        if app.should_quit {
            break;
        }

        // Shorter poll timeout in Interact mode for snappy display
        let timeout = if app.input_mode == InputMode::Interact {
            Duration::from_millis(50)
        } else {
            Duration::from_millis(200)
        };

        if event::poll(timeout)
            .map_err(|e| VexError::ConfigError(format!("event poll error: {e}")))?
            && let Event::Key(key) = event::read()
                .map_err(|e| VexError::ConfigError(format!("event read error: {e}")))?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Ctrl+C always quits (except in Interact mode where it's forwarded)
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && key.code == KeyCode::Char('c')
                && app.input_mode != InputMode::Interact
            {
                break;
            }

            match app.input_mode {
                InputMode::Normal => {
                    // Check for Ctrl+O
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('o')
                    {
                        app.handle_open();
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Char('j') => app.move_down(),
                            KeyCode::Char('k') => app.move_up(),
                            KeyCode::Up => app.scroll_up(),
                            KeyCode::Down => app.scroll_down(),
                            KeyCode::Tab => app.toggle_panel(),
                            KeyCode::Enter => app.enter_interact(),
                            KeyCode::Char('n') => app.start_create(),
                            KeyCode::Char('d') => app.start_delete(),
                            KeyCode::Char('r') => app.start_rename(),
                            _ => {}
                        }
                    }
                }
                InputMode::Interact => {
                    if key.code == KeyCode::Esc {
                        app.input_mode = InputMode::Normal;
                    } else if let Some(tmux_key) = key_to_tmux_send(key.code, key.modifiers) {
                        app.send_key_to_tmux(&tmux_key);
                    }
                }
                InputMode::CreateNew => match key.code {
                    KeyCode::Enter => app.confirm_create(),
                    KeyCode::Esc => {
                        app.input_mode = InputMode::Normal;
                        app.set_status("Cancelled");
                    }
                    _ => app.handle_input_key(key.code),
                },
                InputMode::SelectBaseBranch => match key.code {
                    KeyCode::Enter => app.confirm_base_branch(),
                    KeyCode::Esc => {
                        app.input_mode = InputMode::Normal;
                        app.set_status("Cancelled");
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        let count = app.filtered_branches().len();
                        if count > 0 {
                            let i = app
                                .branch_list_state
                                .selected()
                                .map(|i| if i + 1 < count { i + 1 } else { 0 })
                                .unwrap_or(0);
                            app.branch_list_state.select(Some(i));
                        }
                    }
                    KeyCode::Up => {
                        let count = app.filtered_branches().len();
                        if count > 0 {
                            let i = app
                                .branch_list_state
                                .selected()
                                .map(|i| if i > 0 { i - 1 } else { count - 1 })
                                .unwrap_or(0);
                            app.branch_list_state.select(Some(i));
                        }
                    }
                    KeyCode::Backspace => {
                        app.branch_filter.pop();
                        app.branch_list_state
                            .select(if app.filtered_branches().is_empty() {
                                None
                            } else {
                                Some(0)
                            });
                    }
                    KeyCode::Char(c) => {
                        app.branch_filter.push(c);
                        app.branch_list_state
                            .select(if app.filtered_branches().is_empty() {
                                None
                            } else {
                                Some(0)
                            });
                    }
                    _ => {}
                },
                InputMode::Rename => match key.code {
                    KeyCode::Enter => app.confirm_rename(),
                    KeyCode::Esc => {
                        app.input_mode = InputMode::Normal;
                        app.set_status("Cancelled");
                    }
                    _ => app.handle_input_key(key.code),
                },
                InputMode::ConfirmDelete => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_delete(),
                    _ => {
                        app.input_mode = InputMode::Normal;
                        app.set_status("Cancelled");
                    }
                },
            }
        }

        // Periodic refresh (and always refresh preview in Interact mode)
        if app.input_mode == InputMode::Interact || last_refresh.elapsed() >= refresh_interval {
            if app.input_mode != InputMode::Interact {
                app.refresh_workstreams();
            }
            app.update_preview();
            *last_refresh = Instant::now();
        }

        // Clear stale status messages (after 3 seconds)
        if let Some((_, created)) = &app.status_message
            && created.elapsed() > Duration::from_secs(3)
        {
            app.status_message = None;
        }
    }
    Ok(())
}

fn ui(f: &mut ratatui::Frame, app: &mut App) {
    let size = f.area();

    // Main layout: header + content area + status + help bar
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(3),    // content
            Constraint::Length(1), // status
            Constraint::Length(1), // help
        ])
        .split(size);

    draw_header(f, outer[0]);

    // Content: left list + right preview
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(outer[1]);

    draw_list(f, app, content[0]);
    if app.input_mode == InputMode::SelectBaseBranch {
        draw_branch_picker(f, app, content[1]);
    } else {
        let preview_area = content[1];
        draw_preview(f, app, preview_area);
    }
    draw_status(f, app, outer[2]);
    draw_help(f, app, outer[3]);
}

fn draw_header(f: &mut ratatui::Frame, area: Rect) {
    let version = env!("CARGO_PKG_VERSION");
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " vex ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("v{version}"), Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(header, area);
}

fn draw_list(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .workstreams
        .iter()
        .map(|ws| {
            let style = if ws.active {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            ListItem::new(Line::from(Span::styled(ws.display(), style)))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Workstreams ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::White),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_preview(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    // Store inner area dimensions for pane resizing
    let inner_w = area.width.saturating_sub(2);
    let inner_h = area.height.saturating_sub(2);
    app.preview_inner_size = (inner_w, inner_h);

    match app.panel_view {
        PanelView::DefaultWindow => {
            let title = match app.selected() {
                Some(ws) => format!(" {} - {} ", app.config.default_window, ws.session),
                None => " Preview ".into(),
            };
            let border_color = if app.input_mode == InputMode::Interact {
                Color::Green
            } else {
                Color::Blue
            };

            // Parse ANSI escape codes into styled Text
            let text = app
                .preview_content
                .as_bytes()
                .into_text()
                .unwrap_or_else(|_| Text::from(app.preview_content.as_str()));

            let preview = Paragraph::new(text)
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(border_color)),
                )
                .scroll((app.panel_scroll, 0));

            f.render_widget(preview, area);
        }
        PanelView::GitHubPr => {
            let title = match app.selected() {
                Some(ws) => {
                    if let Some(pr_num) = ws.pr_number {
                        format!(" PR #{} - {} ", pr_num, ws.session)
                    } else {
                        format!(" GitHub - {} ", ws.session)
                    }
                }
                None => " GitHub ".into(),
            };

            let preview = Paragraph::new(Text::from(app.github_content.as_str()))
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Blue)),
                )
                .wrap(Wrap { trim: false })
                .scroll((app.panel_scroll, 0));

            f.render_widget(preview, area);
        }
    }
}

fn draw_branch_picker(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let filtered: Vec<ListItem> = app
        .filtered_branches()
        .iter()
        .map(|(_, b)| {
            ListItem::new(Line::from(Span::styled(
                b.to_string(),
                Style::default().fg(Color::White),
            )))
        })
        .collect();

    let title = format!(" Select base branch for '{}' ", app.pending_new_branch);
    let list = List::new(filtered)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.branch_list_state);
}

fn draw_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let content = match &app.input_mode {
        InputMode::CreateNew => {
            let label = Span::styled("New branch: ", Style::default().fg(Color::Yellow));
            let input = Span::raw(&app.input_buffer);
            let cursor = Span::styled("_", Style::default().fg(Color::Yellow));
            Line::from(vec![label, input, cursor])
        }
        InputMode::SelectBaseBranch => {
            let label = Span::styled("Base branch: ", Style::default().fg(Color::Yellow));
            let input = Span::raw(&app.branch_filter);
            let cursor = Span::styled("_", Style::default().fg(Color::Yellow));
            Line::from(vec![label, input, cursor])
        }
        InputMode::Rename => {
            let label = Span::styled("Rename to: ", Style::default().fg(Color::Yellow));
            let input = Span::raw(&app.input_buffer);
            let cursor = Span::styled("_", Style::default().fg(Color::Yellow));
            Line::from(vec![label, input, cursor])
        }
        InputMode::ConfirmDelete => {
            let branch = app.selected().map(|w| w.branch.as_str()).unwrap_or("?");
            Line::from(Span::styled(
                format!("Delete '{branch}'? (y/N)"),
                Style::default().fg(Color::Red),
            ))
        }
        InputMode::Normal | InputMode::Interact => {
            if let Some((msg, _)) = &app.status_message {
                Line::from(Span::styled(
                    msg.as_str(),
                    Style::default().fg(Color::Yellow),
                ))
            } else {
                Line::from("")
            }
        }
    };

    let status = Paragraph::new(content);
    f.render_widget(status, area);
}

fn draw_help(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let help_text = match app.input_mode {
        InputMode::Normal => {
            "[Enter] Interact  [Tab] Toggle PR  [^O] Open  [j/k] Navigate  [n] New  [d] Del  [r] Rename  [q] Quit"
        }
        InputMode::Interact => "[Esc] Back  \u{2500}  Keystrokes forwarded to pane",
        InputMode::CreateNew | InputMode::Rename => "[Enter] Confirm  [Esc] Cancel",
        InputMode::SelectBaseBranch => {
            "[Enter] Select  [Up/Down] Navigate  [type] Filter  [Esc] Cancel"
        }
        InputMode::ConfirmDelete => "[y] Confirm  [any] Cancel",
    };

    let help = Paragraph::new(Line::from(Span::styled(
        help_text,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(help, area);
}
