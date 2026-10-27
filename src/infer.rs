//! Reads a session's state off its own screen, for the sessions that have never
//! fired a hook. A guess is better than a shrug here, but only just, so this is
//! never allowed to overwrite anything a hook reported.
use crate::agent::Tool;
use crate::bar::Entry;
use crate::pane::Kind;
use crate::state::{self, Status};
use crate::tmux;

/// Capturing a screen costs a tmux round trip, so a workspace full of
/// unreported panes cannot be allowed to make the status row slow.
const MAX_CAPTURES: usize = 12;

/// A line offering a numbered choice, however the tool marks the selected one.
/// On its own this means nothing — a picker looks exactly the same — so it only
/// ever corroborates a question.
fn after_prompt(line: &str) -> &str {
    line.trim_start().trim_start_matches(['❯', '›', '>', ' '])
}

fn is_numbered_option(line: &str) -> bool {
    after_prompt(line)
        .split_once(". ")
        .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// What a pane's screen says its agent is doing, or `None` when it shows
/// nothing this tool is known to print.
///
/// The markers are each tool's current interface rather than a contract, so a
/// redesign there turns the answer back into `None` — the old behaviour, not a
/// wrong one. A tool with no markers at all always answers `None`.
pub(crate) fn from_screen(lines: &[String], tool: Tool) -> Option<Status> {
    let m = tool.markers();
    let has = |needles: &[&str]| {
        !needles.is_empty() && lines.iter().any(|l| needles.iter().any(|n| l.contains(n)))
    };

    // Asking outranks working: the interrupt hint can still be on screen
    // underneath a question.
    if has(m.questions) && lines.iter().any(|l| is_numbered_option(l)) {
        return Some(Status::Blocked);
    }
    if has(m.working) {
        return Some(Status::Working);
    }
    // An input box means it has stopped and wants a reply. The same arrow heads
    // a selected menu item, so a numbered option rules the line out.
    // Some tools mark the box with an arrow and nothing else, so the arrow
    // itself is the marker; others print a placeholder after one, and which
    // arrow they draw is not worth depending on. Either way of matching counts.
    let waiting = !m.input.is_empty()
        && lines.iter().any(|l| {
            let t = l.trim_start();
            let is_box = m
                .input
                .iter()
                .any(|p| t.starts_with(p) || after_prompt(t).starts_with(p));
            is_box && !is_numbered_option(t)
        });
    if waiting {
        return Some(Status::Waiting);
    }
    None
}

/// What a pane's last screen check concluded, and when.
///
/// Written as `<seconds>|<status>`, with the status left empty for a screen
/// that said nothing recognisable — which is a different thing from never
/// having looked, and has to survive as its own answer.
fn read_look(raw: &str) -> Option<(u64, Option<Status>)> {
    let (at, status) = raw.split_once('|')?;
    Some((at.parse().ok()?, Status::parse(status)))
}

fn write_look(at: u64, status: Option<Status>) -> String {
    format!("{at}|{}", status.map(Status::label).unwrap_or(""))
}

/// When this pane was last looked at, treating one never looked at as the
/// oldest there is so it goes to the front of the queue.
fn looked_at(raw: &str) -> u64 {
    read_look(raw).map_or(0, |(at, _)| at)
}

/// Which panes to spend this pass's screen captures on: the ones looked at
/// longest ago.
///
/// The bug this replaces took the first twelve every time and started again
/// from the beginning on the next pass, so a thirteenth unreported pane was
/// never looked at once, however long it sat there. Going by how stale each
/// one is means every pane comes round, and raising the budget is not what
/// stands between a fleet and the truth.
fn stalest_first(entries: &[Entry], budget: usize) -> Vec<usize> {
    let mut due: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| needs_a_guess(e))
        .map(|(i, _)| i)
        .collect();
    due.sort_by_key(|i| looked_at(&entries[*i].info.infer));
    due.truncate(budget);
    due
}

fn needs_a_guess(e: &Entry) -> bool {
    e.kind == Kind::Unreported && e.state.is_none()
}

