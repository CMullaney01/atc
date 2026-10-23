//! The popup you bring up to see every session at once. It owns the terminal
//! while it is open, so it puts the screen back before it moves the tmux client.
use crate::pane::Kind;
use crate::state::{self, Status};
use crate::{bar, tmux};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
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
    kind: Kind,
    status: Option<Status>,
    stale: bool,
    age: Option<u64>,
    tool: Option<String>,
    last_message: Option<String>,
    repo: Option<String>,
    active: bool,
}

impl Item {
    fn glyph(&self) -> &'static str {
        match self.status {
            Some(s) => s.glyph(),
            None => self.kind.glyph(),
        }
    }

    fn colour(&self) -> Color {
        match self.status {
            Some(Status::Idle) => Color::Indexed(244),
            Some(Status::Working) => Color::Indexed(75),
            Some(Status::Waiting) => Color::Indexed(208),
            Some(Status::Blocked) => Color::Indexed(171),
            Some(Status::Error) => Color::Indexed(196),
            None => match self.kind {
                Kind::Unreported | Kind::Job => Color::Indexed(244),
                _ => Color::Indexed(240),
            },
        }
    }

    fn state_label(&self) -> &'static str {
        match self.status {
            Some(s) => s.label(),
            None => self.kind.label(),
        }
    }
}

