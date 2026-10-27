//! Builds the fleet row that tmux draws under the window list. tmux asks for it
//! every couple of seconds, so this only ever formats values it is handed.
use crate::agent::Tool;
use crate::pane::{self, Kind};
use crate::state::{PaneState, Status, Subagent};
use crate::tmux::{self, escape_format, PaneInfo};
use std::collections::{BTreeMap, HashMap};

/// A live tmux pane, with hook state laid over it when there is any.
pub(crate) struct Entry<'a> {
    pub(crate) info: &'a PaneInfo,
    pub(crate) state: Option<&'a PaneState>,
    pub(crate) kind: Kind,
    /// Worked out from the pane's screen because no hook ever reported it. Only
    /// ever consulted when there is no real state to use.
    pub(crate) inferred: Option<Status>,
}

impl Entry<'_> {
    pub(crate) fn status(&self) -> Option<Status> {
        self.state.map(|s| s.status).or(self.inferred)
    }

    /// True when the status above is a guess off the screen rather than
    /// something the session said.
    pub(crate) fn is_inferred(&self) -> bool {
        self.state.is_none() && self.inferred.is_some()
    }

    /// Which agent this is: what the hook said if one has reported, otherwise
    /// what the pane looks like. `None` for a pane that is not an agent.
    pub(crate) fn tool(&self) -> Option<Tool> {
        self.state
            .and_then(|s| Tool::parse(&s.source))
            .or_else(|| pane::detect_tool(self.info))
    }

    pub(crate) fn subagents(&self) -> &[Subagent] {
        self.state.map(|s| s.subagents.as_slice()).unwrap_or(&[])
    }

    pub(crate) fn stale(&self) -> bool {
        self.state.is_some_and(|s| s.stale)
    }

    pub(crate) fn age_secs(&self) -> u64 {
        self.state.map(|s| s.age_secs()).unwrap_or(0)
    }

    pub(crate) fn glyph(&self) -> &'static str {
        match self.status() {
            Some(s) => s.glyph(),
            None => self.kind.glyph(),
        }
    }

    pub(crate) fn colour(&self) -> &'static str {
        match self.status() {
            Some(s) => s.colour(),
            None => self.kind.colour(),
        }
    }

    /// Hooked sessions carry their own title; everything else is better
    /// described by what is actually running in it.
    pub(crate) fn title(&self) -> String {
        if !self.kind.is_ai() {
            return if self.info.command.is_empty() {
                self.info.window_name.clone()
            } else {
                self.info.command.clone()
            };
        }
        if let Some(t) = self.state.and_then(|s| s.title.as_deref()) {
            let cleaned = crate::tmux::clean_title(t);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
        let cleaned = crate::tmux::clean_title(&self.info.title);
        if !cleaned.is_empty() {
            return cleaned;
        }
        self.info.window_name.clone()
    }

    fn urgency(&self) -> u8 {
        self.status().map(|s| s.urgency()).unwrap_or(0)
    }
}

