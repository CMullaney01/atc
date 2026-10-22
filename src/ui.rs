use crate::state::{self, Status};
use crate::{bar, tmux};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

struct Item {
    pane_id: String,
    session: String,
    window_index: u32,
    address: String,
    title: String,
    status: Status,
    stale: bool,
    age: u64,
    tool: Option<String>,
    last_message: Option<String>,
    repo: Option<String>,
    active: bool,
}

enum Mode {
    Browse,
    Prompt(String),
    ConfirmKill,
}

fn colour(status: Status) -> Color {
    match status {
        Status::Idle => Color::Indexed(244),
        Status::Working => Color::Indexed(75),
        Status::Waiting => Color::Indexed(208),
        Status::Blocked => Color::Indexed(171),
        Status::Error => Color::Indexed(196),
    }
}

fn repo_label(cwd: &str, cache: &mut HashMap<String, Option<String>>) -> Option<String> {
    if let Some(hit) = cache.get(cwd) {
        return hit.clone();
    }
    let name = std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().to_string());
    let branch = std::process::Command::new("git")
        .args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty());
    let label = match (name, branch) {
        (Some(n), Some(b)) => Some(format!("{n}/{b}")),
        (Some(n), None) => Some(n),
        _ => None,
    };
    cache.insert(cwd.to_string(), label.clone());
    label
}

/// State carries a title only for hooked sessions; anything reporting through
/// `atc state` falls back to what tmux knows about the pane.
fn pick_title(from_state: Option<&str>, pane_title: &str, window_name: &str) -> String {
    for candidate in [from_state.unwrap_or(""), pane_title, window_name] {
        let cleaned = tmux::clean_title(candidate);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    String::new()
}

fn snapshot(cache: &mut HashMap<String, Option<String>>) -> Vec<Item> {
    let states = state::all();
    let panes = tmux::list_panes();
    bar::live(&states, &panes)
        .iter()
        .map(|e| Item {
            pane_id: e.info.pane_id.clone(),
            session: e.info.session.clone(),
            window_index: e.info.window_index,
            address: format!("{}:{}", e.info.session, e.info.address()),
            title: pick_title(e.state.title.as_deref(), &e.info.title, &e.info.window_name),
            status: e.state.status,
            stale: e.state.stale,
            age: e.state.age_secs(),
            tool: e.state.tool.clone(),
            last_message: e.state.last_message.clone(),
            repo: repo_label(
                e.state.cwd.as_deref().unwrap_or(&e.info.path),
                cache,
            ),
            active: e.info.active,
        })
        .collect()
}

pub fn run() {
    if let Err(e) = event_loop() {
        eprintln!("atc ui: {e}");
        std::process::exit(1);
    }
}

fn event_loop() -> io::Result<()> {
    let mut out = io::stdout();
    enable_raw_mode()?;
    crossterm::execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let mut cache: HashMap<String, Option<String>> = HashMap::new();
    let mut items = snapshot(&mut cache);
    let mut selected: usize = 0;
    let mut mode = Mode::Browse;
    let mut last_refresh = Instant::now();
    let mut jump: Option<(String, String, u32)> = None;

    loop {
        terminal.draw(|f| draw(f, &items, selected, &mode))?;

        if event::poll(Duration::from_millis(300))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match &mut mode {
                    Mode::Prompt(buf) => match key.code {
                        KeyCode::Esc => mode = Mode::Browse,
                        KeyCode::Enter => {
                            if let Some(item) = items.get(selected) {
                                if !buf.is_empty() {
                                    tmux::send_keys(&item.pane_id, buf);
                                }
                            }
                            mode = Mode::Browse;
                        }
                        KeyCode::Backspace => {
                            buf.pop();
                        }
                        KeyCode::Char(c) => buf.push(c),
                        _ => {}
                    },
                    Mode::ConfirmKill => match key.code {
                        KeyCode::Char('y') => {
                            if let Some(item) = items.get(selected) {
                                tmux::kill_pane(&item.pane_id);
                                let _ = state::remove(&item.pane_id);
                            }
                            items = snapshot(&mut cache);
                            selected = selected.min(items.len().saturating_sub(1));
                            mode = Mode::Browse;
                        }
                        _ => mode = Mode::Browse,
                    },
                    Mode::Browse => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                        KeyCode::Char('j') | KeyCode::Down => {
                            if !items.is_empty() {
                                selected = (selected + 1).min(items.len() - 1);
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            selected = selected.saturating_sub(1);
                        }
                        KeyCode::Char('g') => selected = 0,
                        KeyCode::Char('G') => selected = items.len().saturating_sub(1),
                        KeyCode::Char('r') => {
                            cache.clear();
                            items = snapshot(&mut cache);
                        }
                        KeyCode::Char('p') => mode = Mode::Prompt(String::new()),
                        KeyCode::Char('x') => {
                            if !items.is_empty() {
                                mode = Mode::ConfirmKill;
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(item) = items.get(selected) {
                                jump = Some((
                                    item.pane_id.clone(),
                                    item.session.clone(),
                                    item.window_index,
                                ));
                            }
                            break;
                        }
                        _ => {}
                    },
                }
            }
        }

        if last_refresh.elapsed() > Duration::from_secs(1) {
            items = snapshot(&mut cache);
            if !items.is_empty() {
                selected = selected.min(items.len() - 1);
            }
            last_refresh = Instant::now();
        }
    }

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Jump after restoring the terminal so the popup is gone before the client moves.
    if let Some((pane, session, window)) = jump {
        tmux::focus(&pane, &session, window);
    }
    Ok(())
}

