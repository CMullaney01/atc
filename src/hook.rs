//! Decides what one Claude Code hook event means for the pane it came from.
//! Reads and writes nothing, so the event rules can be tested on their own.
use crate::state::{self, PaneState, Status};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct HookInput {
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) session_title: Option<String>,
    #[serde(default)]
    pub(crate) tool_name: Option<String>,
    #[serde(default)]
    pub(crate) last_assistant_message: Option<String>,
    /// Present only when the hook fired inside a subagent.
    #[serde(default)]
    pub(crate) agent_id: Option<String>,
    /// The subagent's display name, such as `Explore`. Can be empty for the
    /// agents Claude Code runs internally.
    #[serde(default)]
    pub(crate) agent_type: Option<String>,
    /// Which alert this is, for the agents that have no matchers to pass one.
    #[serde(default)]
    pub(crate) notification_type: Option<String>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Update(Box<PaneState>),
    Remove,
    Ignore,
}

fn first_line(msg: &str) -> String {
    let line = msg
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > 200 {
        line.chars().take(199).chain("…".chars()).collect()
    } else {
        line.to_string()
    }
}

/// Maps a Claude Code hook event onto a pane's state.
///
/// `kind` is the Notification matcher, passed as an argument rather than read
/// from the payload so the mapping never depends on an undocumented field.
pub(crate) fn apply(
    event: &str,
    kind: Option<&str>,
    input: &HookInput,
    pane_id: &str,
    prev: Option<PaneState>,
) -> Outcome {
    // These two are about a subagent rather than from inside one, so they are
    // read before the guard below, which would otherwise swallow them.
    if matches!(event, "SubagentStart" | "SubagentStop") {
        let Some(prev) = prev else {
            return Outcome::Ignore; // nothing known about this pane yet
        };
        let Some(agent_id) = input.agent_id.clone() else {
            return Outcome::Ignore;
        };
        let mut st = prev;
        match event {
            "SubagentStart" => {
                if !st.subagents.iter().any(|a| a.id == agent_id) {
                    st.subagents.push(state::Subagent {
                        id: agent_id,
                        agent_type: input.agent_type.clone(),
                        since: state::now(),
                    });
                }
            }
            _ => st.subagents.retain(|a| a.id != agent_id),
        }
        return Outcome::Update(Box::new(st));
    }

    // A subagent finishing says nothing about whether the session as a whole
    // wants input, so its Stop must never mark the pane as waiting.
    let in_subagent = input.agent_id.is_some();
    if in_subagent && !matches!(event, "PreToolUse" | "PostToolUse") {
        return Outcome::Ignore;
    }

    if event == "SessionEnd" {
        return Outcome::Remove;
    }

    let existed = prev.is_some();
    let mut st = prev.unwrap_or_else(|| PaneState::new(pane_id));
    st.pane_id = pane_id.to_string();

    if input.session_id.is_some() {
        st.session_id = input.session_id.clone();
    }
    if input.cwd.is_some() {
        st.cwd = input.cwd.clone();
    }
    if input.session_title.is_some() {
        st.title = input.session_title.clone();
    }

    let mut mark_stale = false;
    let next = match event {
        "SessionStart" => {
            st.subagents.clear();
            Some(Status::Idle)
        }
        "UserPromptSubmit" => {
            st.last_message = None;
            st.subagents.clear();
            Some(Status::Working)
        }
        "PreToolUse" | "PostToolUse" => {
            st.tool = input.tool_name.clone();
            Some(Status::Working)
        }
        "PermissionRequest" => {
            st.tool = input.tool_name.clone();
            Some(Status::Blocked)
        }
        "Stop" => {
            st.tool = None;
            st.subagents.clear();
            if let Some(msg) = &input.last_assistant_message {
                st.last_message = Some(first_line(msg));
            }
            Some(Status::Waiting)
        }
        "StopFailure" | "PostToolUseFailure" => Some(Status::Error),
        // Notification is an escalation signal only: permission_prompt lags the
        // real prompt by ~6s and idle_prompt by ~60s, so Stop and
        // PermissionRequest drive state and these just flag it as ignored.
        // Gemini has no matchers, so it names the alert in the payload instead.
        // Reading both means one arm serves every agent.
        "Notification" => match kind.or(input.notification_type.as_deref()) {
            Some("permission_prompt" | "ToolPermission") => {
                mark_stale = true;
                Some(Status::Blocked)
            }
            Some("idle_prompt") => {
                mark_stale = true;
                None
            }
            _ => return Outcome::Ignore,
        },
        _ => return Outcome::Ignore,
    };

    if let Some(s) = next {
        if st.status != s || !existed {
            st.status = s;
            st.since = state::now();
            st.stale = false;
        }
    }
    if mark_stale {
        st.stale = true;
    }

    Outcome::Update(Box::new(st))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> HookInput {
        HookInput::default()
    }

    fn updated(o: Outcome) -> PaneState {
        match o {
            Outcome::Update(st) => *st,
            other => panic!("expected an update, got {:?}", other),
        }
    }

    fn with_agent(id: &str, kind: &str) -> HookInput {
        let mut i = input();
        i.agent_id = Some(id.into());
        i.agent_type = Some(kind.into());
        i
    }

    #[test]
    fn subagents_are_tracked_while_they_run() {
        let pane = PaneState::new("%6");
        let one = updated(apply(
            "SubagentStart",
            None,
            &with_agent("a1", "Explore"),
            "%6",
            Some(pane),
        ));
        assert_eq!(one.subagents.len(), 1);
        assert_eq!(one.subagents[0].label(), "Explore");

        let two = updated(apply(
            "SubagentStart",
            None,
            &with_agent("a2", ""),
            "%6",
            Some(one.clone()),
        ));
        assert_eq!(two.subagents.len(), 2);
        assert_eq!(
            two.subagents[1].label(),
            "agent",
            "an empty type still reads"
        );

        let again = updated(apply(
            "SubagentStart",
            None,
            &with_agent("a1", "Explore"),
            "%6",
            Some(two.clone()),
        ));
        assert_eq!(
            again.subagents.len(),
            2,
            "the same agent must not count twice"
        );

        let gone = updated(apply(
            "SubagentStop",
            None,
            &with_agent("a1", "Explore"),
            "%6",
            Some(again),
        ));
        assert_eq!(gone.subagents.len(), 1);
        assert_eq!(gone.subagents[0].id, "a2");
    }

    #[test]
    fn a_turn_boundary_clears_the_subagent_set() {
        let mut pane = PaneState::new("%6");
        pane.subagents = vec![state::Subagent {
            id: "stale".into(),
            agent_type: None,
            since: 0,
        }];
        for event in ["Stop", "UserPromptSubmit", "SessionStart"] {
            let st = updated(apply(event, None, &input(), "%6", Some(pane.clone())));
            assert!(
                st.subagents.is_empty(),
                "{event} should leave nothing running"
            );
        }
    }

    #[test]
    fn a_subagent_event_for_an_unknown_pane_is_ignored() {
        let i = with_agent("a1", "Explore");
        assert_eq!(
            apply("SubagentStart", None, &i, "%6", None),
            Outcome::Ignore
        );
    }

    #[test]
    fn stop_marks_the_pane_waiting_and_keeps_the_last_line() {
        let mut i = input();
        i.last_assistant_message = Some("Fixed the 3 type errors.\n\nWant me to run tests?".into());
        let st = updated(apply("Stop", None, &i, "%6", None));
        assert_eq!(st.status, Status::Waiting);
        assert_eq!(st.last_message.as_deref(), Some("Fixed the 3 type errors."));
        assert_eq!(st.tool, None);
    }

    #[test]
    fn a_subagent_stop_never_marks_the_session_waiting() {
        let mut i = input();
        i.agent_id = Some("agent-123".into());
        i.last_assistant_message = Some("subagent done".into());
        assert_eq!(apply("Stop", None, &i, "%6", None), Outcome::Ignore);
        assert_eq!(apply("SubagentStop", None, &i, "%6", None), Outcome::Ignore);
    }

    #[test]
    fn prompt_submission_clears_a_stale_waiting_state() {
        let mut prev = PaneState::new("%6");
        prev.status = Status::Waiting;
        prev.stale = true;
        prev.last_message = Some("old".into());
        let st = updated(apply("UserPromptSubmit", None, &input(), "%6", Some(prev)));
        assert_eq!(st.status, Status::Working);
        assert!(!st.stale);
        assert_eq!(st.last_message, None);
    }

    #[test]
    fn permission_request_blocks_immediately_with_the_tool_name() {
        let mut i = input();
        i.tool_name = Some("Bash".into());
        let st = updated(apply("PermissionRequest", None, &i, "%6", None));
        assert_eq!(st.status, Status::Blocked);
        assert_eq!(st.tool.as_deref(), Some("Bash"));
    }

    #[test]
    fn idle_notification_only_escalates_and_does_not_change_status() {
        let mut prev = PaneState::new("%6");
        prev.status = Status::Waiting;
        let before = prev.since;
        let st = updated(apply(
            "Notification",
            Some("idle_prompt"),
            &input(),
            "%6",
            Some(prev),
        ));
        assert_eq!(st.status, Status::Waiting);
        assert!(st.stale);
        assert_eq!(
            st.since, before,
            "escalation must not reset the age counter"
        );
    }

    #[test]
    fn permission_notification_blocks_as_a_fallback() {
        let st = updated(apply(
            "Notification",
            Some("permission_prompt"),
            &input(),
            "%6",
            None,
        ));
        assert_eq!(st.status, Status::Blocked);
        assert!(st.stale);
    }

    #[test]
    fn session_end_removes_the_pane() {
        assert_eq!(
            apply("SessionEnd", None, &input(), "%6", None),
            Outcome::Remove
        );
    }

    #[test]
    fn unknown_events_are_ignored() {
        assert_eq!(
            apply("CwdChanged", None, &input(), "%6", None),
            Outcome::Ignore
        );
        assert_eq!(
            apply("Notification", Some("auth_success"), &input(), "%6", None),
            Outcome::Ignore
        );
    }

    #[test]
    fn repeated_working_events_do_not_reset_the_age() {
        let mut prev = PaneState::new("%6");
        prev.status = Status::Working;
        prev.since = state::now() - 30;
        let before = prev.since;
        let mut i = input();
        i.tool_name = Some("Edit".into());
        let st = updated(apply("PostToolUse", None, &i, "%6", Some(prev)));
        assert_eq!(st.since, before);
        assert_eq!(st.tool.as_deref(), Some("Edit"));
    }

    #[test]
    fn title_and_cwd_carry_forward_across_events() {
        let mut first = input();
        first.session_title = Some("rate-limits".into());
        first.cwd = Some("/Users/c/dev/atc".into());
        let st = updated(apply("SessionStart", None, &first, "%6", None));
        let st = updated(apply("Stop", None, &input(), "%6", Some(st)));
        assert_eq!(st.title.as_deref(), Some("rate-limits"));
        assert_eq!(st.cwd.as_deref(), Some("/Users/c/dev/atc"));
    }
}