/// Every pane tmux reports, classified, most urgent first. The pane list is the
/// source of truth, so panes that never fired a hook still appear, state for
/// panes that no longer exist cannot produce a ghost, and neither can state a
/// pane has outlived.
///
/// Outlived state is left on disk rather than deleted. A pane can report
/// something other than its agent for a moment — a pager the agent opened
/// holds the terminal — and a glyph that comes back on its own is better than
/// a session whose state was thrown away.
pub(crate) fn inventory<'a>(states: &'a [PaneState], panes: &'a [PaneInfo]) -> Vec<Entry<'a>> {
    let by_id: HashMap<&str, &PaneState> = states.iter().map(|s| (s.pane_id.as_str(), s)).collect();
    let mut out: Vec<Entry<'a>> = panes
        .iter()
        .map(|info| {
            let state = by_id
                .get(info.pane_id.as_str())
                .copied()
                .filter(|_| pane::could_be_running_an_agent(info));
            Entry {
                info,
                state,
                kind: pane::classify(info, state),
                inferred: None,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.kind
            .is_ai()
            .cmp(&a.kind.is_ai())
            .then(b.urgency().cmp(&a.urgency()))
            .then(b.stale().cmp(&a.stale()))
            // Among equals, whatever has been sitting longest comes first.
            .then(b.age_secs().cmp(&a.age_secs()))
            .then(a.info.session.cmp(&b.info.session))
            .then(a.info.window_index.cmp(&b.info.window_index))
            .then(a.info.pane_index.cmp(&b.info.pane_index))
    });
    out
}

/// Other sessions sort after the current one, then by window.
type GroupKey = (u8, String, u32);

/// Back to the status bar's own text colour, leaving its background alone.
/// Only the foreground is reset, so the row stays part of the bar.
const PLAIN: &str = "#[fg=default]";

/// Context, in descending order of how much it should draw the eye. These sit
/// on the status bar, which is light by default, so they run dark; the glyphs
/// carry the state and nothing structural competes with them.
const WINDOW: &str = "default";
const OTHER_SESSION: &str = "colour238";
const SEPARATOR: &str = "colour240";

/// One line of the workspace as it is actually laid out. Flat rather than
/// nested so that a caller can walk it straight into rows, and shared so the
/// popup and `atc status` cannot drift apart.
#[derive(Debug)]
pub(crate) enum TreeRow {
    Session(String),
    Window { index: u32, name: String },
    Pane(usize),
}

/// One window and the panes in it, as indices into the entry list.
type WindowGroup = (u32, String, Vec<usize>);
/// One session and its windows.
type SessionGroup = (String, Vec<WindowGroup>);

/// Whether one of atc's tmux options has been turned off. Only an explicit
/// refusal counts, so an option nobody has set behaves the way it always has,
/// and every option answers to the same words.
fn switched_off(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("off" | "0" | "no" | "false"))
}

/// Whether the fleet is listed with whatever wants you first, or in the order
/// tmux itself holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    Triage,
    AsTmux,
}

impl Order {
    pub(crate) fn from_tmux() -> Self {
        Self::read(tmux::global_option("@atc_triage").as_deref())
    }

    fn read(value: Option<&str>) -> Self {
        if switched_off(value) {
            Order::AsTmux
        } else {
            Order::Triage
        }
    }
}

/// Groups panes under the window and session they live in.
///
/// Under triage the same rule runs at all three levels: sessions lead with
/// the one you are attached to, windows lead with whatever is most urgent,
/// and a window's panes lead with the agents. Turned off, everything keeps
/// tmux's own order and nothing moves on its own.
pub(crate) fn tree(entries: &[Entry], current_session: Option<&str>, order: Order) -> Vec<TreeRow> {
    let mut sessions = group_by_window(entries, order);
    order_sessions(&mut sessions, entries, current_session, order);

    let mut out = Vec::new();
    for (session, mut windows) in sessions {
        out.push(TreeRow::Session(session));
        windows.sort_by(|a, b| match order {
            Order::Triage => worst_of(entries, &b.2)
                .cmp(&worst_of(entries, &a.2))
                .then(a.0.cmp(&b.0)),
            Order::AsTmux => a.0.cmp(&b.0),
        });
        for (index, name, members) in windows {
            out.push(TreeRow::Window { index, name });
            out.extend(members.into_iter().map(TreeRow::Pane));
        }
    }
    out
}

/// How much the most urgent pane in a group wants you.
fn worst_of(entries: &[Entry], members: &[usize]) -> u8 {
    members
        .iter()
        .map(|i| entries[*i].urgency())
        .max()
        .unwrap_or(0)
}

