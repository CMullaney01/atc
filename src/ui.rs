//! The popup you bring up to see every session at once, with a live view of
//! whichever one is selected so you can read it without going there. It owns the
//! terminal while it is open, so it puts the screen back before it moves the
//! tmux client.
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

/// How long a captured screen is reused before it is taken again.
const PREVIEW_MAX_AGE: Duration = Duration::from_millis(700);

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

/// The screen of one pane, kept between frames so every redraw does not shell
/// out to tmux.
struct Preview {
    pane_id: String,
    lines: Vec<String>,
    taken: Instant,
}

impl Preview {
    fn blank() -> Self {
        Preview {
            pane_id: String::new(),
            lines: Vec::new(),
            taken: Instant::now() - PREVIEW_MAX_AGE,
        }
    }

    fn follow(&mut self, pane_id: Option<&str>) {
        let Some(pane_id) = pane_id else {
            self.pane_id.clear();
            self.lines.clear();
            return;
        };
        if self.pane_id == pane_id && self.taken.elapsed() < PREVIEW_MAX_AGE {
            return;
        }
        self.pane_id = pane_id.to_string();
        self.lines = tmux::capture_pane(pane_id);
        self.taken = Instant::now();
    }

    /// The bottom of a pane's screen is the part worth reading, so a short
    /// window shows the end rather than the beginning.
    fn tail(&self, height: usize) -> &[String] {
        let start = self.lines.len().saturating_sub(height);
        &self.lines[start..]
    }
}

/// One drawn line of the list: either a section heading or one pane.
enum Row {
    Heading(&'static str),
    Pane(usize),
}

/// Inventory is sorted AI-first, so the heading goes in front of the first
/// non-AI pane and nowhere else.
fn rows(ai_count: usize, visible: usize) -> Vec<Row> {
    let mut out = Vec::with_capacity(visible + 1);
    for i in 0..visible {
        if i == ai_count {
            out.push(Row::Heading("other panes"));
        }
        out.push(Row::Pane(i));
    }
    out
}

/// The chosen pane sits one row lower once the heading is on screen above it.
fn row_of(selected: usize, ai_count: usize, visible: usize) -> usize {
    selected + usize::from(ai_count < visible && selected >= ai_count)
}

/// Scroll only when the chosen row would otherwise fall off an edge, so the
/// list stays still while you move around inside it.
fn scroll_offset(total_rows: usize, height: usize, chosen_row: usize, current: usize) -> usize {
    if height == 0 {
        return 0;
    }
    let furthest = total_rows.saturating_sub(height);
    let mut offset = current.min(furthest);
    if chosen_row < offset {
        offset = chosen_row;
    } else if chosen_row >= offset + height {
        offset = chosen_row + 1 - height;
    }
    offset.min(furthest)
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
    let mut offset: usize = 0;
    let mut show_others = true;
    let mut mode = Mode::Browse;
    let mut preview = Preview::blank();
    let mut last_refresh = Instant::now();
    let mut jump: Option<(String, String, u32)> = None;

    loop {
        let visible = if show_others { items.len() } else { ai_count };
        selected = selected.min(visible.saturating_sub(1));
        preview.follow(items.get(selected).map(|i| i.pane_id.as_str()));

        terminal.draw(|f| {
            let view = View {
                items: &items,
                ai_count,
                visible,
                selected,
                mode: &mode,
                preview: &preview,
            };
            offset = draw(f, &view, offset);
        })?;

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

struct View<'a> {
    items: &'a [Item],
    ai_count: usize,
    visible: usize,
    selected: usize,
    mode: &'a Mode,
    preview: &'a Preview,
}

/// Returns the scroll offset it settled on, so the next frame starts where this
/// one left off.
fn draw(f: &mut ratatui::Frame, v: &View, offset: usize) -> usize {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(3)])
        .split(f.area());
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(outer[0]);

    let all_rows = rows(v.ai_count, v.visible);
    let empty_notice = usize::from(v.ai_count == 0);
    let list_height = usize::from(split[0].height.saturating_sub(2)).saturating_sub(empty_notice);
    let offset = scroll_offset(
        all_rows.len(),
        list_height,
        row_of(v.selected, v.ai_count, v.visible),
        offset,
    );

