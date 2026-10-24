//! Reads a session's state off its own screen, for the sessions that have never
//! fired a hook. A guess is better than a shrug here, but only just, so this is
//! never allowed to overwrite anything a hook reported.
use crate::agent::Tool;
use crate::bar::Entry;
use crate::pane::Kind;
use crate::state::Status;
use crate::tmux;

/// Capturing a screen costs a tmux round trip, so a workspace full of
/// unreported panes cannot be allowed to make the status row slow.
const MAX_CAPTURES: usize = 12;

/// A line offering a numbered choice, however the tool marks the selected one.
/// On its own this means nothing — a picker looks exactly the same — so it only
/// ever corroborates a question.
fn is_numbered_option(line: &str) -> bool {
    let rest = line.trim_start().trim_start_matches(['❯', '›', '>', ' ']);
    rest.split_once(". ")
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
    let waiting = !m.input.is_empty()
        && lines.iter().any(|l| {
            let t = l.trim_start();
            m.input.iter().any(|p| t.starts_with(p)) && !is_numbered_option(t)
        });
    if waiting {
        return Some(Status::Waiting);
    }
    None
}

/// Fills in a status for the panes that look like AI sessions but have reported
/// nothing. Panes with real state are left alone.
pub(crate) fn decorate(entries: &mut [Entry]) {
    let mut budget = MAX_CAPTURES;
    for e in entries.iter_mut() {
        if budget == 0 {
            return;
        }
        if e.kind != Kind::Unreported || e.state.is_some() {
            continue;
        }
        budget -= 1;
        let Some(tool) = e.tool() else { continue };
        e.inferred = from_screen(&tmux::capture_pane(&e.info.pane_id), tool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
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