fn draw(f: &mut ratatui::Frame, items: &[Item], selected: usize, mode: &Mode) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(f.area());

    let mut lines: Vec<Line> = Vec::new();
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no tracked sessions — start a Claude session in a tmux pane",
            Style::default().fg(Color::Indexed(244)),
        )));
    }
    for (i, item) in items.iter().enumerate() {
        let chosen = i == selected;
        let mut glyph = Style::default().fg(colour(item.status));
        if item.stale {
            glyph = glyph.add_modifier(Modifier::BOLD);
        }
        let base = if chosen {
            Style::default().bg(Color::Indexed(237)).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(if chosen { " ▌" } else { "  " }, base),
            Span::styled(
                if item.active { "▸" } else { " " },
                base.fg(Color::Indexed(250)),
            ),
            Span::styled(format!("{} ", item.status.glyph()), glyph),
            Span::styled(format!("{:<28} ", crop(&item.title, 28)), base),
            Span::styled(
                format!("{:<8} ", item.status.label()),
                Style::default().fg(colour(item.status)),
            ),
            Span::styled(
                format!("{:>5}  ", state::fmt_age(item.age)),
                base.fg(Color::Indexed(244)),
            ),
            Span::styled(
                format!("{:<22} ", crop(&item.address, 22)),
                base.fg(Color::Indexed(250)),
            ),
            Span::styled(
                item.repo.clone().unwrap_or_default(),
                base.fg(Color::Indexed(244)),
            ),
        ]));
        if chosen {
            if let Some(tool) = &item.tool {
                lines.push(Line::from(Span::styled(
                    format!("      needs: {tool}"),
                    Style::default().fg(Color::Indexed(171)),
                )));
            }
            if let Some(msg) = &item.last_message {
                lines.push(Line::from(Span::styled(
                    format!("      └ {}", crop(msg, 100)),
                    Style::default().fg(Color::Indexed(248)),
                )));
            }
        }
    }

    let waiting = items
        .iter()
        .filter(|i| matches!(i.status, Status::Waiting | Status::Blocked | Status::Error))
        .count();
    let title = format!(" atc — {} session(s), {waiting} need you ", items.len());
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        chunks[0],
    );

    let footer = match mode {
        Mode::Browse => Line::from(vec![
            Span::styled(" enter", Style::default().fg(Color::Indexed(75))),
            Span::raw(" jump   "),
            Span::styled("p", Style::default().fg(Color::Indexed(75))),
            Span::raw(" prompt   "),
            Span::styled("x", Style::default().fg(Color::Indexed(75))),
            Span::raw(" kill   "),
            Span::styled("r", Style::default().fg(Color::Indexed(75))),
            Span::raw(" refresh   "),
            Span::styled("q", Style::default().fg(Color::Indexed(75))),
            Span::raw(" close"),
        ]),
        Mode::Prompt(buf) => Line::from(vec![
            Span::styled(" send: ", Style::default().fg(Color::Indexed(208))),
            Span::raw(buf.clone()),
            Span::styled("▏", Style::default().fg(Color::Indexed(208))),
        ]),
        Mode::ConfirmKill => Line::from(Span::styled(
            format!(
                " kill {}?  y / any other key to cancel",
                items.get(selected).map(|i| i.address.as_str()).unwrap_or("")
            ),
            Style::default().fg(Color::Indexed(196)),
        )),
    };
    f.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::ALL)),
        chunks[1],
    );
}

fn crop(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain("…".chars()).collect()
}