enum Mode {
    Browse,
    Prompt(String),
    ConfirmKill,
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

/// Inventory is already sorted AI-first, so the AI section is a prefix of this
/// list and hiding the rest is just a shorter slice.
fn snapshot(cache: &mut HashMap<String, Option<String>>) -> (Vec<Item>, usize) {
    let states = state::all();
    let panes = tmux::list_panes();
    let inv = bar::inventory(&states, &panes);
    let ai_count = inv.iter().filter(|e| e.kind.is_ai()).count();
    let items = inv
        .iter()
        .map(|e| Item {
            pane_id: e.info.pane_id.clone(),
            session: e.info.session.clone(),
            window_index: e.info.window_index,
            address: format!("{}:{}", e.info.session, e.info.address()),
            title: e.title(),
            kind: e.kind,
            status: e.status(),
            stale: e.stale(),
            age: e.state.map(|s| s.age_secs()),
            tool: e.state.and_then(|s| s.tool.clone()),
            last_message: e.state.and_then(|s| s.last_message.clone()),
            repo: repo_label(
                e.state
                    .and_then(|s| s.cwd.as_deref())
                    .unwrap_or(&e.info.path),
                cache,
            ),
            active: e.info.is_current(),
        })
        .collect();
    (items, ai_count)
}

pub(crate) fn run() {
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
    let (mut items, mut ai_count) = snapshot(&mut cache);
    let mut selected: usize = 0;
    let mut show_others = true;
    let mut mode = Mode::Browse;
    let mut last_refresh = Instant::now();
    let mut jump: Option<(String, String, u32)> = None;

    loop {
        let visible = if show_others { items.len() } else { ai_count };
        selected = selected.min(visible.saturating_sub(1));
        terminal.draw(|f| draw(f, &items, ai_count, visible, selected, &mode))?;

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
                            (items, ai_count) = snapshot(&mut cache);
                            mode = Mode::Browse;
                        }
                        _ => mode = Mode::Browse,
                    },
                    Mode::Browse => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            if visible > 0 {
                                selected = (selected + 1).min(visible - 1);
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            selected = selected.saturating_sub(1);
                        }
                        KeyCode::Char('g') => selected = 0,
                        KeyCode::Char('G') => selected = visible.saturating_sub(1),
                        KeyCode::Tab => show_others = !show_others,
                        KeyCode::Char('r') => {
                            cache.clear();
                            (items, ai_count) = snapshot(&mut cache);
                        }
                        KeyCode::Char('p') => {
                            // Only an AI session can take a prompt.
                            if items.get(selected).is_some_and(|i| i.kind.is_ai()) {
                                mode = Mode::Prompt(String::new());
                            }
                        }
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
            (items, ai_count) = snapshot(&mut cache);
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

fn item_line(item: &Item, chosen: bool, dim: bool) -> Line<'static> {
    let mut glyph_style = Style::default().fg(item.colour());
    if item.stale {
        glyph_style = glyph_style.add_modifier(Modifier::BOLD);
    }
    let mut base = if chosen {
        Style::default()
            .bg(Color::Indexed(237))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    if dim && !chosen {
        base = base.fg(Color::Indexed(243));
    }
    Line::from(vec![
        Span::styled(if chosen { " ▌" } else { "  " }, base),
        Span::styled(
            if item.active { "▸" } else { " " },
            base.fg(Color::Indexed(250)),
        ),
        Span::styled(format!("{} ", item.glyph()), glyph_style),
        Span::styled(format!("{:<26} ", crop(&item.title, 26)), base),
        Span::styled(
            format!("{:<11} ", item.state_label()),
            Style::default().fg(item.colour()),
        ),
        Span::styled(
            format!(
                "{:>5}  ",
                item.age
                    .map(state::fmt_age)
                    .unwrap_or_else(|| "-".to_string())
            ),
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
    ])
}

fn draw(
    f: &mut ratatui::Frame,
    items: &[Item],
    ai_count: usize,
    visible: usize,
    selected: usize,
    mode: &Mode,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(f.area());

    let mut lines: Vec<Line> = Vec::new();
    if ai_count == 0 {
        lines.push(Line::from(Span::styled(
            "  no AI sessions — start a Claude session in a tmux pane",
            Style::default().fg(Color::Indexed(244)),
        )));
    }

    for (i, item) in items.iter().take(visible).enumerate() {
        if i == ai_count {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  other panes",
                Style::default()
                    .fg(Color::Indexed(240))
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        let chosen = i == selected;
        lines.push(item_line(item, chosen, i >= ai_count));
        if chosen {
            if let Some(tool) = &item.tool {
                lines.push(Line::from(Span::styled(
                    format!("        needs: {tool}"),
                    Style::default().fg(Color::Indexed(171)),
                )));
            }
            if let Some(msg) = &item.last_message {
                lines.push(Line::from(Span::styled(
                    format!("        └ {}", crop(msg, 100)),
                    Style::default().fg(Color::Indexed(248)),
                )));
            }
            if item.kind == Kind::Unreported {
                lines.push(Line::from(Span::styled(
                    "        └ hasn't reported yet — state appears on its next activity",
                    Style::default().fg(Color::Indexed(244)),
                )));
            }
        }
    }

    let needs_you = items
        .iter()
        .take(ai_count)
        .filter(|i| {
            matches!(
                i.status,
                Some(Status::Waiting | Status::Blocked | Status::Error)
            )
        })
        .count();
    let hidden = items.len() - visible;
    let title = if hidden > 0 {
        format!(" atc — {ai_count} session(s), {needs_you} need you, {hidden} pane(s) hidden ")
    } else {
        format!(" atc — {ai_count} session(s), {needs_you} need you ")
    };
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        chunks[0],
    );

    let key = |k: &'static str| Span::styled(k, Style::default().fg(Color::Indexed(75)));
    let footer = match mode {
        Mode::Browse => Line::from(vec![
            Span::raw(" "),
            key("enter"),
            Span::raw(" jump   "),
            key("p"),
            Span::raw(" prompt   "),
            key("x"),
            Span::raw(" kill   "),
            key("tab"),
            Span::raw(" other panes   "),
            key("r"),
            Span::raw(" refresh   "),
            key("q"),
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
                items
                    .get(selected)
                    .map(|i| i.address.as_str())
                    .unwrap_or("")
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
    s.chars()
        .take(max.saturating_sub(1))
        .chain("…".chars())
        .collect()
}
