use crate::state::{PaneState, Status};
use crate::tmux::{escape_format, PaneInfo};
use std::collections::{BTreeMap, HashMap};

/// A tracked pane that still exists in tmux.
pub struct Entry<'a> {
    pub info: &'a PaneInfo,
    pub state: &'a PaneState,
}

/// Drops state for panes tmux no longer reports, so a killed pane or a crashed
/// session cannot leave a ghost glyph on the row.
pub fn live<'a>(states: &'a [PaneState], panes: &'a [PaneInfo]) -> Vec<Entry<'a>> {
    let by_id: HashMap<&str, &PaneInfo> =
        panes.iter().map(|p| (p.pane_id.as_str(), p)).collect();
    let mut out: Vec<Entry<'a>> = states
        .iter()
        .filter_map(|st| {
            by_id
                .get(st.pane_id.as_str())
                .map(|info| Entry { info, state: st })
        })
        .collect();
    out.sort_by(|a, b| {
        b.state
            .status
            .urgency()
            .cmp(&a.state.status.urgency())
            .then(b.state.stale.cmp(&a.state.stale))
            .then(b.state.since.cmp(&a.state.since))
    });
    out
}

fn styled(status: Status, stale: bool) -> String {
    let weight = if stale { ",bold" } else { "" };
    format!("#[fg={}{}]{}", status.colour(), weight, status.glyph())
}

/// The fleet row: one cluster per window, one glyph per tracked pane.
pub fn render(states: &[PaneState], panes: &[PaneInfo], current_session: Option<&str>) -> String {
    let by_id: HashMap<&str, &PaneInfo> =
        panes.iter().map(|p| (p.pane_id.as_str(), p)).collect();

    // (is_other_session, session, window_index) -> (window_name, panes)
    let mut groups: BTreeMap<(u8, String, u32), (String, Vec<(u32, Status, bool)>)> =
        BTreeMap::new();

    for st in states {
        let Some(info) = by_id.get(st.pane_id.as_str()) else {
            continue;
        };
        let is_other = u8::from(current_session.is_some_and(|c| c != info.session));
        let entry = groups
            .entry((is_other, info.session.clone(), info.window_index))
            .or_insert_with(|| (info.window_name.clone(), Vec::new()));
        entry.1.push((info.pane_index, st.status, st.stale));
    }

    let mut out = String::new();
    let mut prev_session: Option<String> = None;
    for ((is_other, session, window_index), (window_name, mut members)) in groups {
        if let Some(prev) = &prev_session {
            let gap = if *prev == session { "  " } else { " #[fg=colour238]│#[default] " };
            out.push_str(gap);
        }
        if is_other == 1 {
            out.push_str(&format!("#[fg=colour244][{}]#[default] ", escape_format(&session)));
        }
        out.push_str(&format!(
            "#[fg=colour250]{}:{}#[default] ",
            window_index,
            escape_format(&window_name)
        ));
        members.sort_by_key(|(idx, _, _)| *idx);
        for (_, status, stale) in &members {
            out.push_str(&styled(*status, *stale));
        }
        out.push_str("#[default]");
        prev_session = Some(session);
    }
    out
}

/// The aggregate glyph for one window, used for `@atc_win` on the window itself.
pub fn window_glyph(
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
    Some(styled(worst.status, worst.stale) + "#[default]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, session: &str, w: u32, wname: &str, p: u32) -> PaneInfo {
        PaneInfo {
            pane_id: id.to_string(),
            session: session.to_string(),
            window_index: w,
            window_name: wname.to_string(),
            pane_index: p,
            active: false,
            title: String::new(),
            path: "/tmp".to_string(),
        }
    }

    fn st(id: &str, status: Status) -> PaneState {
        let mut s = PaneState::new(id);
        s.status = status;
        s
    }

    #[test]
    fn groups_panes_under_their_window() {
        let panes = vec![
            pane("%1", "ws", 0, "nvim", 1),
            pane("%2", "ws", 0, "nvim", 2),
            pane("%3", "ws", 1, "claude", 0),
        ];
        let states = vec![
            st("%1", Status::Waiting),
            st("%2", Status::Working),
            st("%3", Status::Waiting),
        ];
        let row = render(&states, &panes, Some("ws"));
        assert!(row.contains("0:nvim"), "row was: {row}");
        assert!(row.contains("1:claude"));
        // window 0 carries both of its panes' glyphs, in pane order
        let zero = row.split("1:claude").next().unwrap();
        assert!(zero.contains(Status::Waiting.glyph()));
        assert!(zero.contains(Status::Working.glyph()));
    }

    #[test]
    fn drops_ghosts_for_panes_tmux_no_longer_reports() {
        let panes = vec![pane("%1", "ws", 0, "nvim", 1)];
        let states = vec![st("%1", Status::Waiting), st("%99", Status::Blocked)];
        let row = render(&states, &panes, Some("ws"));
        assert!(!row.contains(Status::Blocked.glyph()), "row was: {row}");
        assert_eq!(live(&states, &panes).len(), 1);
    }

    #[test]
    fn other_sessions_are_prefixed_and_sorted_after_the_current_one() {
        let panes = vec![
            pane("%1", "other", 0, "zsh", 0),
            pane("%2", "ws", 3, "claude", 0),
        ];
        let states = vec![st("%1", Status::Idle), st("%2", Status::Waiting)];
        let row = render(&states, &panes, Some("ws"));
        let ws_at = row.find("3:claude").expect("current session present");
        let other_at = row.find("[other]").expect("other session present");
        assert!(ws_at < other_at, "current session must come first: {row}");
    }

    #[test]
    fn empty_state_renders_an_empty_row() {
        assert_eq!(render(&[], &[], Some("ws")), "");
    }

    #[test]
    fn stale_panes_render_bold() {
        let panes = vec![pane("%1", "ws", 0, "claude", 0)];
        let mut s = st("%1", Status::Waiting);
        s.stale = true;
        let row = render(&[s], &panes, Some("ws"));
        assert!(row.contains("bold"), "row was: {row}");
    }

    #[test]
    fn window_glyph_reports_the_most_urgent_pane() {
        let panes = vec![
            pane("%1", "ws", 0, "claude", 0),
            pane("%2", "ws", 0, "claude", 1),
        ];
        let states = vec![st("%1", Status::Working), st("%2", Status::Blocked)];
        let g = window_glyph(&states, &panes, "ws", 0).expect("a glyph");
        assert!(g.contains(Status::Blocked.glyph()), "glyph was: {g}");
    }

    #[test]
    fn window_glyph_is_none_when_nothing_is_tracked() {
        let panes = vec![pane("%1", "ws", 0, "nvim", 0)];
        assert!(window_glyph(&[], &panes, "ws", 0).is_none());
    }

    #[test]
    fn live_sorts_most_urgent_first() {
        let panes = vec![
            pane("%1", "ws", 0, "a", 0),
            pane("%2", "ws", 1, "b", 0),
            pane("%3", "ws", 2, "c", 0),
        ];
        let states = vec![
            st("%1", Status::Working),
            st("%2", Status::Error),
            st("%3", Status::Waiting),
        ];
        let ordered: Vec<&str> = live(&states, &panes)
            .iter()
            .map(|e| e.state.pane_id.as_str())
            .collect();
        assert_eq!(ordered, vec!["%2", "%3", "%1"]);
    }
}
