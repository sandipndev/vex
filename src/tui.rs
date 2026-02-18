use std::collections::HashMap;
use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

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
use crate::repo;
use crate::tmux;
use crate::worker::{Worker, WorkerRequest, WorkerResponse};
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
    PrList,
}

#[derive(PartialEq)]
enum InputMode {
    Normal,
    Interact,
    CreateNew,
    SelectBaseBranch,
    Rename,
    ConfirmDelete,
    PrList,
}

struct PrListDisplayItem {
    number: u64,
    title: String,
    branch: String,
    author: String,
    already_checked_out: bool,
}

struct LoadingState {
    workstreams: bool,
    preview: bool,
    pr_cache: bool,
    pr_details: bool,
    branches: bool,
}

impl LoadingState {
    fn new() -> Self {
        LoadingState {
            workstreams: false,
            preview: false,
            pr_cache: false,
            pr_details: false,
            branches: false,
        }
    }
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
    // Background worker
    worker: Option<Worker>,
    loading: LoadingState,
    initial_load_done: bool,
    /// After create/rename, select this branch when workstreams refresh
    pending_select_branch: Option<String>,
    // PR list mode
    pr_list_items: Vec<PrListDisplayItem>,
    pr_list_state: ListState,
    pr_list_repo_path: String,
    pr_list_repo_name: String,
    // Background git fetch
    last_fetch: Instant,
}

impl App {
    fn new(config: Config) -> Self {
        let mut worker = Worker::spawn();
        // Fire off initial async request instead of blocking
        worker.send(WorkerRequest::RefreshWorkstreams);

        App {
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
            worker: Some(worker),
            loading: LoadingState {
                workstreams: true,
                ..LoadingState::new()
            },
            initial_load_done: false,
            pending_select_branch: None,
            pr_list_items: Vec::new(),
            pr_list_state: ListState::default(),
            pr_list_repo_path: String::new(),
            pr_list_repo_name: String::new(),
            last_fetch: Instant::now(),
        }
    }

