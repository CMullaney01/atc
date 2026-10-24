//! Builds the fleet row that tmux draws under the window list. tmux asks for it
//! every couple of seconds, so this only ever formats values it is handed.
use crate::pane::{self, Kind};
use crate::state::{PaneState, Status, Subagent};
use crate::tmux::{escape_format, PaneInfo};
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
/// source of truth, so panes that never fired a hook still appear and state for
/// panes that no longer exist cannot produce a ghost.
pub(crate) fn inventory<'a>(states: &'a [PaneState], panes: &'a [PaneInfo]) -> Vec<Entry<'a>> {
    let by_id: HashMap<&str, &PaneState> = states.iter().map(|s| (s.pane_id.as_str(), s)).collect();
    let mut out: Vec<Entry<'a>> = panes
        .iter()
        .map(|info| {
            let state = by_id.get(info.pane_id.as_str()).copied();
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
pub(crate) enum TreeRow {
    Session(String),
    Window { index: u32, name: String },
    Pane(usize),
}

/// One window and the panes in it, as indices into the entry list.
type WindowGroup = (u32, String, Vec<usize>);
/// One session and its windows.
type SessionGroup = (String, Vec<WindowGroup>);

/// Groups panes under the window and session they live in.
///
/// Sessions lead with the one you are attached to and windows lead with
/// whatever is most urgent, so the thing that needs you still surfaces — but
/// panes keep their on-screen order inside a window, so a session always
/// appears next to whatever it shares a window with.
pub(crate) fn tree(entries: &[Entry], current_session: Option<&str>) -> Vec<TreeRow> {
    let mut windows: BTreeMap<(String, u32), (String, Vec<usize>)> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        windows
            .entry((e.info.session.clone(), e.info.window_index))
            .or_insert_with(|| (e.info.window_name.clone(), Vec::new()))
            .1
            .push(i);
    }

    let worst = |members: &[usize]| {
        members
            .iter()
            .map(|i| entries[*i].urgency())
            .max()
            .unwrap_or(0)
    };

    let mut by_session: BTreeMap<String, Vec<WindowGroup>> = BTreeMap::new();
    for ((session, index), (name, mut members)) in windows {
        members.sort_by_key(|i| entries[*i].info.pane_index);
        by_session
            .entry(session)
            .or_default()
            .push((index, name, members));
    }

    let mut sessions: Vec<SessionGroup> = by_session.into_iter().collect();
    sessions.sort_by(|a, b| {
        let mine = |name: &str| u8::from(current_session.is_some_and(|c| c == name));
        mine(&b.0)
            .cmp(&mine(&a.0))
            .then(
                b.1.iter()
                    .map(|w| worst(&w.2))
                    .max()
                    .cmp(&a.1.iter().map(|w| worst(&w.2)).max()),
            )
            .then(a.0.cmp(&b.0))
    });

    let mut out = Vec::new();
    for (session, mut wins) in sessions {
        out.push(TreeRow::Session(session));
        wins.sort_by(|a, b| worst(&b.2).cmp(&worst(&a.2)).then(a.0.cmp(&b.0)));
        for (index, name, members) in wins {
            out.push(TreeRow::Window { index, name });
            out.extend(members.into_iter().map(TreeRow::Pane));
        }
    }
    out
}

fn styled(glyph: &str, colour: &str, stale: bool) -> String {
    let weight = if stale { ",bold" } else { "" };
    format!("#[fg={colour}{weight}]{glyph}")
}

/// How much of the workspace the fleet row covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// AI sessions only. The row exists to show which session needs attention,
    /// and a glyph for every shell would bury that.
    AiOnly,
    Everything,
}

/// The fleet row: one cluster per window, one glyph per pane.
pub(crate) fn render(entries: &[Entry], current_session: Option<&str>, scope: Scope) -> String {
    let mut groups: BTreeMap<GroupKey, (String, Vec<(u32, String)>)> = BTreeMap::new();

    for e in entries {
        if scope == Scope::AiOnly && !e.kind.is_ai() {
            continue;
        }
        let info = e.info;
        let is_other = u8::from(current_session.is_some_and(|c| c != info.session));
        let entry = groups
            .entry((is_other, info.session.clone(), info.window_index))
            .or_insert_with(|| (info.window_name.clone(), Vec::new()));
        entry
            .1
            .push((info.pane_index, styled(e.glyph(), e.colour(), e.stale())));
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

/// The aggregate glyph for one window, used for `@atc_win` on the window
/// itself. Only panes with real state count: an unreported session has no
/// urgency to contribute.
///
/// This one is drawn inside the user's own window list, so it sets no colour —
/// shape says which state, weight says whether it wants you, and both stay
/// readable whatever the status bar is styled with.
pub(crate) fn window_glyph(
    states: &[PaneState],
    panes: &[PaneInfo],
    session: &str,
    window_index: u32,
) -> Option<String> {
    let in_window: Vec<&PaneInfo> = panes
        .iter()
        .filter(|p| p.session == session && p.window_index == window_index)
        .collect();
    let worst = states
        .iter()
        .filter(|st| in_window.iter().any(|p| p.pane_id == st.pane_id))
        .max_by_key(|st| st.status.urgency())?;
    let urgent = worst.stale || worst.status.urgency() >= Status::Waiting.urgency();
    Some(match urgent {
        true => format!("#[bold]{}#[default]", worst.status.glyph()),
        false => worst.status.glyph().to_string(),
    })
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
        }
    }

    fn st(id: &str, status: Status) -> PaneState {
        let mut s = PaneState::new(id);
        s.status = status;
        s
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
        let rows = tree(&inv, Some("ws"));

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
                "pane %1",
                "pane %2",
                "window 1 atc",
                "pane %3",
            ],
            "nvim must stay beside the session it shares window 0 with"
        );
    }

    #[test]
    fn the_attached_session_comes_first_however_quiet_it_is() {
        let panes = vec![
            pane("%1", "other", 0, "zsh", 0, "2.1.308"),
            pane("%2", "mine", 0, "zsh", 0, "2.1.308"),
        ];
        let states = vec![st("%1", Status::Blocked), st("%2", Status::Idle)];
        let inv = inventory(&states, &panes);
        let rows = tree(&inv, Some("mine"));
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
        let row = render(&inv, Some("ws"), Scope::AiOnly);
        assert!(row.contains("0:nvim"), "row was: {row}");
        assert!(row.contains("1:claude"));
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
        let row = render(&inv, Some("ws"), Scope::AiOnly);
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

        let quiet = Kind::Tool.colour();
        let row = render(&inv, Some("ws"), Scope::AiOnly);
        assert!(row.contains(Status::Waiting.glyph()));
        assert_eq!(
            row.matches(quiet).count(),
            0,
            "tool/shell glyphs must not be on the default row: {row}"
        );

        let full = render(&inv, Some("ws"), Scope::Everything);
        assert!(
            full.matches(quiet).count() >= 2,
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
        let row = render(&inv, Some("ws"), Scope::AiOnly);
        assert!(row.contains('?'), "row was: {row}");
    }

    #[test]
    fn other_sessions_are_prefixed_and_sorted_after_the_current_one() {
        let panes = vec![
            pane("%1", "other", 0, "zsh", 0, "node"),
            pane("%2", "ws", 3, "claude", 0, "node"),
        ];
        let states = vec![st("%1", Status::Idle), st("%2", Status::Waiting)];
        let inv = inventory(&states, &panes);
        let row = render(&inv, Some("ws"), Scope::AiOnly);
        let ws_at = row.find("3:claude").expect("current session present");
        let other_at = row.find("[other]").expect("other session present");
        assert!(ws_at < other_at, "current session must come first: {row}");
    }

    #[test]
    fn empty_state_renders_an_empty_row() {
        assert_eq!(render(&[], Some("ws"), Scope::AiOnly), "");
    }

    #[test]
    fn stale_panes_render_bold() {
        let panes = vec![pane("%1", "ws", 0, "claude", 0, "node")];
        let mut s = st("%1", Status::Waiting);
        s.stale = true;
        let inv = inventory(std::slice::from_ref(&s), &panes);
        let row = render(&inv, Some("ws"), Scope::AiOnly);
        assert!(row.contains("bold"), "row was: {row}");
    }

    #[test]
    fn window_glyph_reports_the_most_urgent_pane() {
        let panes = vec![
            pane("%1", "ws", 0, "claude", 0, "node"),
            pane("%2", "ws", 0, "claude", 1, "node"),
        ];
        let states = vec![st("%1", Status::Working), st("%2", Status::Blocked)];
        let g = window_glyph(&states, &panes, "ws", 0).expect("a glyph");
        assert!(g.contains(Status::Blocked.glyph()), "glyph was: {g}");
    }

    #[test]
    fn window_glyph_is_none_when_nothing_is_tracked() {
        let panes = vec![pane("%1", "ws", 0, "nvim", 0, "nvim")];
        assert!(window_glyph(&[], &panes, "ws", 0).is_none());
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