/// Fills in a status for the panes that look like AI sessions but have reported
/// nothing. Panes with real state are left alone.
///
/// Capturing a screen costs a tmux round trip, so only a few panes are looked
/// at per pass. The rest keep the answer from the last time they were looked
/// at, which is why a bounded budget does not turn into a row of question
/// marks on a fleet bigger than the budget.
pub(crate) fn decorate(entries: &mut [Entry]) {
    for e in entries.iter_mut() {
        if needs_a_guess(e) {
            e.inferred = read_look(&e.info.infer).and_then(|(_, status)| status);
        }
    }

    let now = state::now();
    let mut looked: Vec<(String, String)> = Vec::new();
    for i in stalest_first(entries, MAX_CAPTURES) {
        let e = &mut entries[i];
        let Some(tool) = e.tool() else { continue };
        e.inferred = from_screen(&tmux::capture_pane(&e.info.pane_id), tool);
        // The time has to be written even when the answer has not changed, or
        // this pane stays the stalest one and the pass never moves on.
        looked.push((e.info.pane_id.clone(), write_look(now, e.inferred)));
    }
    tmux::set_pane_option_on_each("@atc_infer", &looked);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    fn unreported(pane_id: &str, infer: &str) -> crate::tmux::PaneInfo {
        crate::tmux::PaneInfo {
            pane_id: pane_id.to_string(),
            session: "ws".to_string(),
            window_index: 0,
            window_name: "w".to_string(),
            pane_index: 0,
            active: false,
            window_active: false,
            session_attached: false,
            title: String::new(),
            command: "codex".to_string(),
            path: "/tmp".to_string(),
            tty: "ttys001".to_string(),
            argv: String::new(),
            infer: infer.to_string(),
        }
    }

    fn entries(infos: &[crate::tmux::PaneInfo]) -> Vec<Entry<'_>> {
        infos
            .iter()
            .map(|info| Entry {
                info,
                state: None,
                kind: Kind::Unreported,
                inferred: None,
            })
            .collect()
    }

    /// The bug this guards left a pane showing `?` for as long as it existed.
    /// With fifteen unreported panes and a budget of twelve, the last three
    /// were never looked at once: every pass started from the beginning.
    #[test]
    fn a_pane_past_the_budget_still_comes_round() {
        let infos: Vec<_> = (0..15)
            .map(|i| unreported(&format!("%{i}"), "100|"))
            .collect();
        let inv = entries(&infos);

        let first = stalest_first(&inv, MAX_CAPTURES);
        assert_eq!(first.len(), MAX_CAPTURES);
        assert!(
            !first.contains(&14),
            "the last pane loses the first pass, which is fine"
        );

        // What the next pass sees once this one has written its times.
        let after: Vec<_> = (0..15)
            .map(|i| {
                let looked = if first.contains(&i) { "200|" } else { "100|" };
                unreported(&format!("%{i}"), looked)
            })
            .collect();
        let second = stalest_first(&entries(&after), MAX_CAPTURES);
        for late in [12, 13, 14] {
            assert!(
                second.contains(&late),
                "pane {late} waited a pass, so it must lead the next one"
            );
        }
    }

    #[test]
    fn a_pane_nobody_has_looked_at_goes_first() {
        let infos = [
            unreported("%1", "500|working"),
            unreported("%2", ""),
            unreported("%3", "100|blocked"),
        ];
        assert_eq!(
            stalest_first(&entries(&infos), 2),
            vec![1, 2],
            "never looked at beats looked at long ago, which beats looked at recently"
        );
    }

    #[test]
    fn a_pane_waiting_its_turn_keeps_the_last_answer() {
        assert_eq!(read_look("100|blocked"), Some((100, Some(Status::Blocked))));
        assert_eq!(
            read_look("100|"),
            Some((100, None)),
            "a screen that said nothing recognisable is an answer, not a gap"
        );
        assert_eq!(read_look(""), None, "nobody has looked at this one");
        assert_eq!(read_look("nonsense"), None);
    }

    #[test]
    fn a_look_survives_being_written_down_and_read_back() {
        for status in [Some(Status::Blocked), Some(Status::Working), None] {
            let raw = write_look(1_793_100_000, status);
            assert_eq!(read_look(&raw), Some((1_793_100_000, status)), "{raw}");
        }
    }

    #[test]
    fn an_empty_codex_composer_means_it_is_waiting() {
        let s = screen("› Ask Codex to do anything");
        assert_eq!(from_screen(&s, Tool::Codex), Some(Status::Waiting));
    }

    #[test]
    fn a_working_codex_is_not_read_as_waiting() {
        let s = screen(
            "• Working (12s • esc to interrupt)\n\
             › Ask Codex to do anything",
        );
        assert_eq!(
            from_screen(&s, Tool::Codex),
            Some(Status::Working),
            "the composer stays on screen while it works, so working must win"
        );
    }

    #[test]
    fn a_spinner_and_the_interrupt_hint_mean_working() {
        let s = screen(
            "✻ Wandering… (7m 56s · ↓ 30.0k tokens)\n\
             ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt",
        );
        assert_eq!(from_screen(&s, Tool::Claude), Some(Status::Working));
    }

    #[test]
    fn codex_shares_the_interrupt_hint() {
        // Its status line, quoted from codex-rs/tui.
        let s = screen("• Working (12s • esc to interrupt)");
        assert_eq!(from_screen(&s, Tool::Codex), Some(Status::Working));
    }

    #[test]
    fn a_permission_prompt_means_blocked() {
        let s = screen(
            "Bash(rm -rf build)\n\
             Do you want to proceed?\n\
             ❯ 1. Yes\n  2. No",
        );
        assert_eq!(from_screen(&s, Tool::Claude), Some(Status::Blocked));
    }

    #[test]
    fn each_agent_is_asked_about_in_its_own_words() {
        let gemini = screen("Apply this change?\n ❯ 1. Yes, allow once\n  2. No");
        assert_eq!(from_screen(&gemini, Tool::Gemini), Some(Status::Blocked));
        assert_eq!(
            from_screen(&gemini, Tool::Codex),
            None,
            "codex does not ask this, so it must not match"
        );

        let codex = screen("shell: rm -rf build\n › 1. Yes, proceed\n   2. No");
        assert_eq!(from_screen(&codex, Tool::Codex), Some(Status::Blocked));
    }

    #[test]
    fn a_question_outranks_a_leftover_interrupt_hint() {
        let s = screen(
            "esc to interrupt\n\
             Do you trust the files in this folder?\n\
             ❯ 1. Yes, proceed\n  2. No, exit",
        );
        assert_eq!(from_screen(&s, Tool::Claude), Some(Status::Blocked));
    }

    #[test]
    fn a_numbered_menu_is_not_a_permission_prompt() {
        // Seen for real: a shell showing a leftover picker was called blocked.
        let s = screen(
            "  ❯ 1. Verify + merge PR #1\n\
               2. Start the next branch\n\
             Enter to select · Esc to cancel\n\
             ➜",
        );
        assert_eq!(from_screen(&s, Tool::Claude), None);
    }

    #[test]
    fn a_question_with_no_options_is_just_prose() {
        let s = screen("I was going to ask: Do you want to proceed with the refactor?");
        assert_eq!(from_screen(&s, Tool::Claude), None);
    }

    #[test]
    fn a_bare_input_box_means_it_wants_a_reply() {
        let s = screen("Tests pass. Want me to push?\n\n❯\n");
        assert_eq!(from_screen(&s, Tool::Claude), Some(Status::Waiting));
    }

    #[test]
    fn only_claude_has_a_known_input_box() {
        let s = screen("something\n\n❯\n");
        for t in [Tool::Codex, Tool::Gemini, Tool::Copilot] {
            assert_eq!(from_screen(&s, t), None, "{t} has no verified input marker");
        }
    }

    #[test]
    fn copilot_never_guesses() {
        for text in [
            "esc to interrupt",
            "Do you want to proceed?\n❯ 1. Yes",
            "❯",
            "",
        ] {
            assert_eq!(from_screen(&screen(text), Tool::Copilot), None);
        }
    }

    #[test]
    fn an_unrecognised_screen_stays_unknown() {
        assert_eq!(
            from_screen(&screen("$ ls -la\ntotal 0\n"), Tool::Claude),
            None
        );
        assert_eq!(from_screen(&[], Tool::Claude), None);
    }

    #[test]
    fn numbered_options_survive_whatever_marks_the_selection() {
        for line in ["❯ 1. Yes", "› 1. Yes", "  2. No", "> 10. Something"] {
            assert!(is_numbered_option(line), "{line:?}");
        }
        for line in ["❯", "Yes, proceed", "1.No space", "esc to interrupt"] {
            assert!(!is_numbered_option(line), "{line:?}");
        }
    }
}