    /// Apply workstream data from worker response, preserving selection.
    fn apply_workstream_data(&mut self, items: Vec<crate::worker::WorkstreamData>) {
        let old_selected = self.selected_session();

        self.workstreams = items
            .into_iter()
            .map(|d| {
                let key = format!("{}/{}", d.repo_path, d.branch);
                let pr_number = self.pr_cache.get(&key).copied().flatten();
                WorkstreamItem {
                    repo_name: d.repo_name,
                    branch: d.branch,
                    session: d.session,
                    active: d.active,
                    pr_number,
                    repo_path: d.repo_path,
                }
            })
            .collect();

        // If there's a pending branch selection (from create/rename), prefer that
        if let Some(branch) = self.pending_select_branch.take()
            && let Some(idx) = self.workstreams.iter().position(|w| w.branch == branch)
        {
            self.list_state.select(Some(idx));
            return;
        }

        // Try to preserve selection by session name
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

    /// Process all pending worker responses. Called at top of each run_loop iteration.
    fn process_worker_responses(&mut self) {
        let worker = match self.worker.as_mut() {
            Some(w) => w,
            None => return,
        };
        let responses = worker.try_recv_all();

        for resp in responses {
            match resp {
                WorkerResponse::WorkstreamsRefreshed { items } => {
                    self.loading.workstreams = false;
                    self.apply_workstream_data(items);

                    if !self.initial_load_done {
                        self.initial_load_done = true;
                        if !self.workstreams.is_empty() && self.list_state.selected().is_none() {
                            self.list_state.select(Some(0));
                        }
                        // Trigger PR cache load on first load
                        let mut repo_paths: Vec<String> = self
                            .workstreams
                            .iter()
                            .map(|w| w.repo_path.clone())
                            .collect();
                        repo_paths.sort();
                        repo_paths.dedup();
                        if !repo_paths.is_empty()
                            && let Some(w) = self.worker.as_mut()
                            && w.send(WorkerRequest::LoadPrCache {
                                repo_paths: repo_paths.clone(),
                            })
                        {
                            self.loading.pr_cache = true;
                        }
                        // Fire initial git fetch
                        if !repo_paths.is_empty()
                            && let Some(w) = self.worker.as_mut()
                        {
                            w.send(WorkerRequest::GitFetch { repo_paths });
                            self.last_fetch = Instant::now();
                        }
                    }

                    // Request pane capture for selected workstream
                    self.request_capture_pane();
                }

                WorkerResponse::PaneCaptured {
                    session,
                    window,
                    content,
                } => {
                    self.loading.preview = false;
                    // Only update if it still matches the selected workstream
                    let matches = self
                        .selected()
                        .is_some_and(|ws| ws.session == session && ws.active)
                        && window == self.config.default_window;
                    if matches {
                        self.preview_content = content;
                    }
                }

                WorkerResponse::PrCacheLoaded { entries } => {
                    self.loading.pr_cache = false;
                    for (key, num) in entries {
                        self.pr_cache.insert(key, Some(num));
                    }
                    // Update pr_number on all workstream items
                    for ws in &mut self.workstreams {
                        let key = format!("{}/{}", ws.repo_path, ws.branch);
                        ws.pr_number = self.pr_cache.get(&key).copied().flatten();
                    }
                }

                WorkerResponse::PrDetailsFetched { pr_number, content } => {
                    self.loading.pr_details = false;
                    if self.panel_view == PanelView::PrList {
                        // In PR list mode, show details for the selected PR
                        let matches = self
                            .pr_list_state
                            .selected()
                            .and_then(|i| self.pr_list_items.get(i))
                            .is_some_and(|pr| pr.number == pr_number);
                        if matches {
                            self.github_content = content;
                        }
                    } else if self.panel_view == PanelView::GitHubPr {
                        // In normal GitHub panel mode
                        let matches = self
                            .selected()
                            .and_then(|ws| ws.pr_number)
                            .is_some_and(|n| n == pr_number);
                        if matches {
                            self.github_content = content;
                        }
                    }
                }

                WorkerResponse::BranchesListed { branches } => {
                    self.loading.branches = false;
                    // Only transition if we're still waiting for branches
                    if !self.pending_new_branch.is_empty() {
                        self.branch_candidates = branches;
                        self.branch_list_state
                            .select(if self.branch_candidates.is_empty() {
                                None
                            } else {
                                Some(0)
                            });
                        self.input_mode = InputMode::SelectBaseBranch;
                    }
                }

                WorkerResponse::GitFetchCompleted => {
                    // Reload PR cache after fetch
                    let mut repo_paths: Vec<String> = self
                        .workstreams
                        .iter()
                        .map(|w| w.repo_path.clone())
                        .collect();
                    repo_paths.sort();
                    repo_paths.dedup();
                    if !repo_paths.is_empty()
                        && let Some(w) = self.worker.as_mut()
                    {
                        w.send(WorkerRequest::LoadPrCache { repo_paths });
                    }
                    // If in PR list mode, re-fetch the PR list
                    if self.input_mode == InputMode::PrList
                        && !self.pr_list_repo_path.is_empty()
                        && let Some(w) = self.worker.as_mut()
                    {
                        w.send(WorkerRequest::ListPrs {
                            repo_path: self.pr_list_repo_path.clone(),
                        });
                    }
                }

                WorkerResponse::PrsListed { repo_path, prs } => {
                    if self.input_mode == InputMode::PrList && self.pr_list_repo_path == repo_path {
                        let checked_out_branches: Vec<String> =
                            self.workstreams.iter().map(|w| w.branch.clone()).collect();
                        self.pr_list_items = prs
                            .into_iter()
                            .map(|pr| PrListDisplayItem {
                                already_checked_out: checked_out_branches.contains(&pr.head_ref),
                                number: pr.number,
                                title: pr.title,
                                branch: pr.head_ref,
                                author: pr.author,
                            })
                            .collect();
                        if !self.pr_list_items.is_empty() && self.pr_list_state.selected().is_none()
                        {
                            self.pr_list_state.select(Some(0));
                        }
                    }
                }
            }
        }
    }

    /// Send a CapturePane request if appropriate for the current selection.
    fn request_capture_pane(&mut self) {
        if self.panel_view != PanelView::DefaultWindow {
            return;
        }
        let ws_info = self.selected().map(|ws| (ws.session.clone(), ws.active));
        let default_window = self.config.default_window.clone();

        if let Some((session, active)) = ws_info {
            if active {
                if let Some(w) = self.worker.as_mut()
                    && w.send(WorkerRequest::CapturePane {
                        session,
                        window: default_window,
                    })
                {
                    self.loading.preview = true;
                }
            } else {
                self.preview_content = "(session not active)".into();
            }
        } else {
            self.preview_content = "(no workstreams)".into();
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
        self.request_capture_pane();
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
        self.request_capture_pane();
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
                        self.github_content = "(loading PR details...)".into();
                        if let Some(w) = self.worker.as_mut()
                            && w.send(WorkerRequest::FetchPrDetails {
                                repo_path,
                                pr_number: pr_num,
                            })
                        {
                            self.loading.pr_details = true;
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
                self.request_capture_pane();
            }
            PanelView::PrList => {
                // Toggle not applicable from PR list mode
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

        // Send async request for branches
        self.set_status("Loading branches...");
        if let Some(w) = self.worker.as_mut()
            && w.send(WorkerRequest::ListBranches { repo_path: path })
        {
            self.loading.branches = true;
        }
        // Transition to SelectBaseBranch happens in process_worker_responses
        // when BranchesListed arrives
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
        match workstream::create_no_attach(None, branch, base, true) {
            Ok(_session) => {
                self.set_status(format!("Created workstream '{branch}'"));
                self.pending_select_branch = Some(branch.to_string());
                if let Some(w) = self.worker.as_mut() {
                    w.send(WorkerRequest::RefreshWorkstreams);
                    self.loading.workstreams = true;
                }
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
            match workstream::remove(Some(&repo_name), &branch, true) {
                Ok(()) => {
                    self.set_status(format!("Deleted workstream '{branch}'"));
                    if let Some(w) = self.worker.as_mut() {
                        w.send(WorkerRequest::RefreshWorkstreams);
                        self.loading.workstreams = true;
                    }
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
            match workstream::rename(Some(&repo_name), Some(&old_branch), &new_branch, true) {
                Ok(()) => {
                    self.set_status(format!("Renamed '{old_branch}' -> '{new_branch}'"));
                    self.pending_select_branch = Some(new_branch);
                    if let Some(w) = self.worker.as_mut() {
                        w.send(WorkerRequest::RefreshWorkstreams);
                        self.loading.workstreams = true;
                    }
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

    fn enter_pr_list(&mut self) {
        // Find the first repo path to list PRs for
        let repo_info = self
            .workstreams
            .first()
            .map(|w| (w.repo_path.clone(), w.repo_name.clone()))
            .or_else(|| {
                repo::list_repos()
                    .ok()
                    .and_then(|repos| repos.into_iter().next())
                    .map(|r| (r.path.clone(), r.name.clone()))
            });

        let Some((repo_path, repo_name)) = repo_info else {
            self.set_status("No repos registered");
            return;
        };

        self.input_mode = InputMode::PrList;
        self.panel_view = PanelView::PrList;
        self.panel_scroll = 0;
        self.pr_list_items.clear();
        self.pr_list_state.select(None);
        self.pr_list_repo_path = repo_path.clone();
        self.pr_list_repo_name = repo_name;
        self.github_content = "(select a PR and press Tab for details)".into();

        // Send ListPrs request
        if let Some(w) = self.worker.as_mut() {
            w.send(WorkerRequest::ListPrs {
                repo_path: repo_path.clone(),
            });
        }

        // Trigger a fetch if stale (>10s)
        if self.last_fetch.elapsed() > Duration::from_secs(10) {
            if let Some(w) = self.worker.as_mut() {
                w.send(WorkerRequest::GitFetch {
                    repo_paths: vec![repo_path],
                });
            }
            self.last_fetch = Instant::now();
        }
    }

    fn leave_pr_list(&mut self) {
        self.input_mode = InputMode::Normal;
        self.panel_view = PanelView::DefaultWindow;
        self.panel_scroll = 0;
        self.request_capture_pane();
    }

    fn checkout_selected_pr(&mut self) {
        let selected = self
            .pr_list_state
            .selected()
            .and_then(|i| self.pr_list_items.get(i));
        let Some(pr) = selected else { return };

        if pr.already_checked_out {
            // Switch to existing workstream
            let branch = pr.branch.clone();
            if let Some(idx) = self.workstreams.iter().position(|w| w.branch == branch) {
                self.list_state.select(Some(idx));
                self.leave_pr_list();
                self.set_status(format!("Switched to existing workstream '{branch}'"));
            }
            return;
        }

        let branch = pr.branch.clone();
        let pr_number = pr.number;
        let repo_name = self.pr_list_repo_name.clone();

        self.set_status(format!("Checking out PR #{pr_number}..."));

        match workstream::checkout_pr(&repo_name, &branch, pr_number, true) {
            Ok(_session) => {
                self.set_status(format!("Checked out PR #{pr_number} ('{branch}')"));
                self.pending_select_branch = Some(branch);
                if let Some(w) = self.worker.as_mut() {
                    w.send(WorkerRequest::RefreshWorkstreams);
                    self.loading.workstreams = true;
                }
                self.leave_pr_list();
            }
            Err(e) => self.set_status(format!("Error: {e}")),
        }
    }

    fn pr_list_show_details(&mut self) {
        let selected = self
            .pr_list_state
            .selected()
            .and_then(|i| self.pr_list_items.get(i));
        let Some(pr) = selected else { return };

        let pr_number = pr.number;
        let repo_path = self.pr_list_repo_path.clone();
        self.github_content = "(loading PR details...)".into();
        self.panel_scroll = 0;
        if let Some(w) = self.worker.as_mut()
            && w.send(WorkerRequest::FetchPrDetails {
                repo_path,
                pr_number,
            })
        {
            self.loading.pr_details = true;
        }
    }

    fn pr_list_move_up(&mut self) {
        if self.pr_list_items.is_empty() {
            return;
        }
        let i = match self.pr_list_state.selected() {
            Some(i) if i > 0 => i - 1,
            Some(_) => self.pr_list_items.len() - 1,
            None => 0,
        };
        self.pr_list_state.select(Some(i));
        self.panel_scroll = 0;
    }

    fn pr_list_move_down(&mut self) {
        if self.pr_list_items.is_empty() {
            return;
        }
        let i = match self.pr_list_state.selected() {
            Some(i) if i < self.pr_list_items.len() - 1 => i + 1,
            Some(_) => 0,
            None => 0,
        };
        self.pr_list_state.select(Some(i));
        self.panel_scroll = 0;
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

    // Shut down worker thread
    if let Some(worker) = app.worker.take() {
        worker.shutdown();
    }

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
        // Process any pending worker responses before drawing
        app.process_worker_responses();

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
                            KeyCode::Char('p') => app.enter_pr_list(),
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
                        app.pending_new_branch.clear();
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
                InputMode::PrList => match key.code {
                    KeyCode::Esc => app.leave_pr_list(),
                    KeyCode::Char('j') | KeyCode::Down => app.pr_list_move_down(),
                    KeyCode::Char('k') | KeyCode::Up => app.pr_list_move_up(),
                    KeyCode::Enter => app.checkout_selected_pr(),
                    KeyCode::Tab => app.pr_list_show_details(),
                    _ => {}
                },
            }
        }

        // Periodic refresh (and always refresh preview in Interact mode)
        if app.input_mode == InputMode::Interact || last_refresh.elapsed() >= refresh_interval {
            if app.input_mode != InputMode::Interact
                && let Some(w) = app.worker.as_mut()
            {
                w.send(WorkerRequest::RefreshWorkstreams);
            }
            app.request_capture_pane();
            *last_refresh = Instant::now();
        }

        // Periodic git fetch (every 60s)
        if app.last_fetch.elapsed() >= Duration::from_secs(60) {
            let mut repo_paths: Vec<String> = app
                .workstreams
                .iter()
                .map(|w| w.repo_path.clone())
                .collect();
            repo_paths.sort();
            repo_paths.dedup();
            if !repo_paths.is_empty()
                && let Some(w) = app.worker.as_mut()
            {
                w.send(WorkerRequest::GitFetch { repo_paths });
            }
            app.last_fetch = Instant::now();
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

    if app.input_mode == InputMode::PrList {
        // PR list mode: left PR list + right PR details
        let content = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(outer[1]);

        draw_pr_list(f, app, content[0]);
        draw_pr_detail_panel(f, app, content[1]);
    } else {
        // Normal mode: left workstream list + right preview
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
    }
    draw_status(f, app, outer[2]);
    draw_help(f, app, outer[3]);
}

fn draw_header(f: &mut ratatui::Frame, area: Rect) {
    let version = env!("VEX_VERSION");
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
    let items: Vec<ListItem> = if app.workstreams.is_empty() && app.loading.workstreams {
        vec![ListItem::new(Line::from(Span::styled(
            "(loading...)",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.workstreams
            .iter()
            .map(|ws| {
                let style = if ws.active {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                ListItem::new(Line::from(Span::styled(ws.display(), style)))
            })
            .collect()
    };

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

            let preview = Paragraph::new(Text::from(app.preview_content.as_str()))
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(border_color)),
                )
                .wrap(Wrap { trim: false })
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
        PanelView::PrList => {
            // Handled by draw_pr_list / draw_pr_detail_panel in PrList mode
        }
    }
}

fn draw_pr_list(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = if app.pr_list_items.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "(loading PRs...)",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.pr_list_items
            .iter()
            .map(|pr| {
                let checked = if pr.already_checked_out {
                    " [checked out]"
                } else {
                    ""
                };
                let display = format!(
                    "#{} {} (@{}) {}{}",
                    pr.number, pr.branch, pr.author, pr.title, checked
                );
                let style = if pr.already_checked_out {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Line::from(Span::styled(display, style)))
            })
            .collect()
    };

    let title = format!(" Open PRs ({}) ", app.pr_list_repo_name);
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Magenta)),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, area, &mut app.pr_list_state);
}

fn draw_pr_detail_panel(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let title = app
        .pr_list_state
        .selected()
        .and_then(|i| app.pr_list_items.get(i))
        .map(|pr| format!(" PR #{} ", pr.number))
        .unwrap_or_else(|| " PR Details ".into());

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
        InputMode::Normal | InputMode::Interact | InputMode::PrList => {
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
            "[Enter] Interact  [Tab] Toggle PR  [^O] Open  [j/k] Navigate  [n] New  [d] Del  [r] Rename  [p] PRs  [q] Quit"
        }
        InputMode::Interact => "[Esc] Back  \u{2500}  Keystrokes forwarded to pane",
        InputMode::CreateNew | InputMode::Rename => "[Enter] Confirm  [Esc] Cancel",
        InputMode::SelectBaseBranch => {
            "[Enter] Select  [Up/Down] Navigate  [type] Filter  [Esc] Cancel"
        }
        InputMode::ConfirmDelete => "[y] Confirm  [any] Cancel",
        InputMode::PrList => "[Enter] Checkout  [Tab] Details  [j/k] Navigate  [Esc] Back",
    };

    let help = Paragraph::new(Line::from(Span::styled(
        help_text,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(help, area);
}
