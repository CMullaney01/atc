//! The popup you bring up to see every session at once, laid out the way your
//! tmux actually is, with a live view of whichever one is selected so you can
//! read it without going there. It owns the terminal while it is open, so it
//! puts the screen back before it moves the tmux client.
use crate::agent::Tool;
use crate::bar::TreeRow;
use crate::pane::Kind;
use crate::state::{self, Status, Subagent};
use crate::{bar, infer, tmux};
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
    inferred: bool,
    subagents: Vec<Subagent>,
    tool: Option<Tool>,
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

    fn age_label(&self) -> String {
        match self.age {
            Some(secs) => state::fmt_age(secs),
            None if self.inferred => "~".to_string(),
            None => "-".to_string(),
        }
    }
}

enum Mode {
    Browse,
    Prompt(String),
    ConfirmKill,
}

/// One drawn line. Only `Pane` can be selected — the rest are structure, and a
/// subagent is not a pane, so jumping to or killing one means nothing.
enum Row {
    Session(String),
    Window { index: u32, name: String },
    Pane(usize),
    Subagent { parent: usize, agent: usize },
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

/// Turns the workspace order into drawn lines, dropping panes that are not AI
/// sessions when they are hidden, along with any window or session left empty.
fn build_rows(items: &[Item], order: &[TreeRow], show_others: bool) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    for row in order {
        match row {
            TreeRow::Session(name) => {
                drop_empty_session(&mut out);
                out.push(Row::Session(name.clone()));
            }
            TreeRow::Window { index, name } => {
                drop_empty_window(&mut out);
                out.push(Row::Window {
                    index: *index,
                    name: name.clone(),
                });
            }
            TreeRow::Pane(i) => {
                let item = &items[*i];
                if !show_others && !item.kind.is_ai() {
                    continue;
                }
                out.push(Row::Pane(*i));
                for agent in 0..item.subagents.len() {
                    out.push(Row::Subagent { parent: *i, agent });
                }
            }
        }
    }
    drop_empty_session(&mut out);
    out
}

/// A window heading that gained no panes under it should not be drawn.
fn drop_empty_window(out: &mut Vec<Row>) {
    if matches!(out.last(), Some(Row::Window { .. })) {
        out.pop();
    }
}

/// Nor should a session heading that gained no windows.
fn drop_empty_session(out: &mut Vec<Row>) {
    drop_empty_window(out);
    if matches!(out.last(), Some(Row::Session(_))) {
        out.pop();
    }
}

/// The item indices that can be selected, in the order they are drawn.
fn selectable(rows: &[Row]) -> Vec<usize> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Pane(i) => Some(*i),
            _ => None,
        })
        .collect()
}