    let mut lines: Vec<Line> = Vec::new();
    if v.ai_count == 0 {
        lines.push(Line::from(Span::styled(
            "  no AI sessions — start a Claude session in a tmux pane",
            Style::default().fg(Color::Indexed(244)),
        )));
    }
    for row in all_rows.iter().skip(offset).take(list_height) {
        match row {
            Row::Heading(text) => lines.push(Line::from(Span::styled(
                format!("  {text}"),
                Style::default()
                    .fg(Color::Indexed(240))
                    .add_modifier(Modifier::ITALIC),
            ))),
            Row::Pane(i) => {
                let item = &v.items[*i];
                lines.push(item_line(item, *i == v.selected, *i >= v.ai_count));
            }
        }
    }

    let needs_you = v
        .items
        .iter()
        .take(v.ai_count)
        .filter(|i| {
            matches!(
                i.status,
                Some(Status::Waiting | Status::Blocked | Status::Error)
            )
        })
        .count();
    let hidden = v.items.len() - v.visible;
    let title = if hidden > 0 {
        format!(
            " atc — {} session(s), {needs_you} need you, {hidden} pane(s) hidden ",
            v.ai_count
        )
    } else {
        format!(" atc — {} session(s), {needs_you} need you ", v.ai_count)
    };
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        split[0],
    );

    draw_preview(f, v, split[1]);
    draw_footer(f, v, outer[1]);
    offset
}

fn draw_preview(f: &mut ratatui::Frame, v: &View, area: ratatui::layout::Rect) {
    let chosen = v.items.get(v.selected);
    let title = match chosen {
        Some(item) => match &item.repo {
            Some(repo) => format!(" {} · {repo} ", item.address),
            None => format!(" {} ", item.address),
        },
        None => " nothing to show ".to_string(),
    };
    let height = usize::from(area.height.saturating_sub(2));
    let body: Vec<Line> = v
        .preview
        .tail(height)
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                l.clone(),
                Style::default().fg(Color::Indexed(250)),
            ))
        })
        .collect();
    f.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_style(Style::default().fg(Color::Indexed(75))),
        ),
        area,
    );
}

fn draw_footer(f: &mut ratatui::Frame, v: &View, area: ratatui::layout::Rect) {
    let key = |k: &'static str| Span::styled(k, Style::default().fg(Color::Indexed(75)));
    let footer = match v.mode {
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
                v.items
                    .get(v.selected)
                    .map(|i| i.address.as_str())
                    .unwrap_or("")
            ),
            Style::default().fg(Color::Indexed(196)),
        )),
    };
    f.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::ALL)),
        area,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_goes_in_front_of_the_first_non_ai_pane() {
        let laid_out: Vec<String> = rows(2, 4)
            .iter()
            .map(|r| match r {
                Row::Heading(t) => (*t).to_string(),
                Row::Pane(i) => i.to_string(),
            })
            .collect();
        assert_eq!(laid_out, ["0", "1", "other panes", "2", "3"]);
    }

    #[test]
    fn no_heading_when_the_other_panes_are_hidden() {
        assert_eq!(rows(2, 2).len(), 2);
    }

    #[test]
    fn chosen_row_accounts_for_the_heading_above_it() {
        assert_eq!(row_of(1, 2, 4), 1);
        assert_eq!(row_of(2, 2, 4), 3);
        assert_eq!(row_of(1, 2, 2), 1);
        assert_eq!(row_of(0, 0, 3), 1);
    }

    #[test]
    fn list_does_not_move_while_the_chosen_row_is_in_view() {
        assert_eq!(scroll_offset(20, 10, 5, 0), 0);
        assert_eq!(scroll_offset(20, 10, 9, 0), 0);
    }

    #[test]
    fn list_follows_the_chosen_row_off_either_edge() {
        assert_eq!(scroll_offset(20, 10, 10, 0), 1);
        assert_eq!(scroll_offset(20, 10, 3, 8), 3);
    }

    #[test]
    fn list_never_scrolls_past_the_last_row() {
        assert_eq!(scroll_offset(12, 10, 11, 9), 2);
        assert_eq!(scroll_offset(4, 10, 3, 0), 0);
    }

    #[test]
    fn zero_height_list_has_no_offset() {
        assert_eq!(scroll_offset(20, 0, 15, 7), 0);
    }

    #[test]
    fn preview_shows_the_end_of_a_long_screen() {
        let mut p = Preview::blank();
        p.lines = (0..10).map(|i| i.to_string()).collect();
        assert_eq!(p.tail(3), ["7", "8", "9"]);
        assert_eq!(p.tail(0).len(), 0);
        assert_eq!(p.tail(99).len(), 10);
    }
}