/// Panes collected under their window, and windows under their session.
///
/// Under triage a window's panes keep the order the inventory put them in,
/// which leads with the agents, so a session is never buried under the
/// editor and the shells it shares a window with. Turned off they go back to
/// the order they sit on screen.
fn group_by_window(entries: &[Entry], order: Order) -> Vec<SessionGroup> {
    let mut windows: BTreeMap<(String, u32), (String, Vec<usize>)> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        windows
            .entry((e.info.session.clone(), e.info.window_index))
            .or_insert_with(|| (e.info.window_name.clone(), Vec::new()))
            .1
            .push(i);
    }

    let mut by_session: BTreeMap<String, Vec<WindowGroup>> = BTreeMap::new();
    for ((session, index), (name, mut members)) in windows {
        if order == Order::AsTmux {
            members.sort_by_key(|i| entries[*i].info.pane_index);
        }
        by_session
            .entry(session)
            .or_default()
            .push((index, name, members));
    }
    by_session.into_iter().collect()
}

/// The session you are attached to leads, then whichever holds the most urgent
/// pane, then by name so the order never wobbles between redraws.
fn order_sessions(
    sessions: &mut [SessionGroup],
    entries: &[Entry],
    current: Option<&str>,
    order: Order,
) {
    if order == Order::AsTmux {
        sessions.sort_by(|a, b| a.0.cmp(&b.0));
        return;
    }
    sessions.sort_by(|a, b| {
        let mine = |name: &str| u8::from(current.is_some_and(|c| c == name));
        let worst_in = |windows: &Vec<WindowGroup>| {
            windows
                .iter()
                .map(|w| worst_of(entries, &w.2))
                .max()
                .unwrap_or(0)
        };
        mine(&b.0)
            .cmp(&mine(&a.0))
            .then(worst_in(&b.1).cmp(&worst_in(&a.1)))
            .then(a.0.cmp(&b.0))
    });
}

/// Marks a state worked out from a pane's screen rather than reported by a
/// hook, wherever one is shown.
pub(crate) const INFERRED_MARK: &str = "~";

/// Whether the bar paints its own glyphs or leaves them in the status bar's
/// colours. Off is a useful fallback for a bar whose styling the palette fights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Paint {
    Colour,
    Mono,
}

impl Paint {
    pub(crate) fn from_tmux() -> Self {
        Self::read(tmux::global_option("@atc_colour").as_deref())
    }

    fn read(value: Option<&str>) -> Self {
        if switched_off(value) {
            Paint::Mono
        } else {
            Paint::Colour
        }
    }

    /// One glyph, dressed, and self-contained: whatever it turns on it turns
    /// back off, so glyphs can be concatenated without bleeding into each other.
    /// Weight carries urgency either way, so turning colour off loses the
    /// state's hue but not the fact that it wants you.
    fn apply(self, glyph: &str, colour: &str, stale: bool) -> String {
        match self {
            Paint::Colour => {
                let weight = if stale { ",bold" } else { "" };
                format!("#[fg={colour}{weight}]{glyph}#[default]")
            }
            Paint::Mono if stale => format!("#[bold]{glyph}#[default]"),
            Paint::Mono => glyph.to_string(),
        }
    }
}

/// How much of the workspace the fleet row covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// AI sessions only. The row exists to show which session needs attention,
    /// and a glyph for every shell would bury that.
    AiOnly,
    Everything,
}

/// How the row should be drawn.
pub(crate) struct RowOpts {
    pub(crate) scope: Scope,
    pub(crate) paint: Paint,
}

/// The fleet row: one cluster per window, one glyph per pane.
pub(crate) fn render(entries: &[Entry], current_session: Option<&str>, opts: RowOpts) -> String {
    let mut groups: BTreeMap<GroupKey, (String, Vec<(u32, String)>)> = BTreeMap::new();

    for e in entries {
        if opts.scope == Scope::AiOnly && !e.kind.is_ai() {
            continue;
        }
        let info = e.info;
        let is_other = u8::from(current_session.is_some_and(|c| c != info.session));
        let entry = groups
            .entry((is_other, info.session.clone(), info.window_index))
            .or_insert_with(|| (info.window_name.clone(), Vec::new()));
        entry.1.push((
            info.pane_index,
            opts.paint.apply(e.glyph(), e.colour(), e.stale()),
        ));
    }

    let mut out = String::new();
    let mut prev_session: Option<String> = None;
    for ((is_other, session, window_index), (window_name, mut members)) in groups {
        if let Some(prev) = &prev_session {
            let gap = if *prev == session {
                "  ".to_string()
            } else {
                format!(" #[fg={SEPARATOR}]│{PLAIN} ")
            };
            out.push_str(&gap);
        }
        if is_other == 1 {
            out.push_str(&format!(
                "#[fg={OTHER_SESSION}][{}]{PLAIN} ",
                escape_format(&session)
            ));
        }
        out.push_str(&format!(
            "#[fg={WINDOW}]{}:{}{PLAIN} ",
            window_index,
            escape_format(&window_name)
        ));
        members.sort_by_key(|(idx, _)| *idx);
        for (_, glyph) in &members {
            out.push_str(glyph);
        }
        out.push_str(PLAIN);
        prev_session = Some(session);
    }
    out
}