/// Which drawn line the cursor is sitting on, so the viewport can keep it in
/// view regardless of how much structure is above it.
fn row_of(rows: &[Row], item: Option<usize>) -> usize {
    let Some(item) = item else { return 0 };
    rows.iter()
        .position(|r| matches!(r, Row::Pane(i) if *i == item))
        .unwrap_or(0)
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

fn snapshot(cache: &mut HashMap<String, Option<String>>) -> (Vec<Item>, Vec<TreeRow>, usize) {
    let states = state::all();
    let panes = tmux::list_panes();
    let mut inv = bar::inventory(&states, &panes);
    infer::decorate(&mut inv);
    let order = bar::tree(&inv, tmux::current_session().as_deref());
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
            inferred: e.is_inferred(),
            subagents: e.subagents().to_vec(),
            tool: e.tool(),
        })
        .collect();
    (items, order, ai_count)
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
    let (mut items, mut order, mut ai_count) = snapshot(&mut cache);
    let mut cursor: usize = 0;
    let mut offset: usize = 0;
    let mut show_others = true;
    let mut mode = Mode::Browse;
    let mut preview = Preview::blank();
    let mut last_refresh = Instant::now();
    let mut jump: Option<(String, String, u32)> = None;

    loop {
        let rows = build_rows(&items, &order, show_others);
        let pick = selectable(&rows);
        cursor = cursor.min(pick.len().saturating_sub(1));
        let chosen = pick.get(cursor).copied();
        preview.follow(chosen.map(|i| items[i].pane_id.as_str()));

        terminal.draw(|f| {
            let view = View {
                items: &items,
                rows: &rows,
                chosen,
                ai_count,
                hidden: items.len() - pick.len(),
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
                            if let Some(item) = chosen.map(|i| &items[i]) {
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
                            if let Some(item) = chosen.map(|i| &items[i]) {
                                tmux::kill_pane(&item.pane_id);
                                let _ = state::remove(&item.pane_id);
                            }
                            (items, order, ai_count) = snapshot(&mut cache);
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
                            if !pick.is_empty() {
                                cursor = (cursor + 1).min(pick.len() - 1);
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            cursor = cursor.saturating_sub(1);
                        }
                        KeyCode::Char('g') => cursor = 0,
                        KeyCode::Char('G') => cursor = pick.len().saturating_sub(1),
                        KeyCode::Tab => show_others = !show_others,
                        KeyCode::Char('r') => {
                            cache.clear();
                            (items, order, ai_count) = snapshot(&mut cache);
                        }
                        KeyCode::Char('p') => {
                            // Only an AI session can take a prompt.
                            if chosen.is_some_and(|i| items[i].kind.is_ai()) {
                                mode = Mode::Prompt(String::new());
                            }
                        }
                        KeyCode::Char('x') => {
                            if chosen.is_some() {
                                mode = Mode::ConfirmKill;
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(item) = chosen.map(|i| &items[i]) {
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
            (items, order, ai_count) = snapshot(&mut cache);
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

fn item_line(item: &Item, chosen: bool) -> Line<'static> {
    let dim = !item.kind.is_ai();
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
        Span::styled(if chosen { "  ▌" } else { "   " }, base),
        Span::styled(
            if item.active { "▸" } else { " " },
            base.fg(Color::Indexed(250)),
        ),
        Span::styled(item.glyph(), glyph_style),
        Span::styled(
            if item.inferred { "~ " } else { "  " },
            base.fg(Color::Indexed(244)),
        ),
        Span::styled(format!("{:<24} ", crop(&item.title, 24)), base),
        Span::styled(
            format!("{:<8} ", item.tool.map(|t| t.id()).unwrap_or("")),
            base.fg(Color::Indexed(109)),
        ),
        Span::styled(
            format!("{:<11} ", item.state_label()),
            Style::default().fg(item.colour()),
        ),
        Span::styled(
            format!("{:>5}  ", item.age_label()),
            base.fg(Color::Indexed(244)),
        ),
        Span::styled(
            item.repo.clone().unwrap_or_default(),
            base.fg(Color::Indexed(244)),
        ),
    ])
}

fn subagent_line(agent: &Subagent) -> Line<'static> {
    Line::from(vec![
        Span::raw("         "),
        Span::styled("⑂ ", Style::default().fg(Color::Indexed(109))),
        Span::styled(
            format!("{:<22} ", crop(agent.label(), 22)),
            Style::default().fg(Color::Indexed(109)),
        ),
        Span::styled(
            format!("{:>5}", state::fmt_age(agent.age_secs())),
            Style::default().fg(Color::Indexed(244)),
        ),
    ])
}

struct View<'a> {
    items: &'a [Item],
    rows: &'a [Row],
    chosen: Option<usize>,
    ai_count: usize,
    hidden: usize,
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

    let empty_notice = usize::from(v.ai_count == 0);
    let list_height = usize::from(split[0].height.saturating_sub(2)).saturating_sub(empty_notice);
    let offset = scroll_offset(v.rows.len(), list_height, row_of(v.rows, v.chosen), offset);

    let mut lines: Vec<Line> = Vec::new();
    if v.ai_count == 0 {
        lines.push(Line::from(Span::styled(
            "  no AI sessions — start a Claude session in a tmux pane",
            Style::default().fg(Color::Indexed(244)),
        )));
    }
    for row in v.rows.iter().skip(offset).take(list_height) {
        lines.push(match row {
            Row::Session(name) => Line::from(Span::styled(
                format!(" {name}"),
                Style::default()
                    .fg(Color::Indexed(250))
                    .add_modifier(Modifier::BOLD),
            )),
            Row::Window { index, name } => Line::from(Span::styled(
                format!("   {index} {name}"),
                Style::default().fg(Color::Indexed(240)),
            )),
            Row::Pane(i) => item_line(&v.items[*i], v.chosen == Some(*i)),
            Row::Subagent { parent, agent } => subagent_line(&v.items[*parent].subagents[*agent]),
        });
    }

    let needs_you = v
        .items
        .iter()
        .filter(|i| {
            i.kind.is_ai()
                && matches!(
                    i.status,
                    Some(Status::Waiting | Status::Blocked | Status::Error)
                )
        })
        .count();
    let title = if v.hidden > 0 {
        format!(
            " atc — {} session(s), {needs_you} need you, {} pane(s) hidden ",
            v.ai_count, v.hidden
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
    let chosen = v.chosen.map(|i| &v.items[i]);
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
        .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(Color::Reset))))
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
                v.chosen.map(|i| v.items[i].address.as_str()).unwrap_or("")
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

    fn item(title: &str, kind: Kind, agents: usize) -> Item {
        Item {
            pane_id: format!("%{title}"),
            session: "ws".to_string(),
            window_index: 0,
            address: "ws:0.0".to_string(),
            title: title.to_string(),
            kind,
            status: None,
            stale: false,
            age: None,
            repo: None,
            active: false,
            inferred: false,
            tool: matches!(kind, Kind::Session | Kind::Unreported).then_some(Tool::Claude),
            subagents: (0..agents)
                .map(|i| Subagent {
                    id: format!("a{i}"),
                    agent_type: Some("Explore".to_string()),
                    since: 0,
                })
                .collect(),
        }
    }

    fn order() -> Vec<TreeRow> {
        vec![
            TreeRow::Session("ws".to_string()),
            TreeRow::Window {
                index: 0,
                name: "editor".to_string(),
            },
            TreeRow::Pane(0),
            TreeRow::Pane(1),
        ]
    }

    #[test]
    fn subagents_are_drawn_under_their_pane_but_never_selectable() {
        let items = vec![
            item("claude", Kind::Session, 2),
            item("zsh", Kind::Shell, 0),
        ];
        let rows = build_rows(&items, &order(), true);
        assert!(matches!(rows[2], Row::Pane(0)));
        assert!(matches!(rows[3], Row::Subagent { parent: 0, .. }));
        assert!(matches!(rows[4], Row::Subagent { parent: 0, .. }));
        assert!(matches!(rows[5], Row::Pane(1)));
        assert_eq!(selectable(&rows), vec![0, 1]);
    }

    #[test]
    fn hiding_other_panes_drops_the_window_it_empties() {
        let items = vec![item("nvim", Kind::Tool, 0)];
        let only = vec![
            TreeRow::Session("ws".to_string()),
            TreeRow::Window {
                index: 0,
                name: "editor".to_string(),
            },
            TreeRow::Pane(0),
        ];
        assert_eq!(build_rows(&items, &only, true).len(), 3);
        assert!(
            build_rows(&items, &only, false).is_empty(),
            "a session with nothing left in it should not be drawn"
        );
    }

    #[test]
    fn the_cursor_row_accounts_for_structure_and_subagents() {
        let items = vec![
            item("claude", Kind::Session, 2),
            item("zsh", Kind::Shell, 0),
        ];
        let rows = build_rows(&items, &order(), true);
        assert_eq!(row_of(&rows, Some(0)), 2);
        assert_eq!(row_of(&rows, Some(1)), 5);
        assert_eq!(row_of(&rows, None), 0);
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

    #[test]
    fn an_inferred_state_shows_no_age() {
        let mut it = item("claude", Kind::Unreported, 0);
        it.inferred = true;
        assert_eq!(it.age_label(), "~");
        it.inferred = false;
        assert_eq!(it.age_label(), "-");
    }
}
