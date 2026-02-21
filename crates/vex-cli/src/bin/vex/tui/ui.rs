use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use vex_cli::WorkstreamStatus;

use super::app::{App, Mode, running_agents_count, ws_status_str};

// ── Main render ───────────────────────────────────────────────────────────────

pub fn render(f: &mut Frame, app: &App) {
    let area = f.area();

    // Split into header | body | footer
    let footer_height = match &app.mode {
        Mode::SpawnInput | Mode::ConfirmAttach { .. } => 3,
        _ => 1,
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(footer_height),
        ])
        .split(area);

    render_header(f, app, chunks[0]);
    render_body(f, app, chunks[1]);
    render_footer(f, app, chunks[2]);
}

// ── Header ────────────────────────────────────────────────────────────────────

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let total_ws = app.total_workstreams();
    let ws_word = if total_ws == 1 {
        "workstream"
    } else {
        "workstreams"
    };

    let left = Span::styled(
        "  VEX",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let right = format!("vexd @ {}  ●  {total_ws} {ws_word}  ", app.conn_label);
    let right_span = Span::styled(right, Style::default().fg(Color::DarkGray));

    // Use a two-column layout for left/right alignment
    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(50)])
        .split(area);

    f.render_widget(Paragraph::new(Line::from(vec![left])), header_chunks[0]);
    f.render_widget(
        Paragraph::new(right_span).alignment(Alignment::Right),
        header_chunks[1],
    );
}

// ── Body ──────────────────────────────────────────────────────────────────────

fn render_body(f: &mut Frame, app: &App, area: Rect) {
    if app.repos.is_empty() {
        let msg = Paragraph::new("No repositories registered. Run 'vexd repo register <path>'.")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(msg, area);
        return;
    }

    // Compute heights: each repo block = 2 (border) + 1 (pad top) + n_ws rows + 1 (pad bot)
    let block_heights: Vec<u16> = app
        .repos
        .iter()
        .map(|r| {
            let rows = r.workstreams.len().max(1) as u16;
            rows + 4
        })
        .collect();

    let gap_between = 1u16;
    let total_blocks = app.repos.len();
    let total_h: u16 =
        block_heights.iter().sum::<u16>() + gap_between * (total_blocks.saturating_sub(1)) as u16;

    // Build constraints: [block, gap, block, gap, ...]
    let mut constraints = Vec::new();
    for (i, h) in block_heights.iter().enumerate() {
        constraints.push(Constraint::Length(*h));
        if i + 1 < total_blocks {
            constraints.push(Constraint::Length(gap_between));
        }
    }
    // If content fits, leave remaining space
    if total_h < area.height {
        constraints.push(Constraint::Min(0));
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let positions = app.ws_positions();
    let mut chunk_idx = 0usize;
    for (ri, repo) in app.repos.iter().enumerate() {
        let block_area = chunks[chunk_idx];
        chunk_idx += 2; // skip gap chunk (or skip nothing for last)

        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", repo.name));
        let inner = block.inner(block_area);
        f.render_widget(block, block_area);

        if repo.workstreams.is_empty() {
            let empty =
                Paragraph::new("  (no workstreams)").style(Style::default().fg(Color::DarkGray));
            f.render_widget(empty, inner);
            continue;
        }

        // Render each workstream row
        let row_constraints: Vec<Constraint> = repo
            .workstreams
            .iter()
            .map(|_| Constraint::Length(1))
            .collect();
        let row_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(row_constraints)
            .split(inner);

        for (wi, ws) in repo.workstreams.iter().enumerate() {
            let flat_idx = positions.iter().position(|&(r, w)| r == ri && w == wi);
            let is_selected = flat_idx == Some(app.selected_ws);

            let marker = if is_selected { "▶ " } else { "  " };
            let running = running_agents_count(ws);
            let agents_str = if running == 1 {
                format!("{running} agent  ")
            } else {
                format!("{running} agents ")
            };
            let status_color = match ws.status {
                WorkstreamStatus::Running => Color::Green,
                WorkstreamStatus::Idle => Color::Yellow,
                WorkstreamStatus::Stopped => Color::Red,
            };

            let row_line = Line::from(vec![
                Span::styled(
                    marker,
                    if is_selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    format!("{:<20}", ws.name),
                    if is_selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    format!("{:<20}", ws.branch),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("{:<12}", agents_str), Style::default()),
                Span::raw("1 shell   "),
                Span::styled(ws_status_str(&ws.status), Style::default().fg(status_color)),
            ]);

            let row_style = if is_selected {
                Style::default().bg(Color::DarkGray)
            } else {
                Style::default()
            };
            f.render_widget(Paragraph::new(row_line).style(row_style), row_chunks[wi]);
        }
    }
}

// ── Footer ────────────────────────────────────────────────────────────────────

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let content = match &app.mode {
        Mode::Normal => {
            let keys = "↑↓ navigate   enter attach   a spawn agent   s shell   d delete   r refresh   q quit";
            let line = if let Some(msg) = &app.status_msg {
                Line::from(vec![
                    Span::styled(msg.clone(), Style::default().fg(Color::Yellow)),
                    Span::raw("   "),
                    Span::styled(keys, Style::default().fg(Color::DarkGray)),
                ])
            } else {
                Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray)))
            };
            Paragraph::new(line)
        }
        Mode::SpawnInput => {
            let prompt = format!("Task: {}", app.spawn_input);
            let lines = vec![
                Line::from(Span::styled(prompt, Style::default())),
                Line::from(Span::styled(
                    "enter to submit   esc to cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            Paragraph::new(lines)
        }
        Mode::ConfirmAttach { .. } => {
            let lines = vec![
                Line::from(Span::styled(
                    "Agent spawned.  Attach to it? [y/N]",
                    Style::default().fg(Color::Green),
                )),
                Line::from(Span::styled(
                    "y/enter attach   n/esc skip",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            Paragraph::new(lines)
        }
        Mode::ConfirmDelete => Paragraph::new(Line::from(vec![
            Span::styled("Delete this workstream? ", Style::default().fg(Color::Red)),
            Span::styled("[y/N]", Style::default().fg(Color::Yellow)),
            Span::styled(
                "  y confirm   esc cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ])),
    };

    f.render_widget(content, area);
}