/// Every AI pane in one window, as one glyph each in the order they sit on
/// screen. This is what `@atc_win` holds, so tmux can draw it inside the window
/// name for nothing.
///
/// Non-AI panes stay out for the same reason they stay off the row: a glyph for
/// every shell buries the one you are looking for. Returns `None` when a window
/// holds no AI pane at all, so the option is unset rather than left blank.
pub(crate) fn window_glyphs(
    entries: &[Entry],
    session: &str,
    window_index: u32,
    paint: Paint,
) -> Option<String> {
    let mut members: Vec<&Entry> = entries
        .iter()
        .filter(|e| {
            e.kind.is_ai() && e.info.session == session && e.info.window_index == window_index
        })
        .collect();
    if members.is_empty() {
        return None;
    }
    members.sort_by_key(|e| e.info.pane_index);
    Some(
        members
            .iter()
            .map(|e| paint.apply(e.glyph(), e.colour(), e.stale()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, session: &str, w: u32, wname: &str, p: u32, cmd: &str) -> PaneInfo {
        PaneInfo {
            pane_id: id.to_string(),
            session: session.to_string(),
            window_index: w,
            window_name: wname.to_string(),
            pane_index: p,
            active: false,
            window_active: false,
            session_attached: false,
            title: String::new(),
            command: cmd.to_string(),
            path: "/tmp".to_string(),
            tty: "ttys001".to_string(),
            argv: String::new(),
        }
    }

    /// The row with every style directive stripped, so a test about what is
    /// drawn does not break when the palette changes. A palette change broke
    /// one of these once.
    fn visible(row: &str) -> String {
        let mut out = String::new();
        let mut rest = row;
        while let Some(open) = rest.find("#[") {
            out.push_str(&rest[..open]);
            match rest[open..].find(']') {
                Some(close) => rest = &rest[open + close + 1..],
                None => break,
            }
        }
        out.push_str(rest);
        out
    }

    /// Whether a window's cluster holds a glyph for a quiet, non-session pane.
    fn counts_quiet_panes(row: &str) -> usize {
        row.matches(Kind::Tool.colour()).count()
    }

    fn ai_only() -> RowOpts {
        RowOpts {
            scope: Scope::AiOnly,
            paint: Paint::Colour,
        }
    }

    fn everything() -> RowOpts {
        RowOpts {
            scope: Scope::Everything,
            paint: Paint::Colour,
        }
    }

    fn st(id: &str, status: Status) -> PaneState {
        let mut s = PaneState::new(id);
        s.status = status;
        s
    }

    /// The bug this guards put a blocked glyph on a pane that had been
    /// running an unrelated command for a day: the agent exited without a
    /// last hook, so nothing ever took its state away.
    #[test]
    fn a_pane_that_outlived_its_agent_shows_what_it_is_now() {
        let panes = vec![
            pane("%1", "ws", 0, "w", 0, "caffeinate"),
            pane("%2", "ws", 0, "w", 1, "2.1.308"),
        ];
        let states = vec![st("%1", Status::Blocked), st("%2", Status::Blocked)];
        let inv = inventory(&states, &panes);

        let outlived = inv.iter().find(|e| e.info.pane_id == "%1").unwrap();
        assert_eq!(outlived.kind, Kind::Job);
        assert_eq!(
            outlived.status(),
            None,
            "a leftover state file must not speak for a pane that moved on"
        );

        let live = inv.iter().find(|e| e.info.pane_id == "%2").unwrap();
        assert_eq!(live.kind, Kind::Session);
        assert_eq!(live.status(), Some(Status::Blocked), "this one is real");
    }

    #[test]
    fn the_tree_keeps_a_window_together_and_floats_the_urgent_one_up() {
        let panes = vec![
            pane("%1", "ws", 0, "editor", 0, "nvim"),
            pane("%2", "ws", 0, "editor", 1, "2.1.308"),
            pane("%3", "ws", 1, "atc", 0, "2.1.308"),
        ];
        let states = vec![st("%3", Status::Working), st("%2", Status::Blocked)];
        let inv = inventory(&states, &panes);
        let rows = tree(&inv, Some("ws"), Order::Triage);

        let shape: Vec<String> = rows
            .iter()
            .map(|r| match r {
                TreeRow::Session(n) => format!("session {n}"),
                TreeRow::Window { index, name } => format!("window {index} {name}"),
                TreeRow::Pane(i) => format!("pane {}", inv[*i].info.pane_id),
            })
            .collect();

        assert_eq!(
            shape,
            [
                "session ws",
                // window 0 holds the blocked pane, so it leads
                "window 0 editor",
                "pane %2",
                "pane %1",
                "window 1 atc",
                "pane %3",
            ],
            "nvim must stay in window 0, under the session it shares it with"
        );
    }

    #[test]
    fn a_window_leads_with_its_agents_and_not_with_the_editor() {
        let panes = vec![
            pane("%1", "ws", 0, "editor", 0, "nvim"),
            pane("%2", "ws", 0, "editor", 1, "zsh"),
            pane("%3", "ws", 0, "editor", 2, "zsh"),
            pane("%4", "ws", 0, "editor", 3, "2.1.308"),
        ];
        let states = vec![st("%4", Status::Blocked)];
        let inv = inventory(&states, &panes);

        let panes_of = |rows: Vec<TreeRow>| -> Vec<String> {
            rows.iter()
                .filter_map(|r| match r {
                    TreeRow::Pane(i) => Some(inv[*i].info.pane_id.clone()),
                    _ => None,
                })
                .collect()
        };

        assert_eq!(
            panes_of(tree(&inv, Some("ws"), Order::Triage)),
            ["%4", "%1", "%2", "%3"],
            "the session wanting you leads the window it is buried in"
        );
        assert_eq!(
            panes_of(tree(&inv, Some("ws"), Order::AsTmux)),
            ["%1", "%2", "%3", "%4"],
            "triage off keeps the panes where tmux put them"
        );
    }

    #[test]
    fn triage_off_leaves_the_urgent_window_where_tmux_put_it() {
        let panes = vec![
            pane("%1", "ws", 0, "editor", 0, "2.1.308"),
            pane("%2", "ws", 1, "atc", 0, "2.1.308"),
        ];
        let states = vec![st("%2", Status::Blocked)];
        let inv = inventory(&states, &panes);

        let triaged = tree(&inv, Some("ws"), Order::Triage);
        let as_tmux = tree(&inv, Some("ws"), Order::AsTmux);

        let leads = |rows: &[TreeRow]| match &rows[1] {
            TreeRow::Window { index, .. } => *index,
            other => panic!("a session is followed by a window, got {other:?}"),
        };
        assert_eq!(leads(&triaged), 1, "the blocked window rises under triage");
        assert_eq!(leads(&as_tmux), 0, "nothing moves with triage off");
    }

    #[test]
    fn the_attached_session_comes_first_however_quiet_it_is() {
        let panes = vec![
            pane("%1", "other", 0, "zsh", 0, "2.1.308"),
            pane("%2", "mine", 0, "zsh", 0, "2.1.308"),
        ];
        let states = vec![st("%1", Status::Blocked), st("%2", Status::Idle)];
        let inv = inventory(&states, &panes);
        let rows = tree(&inv, Some("mine"), Order::Triage);
        match &rows[0] {
            TreeRow::Session(n) => assert_eq!(n, "mine"),
            _ => panic!("a session heading should lead"),
        }
    }

    #[test]
    fn an_inferred_status_is_used_but_flagged() {
        let panes = [pane("%1", "ws", 0, "claude", 0, "2.1.308")];
        let mut inv = inventory(&[], &panes);
        assert_eq!(inv[0].status(), None);
        assert!(!inv[0].is_inferred());

        inv[0].inferred = Some(Status::Waiting);
        assert_eq!(inv[0].status(), Some(Status::Waiting));
        assert!(inv[0].is_inferred());
        assert_eq!(inv[0].glyph(), Status::Waiting.glyph());
    }

    #[test]
    fn groups_panes_under_their_window() {
        let panes = vec![
            pane("%1", "ws", 0, "nvim", 1, "node"),
            pane("%2", "ws", 0, "nvim", 2, "node"),
            pane("%3", "ws", 1, "claude", 0, "node"),
        ];
        let states = vec![
            st("%1", Status::Waiting),
            st("%2", Status::Working),
            st("%3", Status::Waiting),
        ];
        let inv = inventory(&states, &panes);
        let row = render(&inv, Some("ws"), ai_only());
        let seen = visible(&row);
        assert!(seen.contains("0:nvim"), "row was: {seen}");
        assert!(seen.contains("1:claude"), "row was: {seen}");
        let zero = row.split("1:claude").next().unwrap();
        assert!(zero.contains(Status::Waiting.glyph()));
        assert!(zero.contains(Status::Working.glyph()));
    }

    #[test]
    fn state_for_a_pane_tmux_no_longer_reports_is_dropped() {
        let panes = vec![pane("%1", "ws", 0, "nvim", 1, "node")];
        let states = vec![st("%1", Status::Waiting), st("%99", Status::Blocked)];
        let inv = inventory(&states, &panes);
        assert_eq!(inv.len(), 1, "only live panes appear");
        let row = render(&inv, Some("ws"), ai_only());
        assert!(!row.contains(Status::Blocked.glyph()), "row was: {row}");
    }

    #[test]
    fn non_ai_panes_stay_off_the_default_row_but_appear_with_all() {
        let panes = vec![
            pane("%1", "ws", 0, "editor", 0, "nvim"),
            pane("%2", "ws", 0, "editor", 1, "zsh"),
            pane("%3", "ws", 0, "editor", 2, "node"),
        ];
        let states = vec![st("%3", Status::Waiting)];
        let inv = inventory(&states, &panes);

        let row = render(&inv, Some("ws"), ai_only());
        assert!(row.contains(Status::Waiting.glyph()));
        assert_eq!(
            counts_quiet_panes(&row),
            0,
            "tool/shell glyphs must not be on the default row: {row}"
        );

        let full = render(&inv, Some("ws"), everything());
        assert!(
            counts_quiet_panes(&full) >= 2,
            "nvim and zsh should both show with --all: {full}"
        );
    }

    #[test]
    fn an_unreported_session_still_reaches_the_row() {
        let mut p = pane("%1", "ws", 0, "claude", 0, "2.1.308");
        p.title = "✳ fix-lint".to_string();
        let panes = [p];
        let inv = inventory(&[], &panes);
        assert_eq!(inv[0].kind, Kind::Unreported);
        let row = render(&inv, Some("ws"), ai_only());
        assert!(visible(&row).contains('?'), "row was: {row}");
    }

    #[test]
    fn other_sessions_are_prefixed_and_sorted_after_the_current_one() {
        let panes = vec![
            pane("%1", "other", 0, "zsh", 0, "node"),
            pane("%2", "ws", 3, "claude", 0, "node"),
        ];
        let states = vec![st("%1", Status::Idle), st("%2", Status::Waiting)];
        let inv = inventory(&states, &panes);
        let row = render(&inv, Some("ws"), ai_only());
        let ws_at = row.find("3:claude").expect("current session present");
        let other_at = row.find("[other]").expect("other session present");
        assert!(ws_at < other_at, "current session must come first: {row}");
    }

    #[test]
    fn empty_state_renders_an_empty_row() {
        assert_eq!(render(&[], Some("ws"), ai_only()), "");
    }

    #[test]
    fn stale_panes_render_bold() {
        let panes = vec![pane("%1", "ws", 0, "claude", 0, "node")];
        let mut s = st("%1", Status::Waiting);
        s.stale = true;
        let inv = inventory(std::slice::from_ref(&s), &panes);
        let row = render(&inv, Some("ws"), ai_only());
        // Weight is the escalation signal, so asserting on it is the point.
        assert!(row.contains("bold"), "row was: {row}");
    }

    #[test]
    fn a_reported_agent_outranks_what_the_pane_looks_like() {
        // The command says claude; the hook that reported said gemini.
        let panes = [pane("%1", "ws", 0, "claude", 0, "2.1.308")];
        let mut s = st("%1", Status::Working);
        s.source = "gemini".to_string();
        let inv = inventory(std::slice::from_ref(&s), &panes);
        assert_eq!(inv[0].tool(), Some(Tool::Gemini));

        // With nothing reported, detection answers instead.
        let bare = inventory(&[], &panes);
        assert_eq!(bare[0].tool(), Some(Tool::Claude));

        let shell = [pane("%2", "ws", 0, "ws", 0, "zsh")];
        assert_eq!(inventory(&[], &shell)[0].tool(), None);
    }

    #[test]
    fn window_glyphs_follow_the_order_the_panes_sit_in() {
        let panes = vec![
            pane("%2", "ws", 0, "claude", 1, "2.1.308"),
            pane("%1", "ws", 0, "claude", 0, "2.1.308"),
        ];
        let states = vec![st("%1", Status::Working), st("%2", Status::Blocked)];
        let inv = inventory(&states, &panes);
        let g = window_glyphs(&inv, "ws", 0, Paint::Colour).expect("a cluster");
        let working = g.find(Status::Working.glyph()).expect("working present");
        let blocked = g.find(Status::Blocked.glyph()).expect("blocked present");
        assert!(working < blocked, "pane 0 before pane 1, got: {g}");
    }

    #[test]
    fn window_glyphs_leave_out_everything_that_is_not_a_session() {
        let panes = vec![
            pane("%1", "ws", 0, "editor", 0, "nvim"),
            pane("%2", "ws", 0, "editor", 1, "zsh"),
            pane("%3", "ws", 0, "editor", 2, "cargo"),
            pane("%4", "ws", 0, "editor", 3, "2.1.308"),
        ];
        let states = vec![st("%4", Status::Waiting)];
        let inv = inventory(&states, &panes);
        let g = window_glyphs(&inv, "ws", 0, Paint::Colour).expect("a cluster");
        assert_eq!(
            g.matches(Status::Waiting.glyph()).count(),
            1,
            "only the session should appear, got: {g}"
        );
        assert!(!g.contains(Kind::Tool.colour()), "nvim leaked in: {g}");
        assert!(!g.contains(Kind::Job.colour()), "cargo leaked in: {g}");
    }

    #[test]
    fn a_window_with_no_session_in_it_has_no_cluster() {
        let panes = [pane("%1", "ws", 0, "editor", 0, "nvim")];
        let inv = inventory(&[], &panes);
        assert!(window_glyphs(&inv, "ws", 0, Paint::Colour).is_none());
        assert!(window_glyphs(&inv, "ws", 9, Paint::Colour).is_none());
    }

    #[test]
    fn a_session_known_only_from_its_screen_still_reaches_the_window() {
        let panes = [pane("%1", "ws", 0, "claude", 0, "2.1.308")];
        let mut inv = inventory(&[], &panes);
        inv[0].inferred = Some(Status::Waiting);
        let g = window_glyphs(&inv, "ws", 0, Paint::Colour).expect("a cluster");
        assert!(g.contains(Status::Waiting.glyph()), "glyph was: {g}");
    }

    #[test]
    fn turning_colour_off_keeps_the_glyph_and_the_weight() {
        let panes = [pane("%1", "ws", 0, "claude", 0, "2.1.308")];
        let mut s = st("%1", Status::Blocked);
        s.stale = true;
        let inv = inventory(std::slice::from_ref(&s), &panes);

        let colour = window_glyphs(&inv, "ws", 0, Paint::Colour).expect("a cluster");
        assert!(colour.contains("#[fg="), "should be painted: {colour}");

        let mono = window_glyphs(&inv, "ws", 0, Paint::Mono).expect("a cluster");
        assert!(!mono.contains("#[fg="), "should set no colour: {mono}");
        assert!(mono.contains(Status::Blocked.glyph()), "glyph lost: {mono}");
        assert!(
            mono.contains("bold"),
            "urgency lost with the colour: {mono}"
        );
    }

    /// Both options are read the same way on purpose: an option that honoured
    /// "0" while its neighbour only honoured "off" would be a trap.
    #[test]
    fn only_an_explicit_refusal_turns_an_option_off() {
        for off in ["off", "0", "no", "false", " off "] {
            assert_eq!(Paint::read(Some(off)), Paint::Mono, "{off:?}");
            assert_eq!(Order::read(Some(off)), Order::AsTmux, "{off:?}");
        }
        for on in [None, Some(""), Some("on"), Some("yes"), Some("anything")] {
            assert_eq!(Paint::read(on), Paint::Colour, "{on:?}");
            assert_eq!(Order::read(on), Order::Triage, "{on:?}");
        }
    }

    #[test]
    fn a_non_ai_pane_is_titled_by_what_is_running() {
        let mut p = pane("%1", "ws", 0, "editor", 0, "nvim");
        p.title = "conormullaney@MacBookPro:~".to_string();
        let panes = [p];
        let inv = inventory(&[], &panes);
        assert_eq!(
            inv[0].title(),
            "nvim",
            "prefer the command over a shell prompt title"
        );
    }

    #[test]
    fn an_ai_pane_is_titled_by_its_session_title() {
        let mut p = pane("%1", "ws", 0, "claude", 0, "2.1.308");
        p.title = "✳ fix-pyright-lint-errors".to_string();
        let mut s = st("%1", Status::Waiting);
        s.title = Some("✳ renamed-task".to_string());
        let panes = [p];
        let inv = inventory(std::slice::from_ref(&s), &panes);
        assert_eq!(inv[0].title(), "renamed-task");
    }

    #[test]
    fn inventory_puts_ai_sessions_before_other_panes() {
        let panes = vec![
            pane("%1", "ws", 0, "a", 0, "nvim"),
            pane("%2", "ws", 1, "b", 0, "node"),
            pane("%3", "ws", 2, "c", 0, "node"),
        ];
        let states = vec![st("%2", Status::Working), st("%3", Status::Error)];
        let ordered: Vec<&str> = inventory(&states, &panes)
            .iter()
            .map(|e| e.info.pane_id.as_str())
            .collect();
        assert_eq!(ordered, vec!["%3", "%2", "%1"]);
    }

    #[test]
    fn equal_urgency_puts_the_longest_waiting_first() {
        let panes = vec![
            pane("%1", "ws", 0, "a", 0, "node"),
            pane("%2", "ws", 1, "b", 0, "node"),
        ];
        let mut fresh = st("%1", Status::Waiting);
        fresh.since = crate::state::now();
        let mut old = st("%2", Status::Waiting);
        old.since = crate::state::now() - 600;
        let states = vec![fresh, old];
        let ordered: Vec<&str> = inventory(&states, &panes)
            .iter()
            .map(|e| e.info.pane_id.as_str())
            .collect();
        assert_eq!(ordered, vec!["%2", "%1"]);
    }
}
