//! Reads a session's state off its own screen, for the sessions that have never
//! fired a hook. A guess is better than a shrug here, but only just, so this is
//! never allowed to overwrite anything a hook reported.
use crate::bar::Entry;
use crate::pane::Kind;
use crate::state::Status;
use crate::tmux;

/// Capturing a screen costs a tmux round trip, so a workspace full of
/// unreported panes cannot be allowed to make the status row slow.
const MAX_CAPTURES: usize = 12;

/// What the screen of a Claude Code session says it is doing, or `None` when it
/// shows nothing recognisable.
///
/// These markers are Claude Code's current interface rather than a contract, so
/// a redesign there turns the answer back into `None` — which is the old
/// behaviour, not a wrong answer.
pub(crate) fn from_screen(lines: &[String]) -> Option<Status> {
    // Both halves are required. A numbered list on its own is just as likely
    // to be a menu, and guessing "blocked" off one is worse than not guessing.
    let question = lines
        .iter()
        .any(|l| l.contains("Do you want") || l.contains("Do you trust"));
    let options = lines.iter().any(|l| {
        let t = l.trim_start_matches(['❯', ' ']);
        t.starts_with("1. Yes") || t.starts_with("2. No")
    });
    if question && options {
        return Some(Status::Blocked);
    }
    if lines.iter().any(|l| l.contains("esc to interrupt")) {
        return Some(Status::Working);
    }
    // The same arrow heads both the input box and a selected menu item, so a
    // numbered option after it rules the line out.
    let has_input_box = lines.iter().any(|l| {
        let t = l.trim_start();
        let rest = t.trim_start_matches(['❯', ' ']);
        let numbered = rest
            .split_once(". ")
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        (t.starts_with('❯') || t.starts_with("│ >")) && !numbered
    });
    if has_input_box {
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
        e.inferred = from_screen(&tmux::capture_pane(&e.info.pane_id));
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
        assert_eq!(from_screen(&s), Some(Status::Working));
    }

    #[test]
    fn a_permission_prompt_means_blocked() {
        let s = screen(
            "Bash(rm -rf build)\n\
             Do you want to proceed?\n\
             ❯ 1. Yes\n  2. No",
        );
        assert_eq!(from_screen(&s), Some(Status::Blocked));
    }

    #[test]
    fn a_question_outranks_a_leftover_interrupt_hint() {
        let s = screen(
            "esc to interrupt\n\
             Do you trust the files in this folder?\n\
             ❯ 1. Yes, proceed\n  2. No, exit",
        );
        assert_eq!(from_screen(&s), Some(Status::Blocked));
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
        assert_eq!(from_screen(&s), None);
    }

    #[test]
    fn a_bare_input_box_means_it_wants_a_reply() {
        let s = screen("Tests pass. Want me to push?\n\n❯\n");
        assert_eq!(from_screen(&s), Some(Status::Waiting));
    }

    #[test]
    fn an_unrecognised_screen_stays_unknown() {
        assert_eq!(from_screen(&screen("$ ls -la\ntotal 0\n")), None);
        assert_eq!(from_screen(&[]), None);
    }
}
