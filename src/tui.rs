use std::io;
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
use crate::repo;
use crate::tmux;
use crate::workstream;

struct WorkstreamItem {
    repo_name: String,
    branch: String,
    session: String,
    active: bool,
}

impl WorkstreamItem {
    fn display(&self) -> String {
        let active_marker = if self.active { " [active]" } else { "" };
        format!("{}/{}{}", self.repo_name, self.branch, active_marker)
    }
}

#[derive(PartialEq)]
enum InputMode {
    Normal,
    CreateNew,
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
        };
        app.refresh_workstreams();
        if !app.workstreams.is_empty() {
            app.list_state.select(Some(0));
            app.update_preview();
        }
        app
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
                items.push(WorkstreamItem {
                    repo_name: repo_meta.name.clone(),
                    branch: ws.branch.clone(),
                    session,
                    active,
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
        if let Some(ws) = self.selected() {
            if ws.active {
                let default_window = self
                    .config
                    .windows
                    .first()
                    .map(|w| w.name.as_str())
                    .unwrap_or("0");
                self.preview_content = tmux::capture_pane(&ws.session, default_window);
            } else {
                self.preview_content = "(session not active)".into();
            }
        } else {
            self.preview_content = "(no workstreams)".into();
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
        self.update_preview();
    }

    fn handle_switch(&mut self) {
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

    fn start_create(&mut self) {
        self.input_mode = InputMode::CreateNew;
        self.input_buffer.clear();
        self.input_cursor = 0;
    }

    fn confirm_create(&mut self) {
        let branch = self.input_buffer.trim().to_string();
        self.input_mode = InputMode::Normal;
        if branch.is_empty() {
            self.set_status("Cancelled: empty branch name");
            return;
        }
        match workstream::create_no_attach(None, &branch) {
            Ok(_session) => {
                self.set_status(format!("Created workstream '{branch}'"));
                self.refresh_workstreams();
                // Select the newly created workstream
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

        // Poll for events with timeout for periodic refresh
        let timeout = Duration::from_millis(200);
        if event::poll(timeout)
            .map_err(|e| VexError::ConfigError(format!("event poll error: {e}")))?
            && let Event::Key(key) = event::read()
                .map_err(|e| VexError::ConfigError(format!("event read error: {e}")))?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            // Ctrl+C always quits
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                break;
            }

            match app.input_mode {
                InputMode::Normal => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('j') | KeyCode::Down => app.move_down(),
                    KeyCode::Char('k') | KeyCode::Up => app.move_up(),
                    KeyCode::Enter => app.handle_switch(),
                    KeyCode::Char('n') => app.start_create(),
                    KeyCode::Char('d') => app.start_delete(),
                    KeyCode::Char('r') => app.start_rename(),
                    _ => {}
                },
                InputMode::CreateNew => match key.code {
                    KeyCode::Enter => app.confirm_create(),
                    KeyCode::Esc => {
                        app.input_mode = InputMode::Normal;
                        app.set_status("Cancelled");
                    }
                    _ => app.handle_input_key(key.code),
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

        // Periodic refresh
        if last_refresh.elapsed() >= refresh_interval {
            app.refresh_workstreams();
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

    // Main layout: content area + status + help bar
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // content
            Constraint::Length(1), // status
            Constraint::Length(1), // help
        ])
        .split(size);

    // Content: left list + right preview
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(outer[0]);

    draw_list(f, app, content[0]);
    draw_preview(f, app, content[1]);
    draw_status(f, app, outer[1]);
    draw_help(f, app, outer[2]);
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

fn draw_preview(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let title = match app.selected() {
        Some(ws) => format!(" Preview - {} ", ws.session),
        None => " Preview ".into(),
    };

    let preview = Paragraph::new(Text::from(app.preview_content.as_str()))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue)),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(preview, area);
}

fn draw_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let content = match &app.input_mode {
        InputMode::CreateNew => {
            let label = Span::styled("New branch: ", Style::default().fg(Color::Yellow));
            let input = Span::raw(&app.input_buffer);
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
        InputMode::Normal => {
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
        InputMode::Normal => "[Enter] Switch  [n] New  [d] Delete  [r] Rename  [q] Quit",
        InputMode::CreateNew | InputMode::Rename => "[Enter] Confirm  [Esc] Cancel",
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
