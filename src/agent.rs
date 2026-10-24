//! The agent CLIs atc knows about, and everything that differs between them.
//! Adding another one should be a match arm per method here and nothing else.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Claude,
    Codex,
    Gemini,
    Copilot,
}

/// Whether a tool's screen markers have been checked against a real screen.
/// `atc doctor` reports this, because a guess that looks like a fact is the
/// thing most likely to mislead you.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confidence {
    /// Seen working against a live session.
    Seen,
    /// Taken from the tool's own source, never run.
    FromSource,
    /// Nothing to go on.
    None,
}

/// What to look for on a pane's screen when no hook has reported it.
pub(crate) struct Markers {
    /// Any one of these means the agent is working.
    pub(crate) working: &'static [&'static str],
    /// One of these plus a numbered option on screen means it is asking.
    /// A question alone is prose; options alone are a menu.
    pub(crate) questions: &'static [&'static str],
    /// The start of its input box, which means it is waiting for a reply.
    pub(crate) input: &'static [&'static str],
    pub(crate) confidence: Confidence,
}

impl Tool {
    pub(crate) const ALL: [Tool; 4] = [Tool::Claude, Tool::Codex, Tool::Gemini, Tool::Copilot];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Tool::Claude => "claude",
            Tool::Codex => "codex",
            Tool::Gemini => "gemini",
            Tool::Copilot => "copilot",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.id() == name)
    }

    /// The process names tmux reports for it. Claude Code has none worth
    /// matching: its binary is named after its version, so it is recognised by
    /// that shape and by the mark it puts in the pane title instead.
    pub(crate) fn commands(self) -> &'static [&'static str] {
        match self {
            Tool::Claude => &[],
            Tool::Codex => &["codex"],
            Tool::Gemini => &["gemini"],
            Tool::Copilot => &["copilot"],
        }
    }

    /// Maps this tool's name for an event onto the one `hook::apply` matches.
    /// `None` for events atc has no use for, which the caller ignores.
    pub(crate) fn normalise(self, event: &str) -> Option<&'static str> {
        // Everything already canonical passes through for every tool: codex
        // uses these names natively and copilot accepts them as aliases.
        const CANONICAL: &[&str] = &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PostToolUseFailure",
            "PermissionRequest",
            "Stop",
            "StopFailure",
            "SessionEnd",
            "Notification",
            "SubagentStart",
            "SubagentStop",
        ];
        if let Some(hit) = CANONICAL.iter().find(|c| **c == event) {
            return Some(hit);
        }
        match self {
            Tool::Claude | Tool::Codex => None,
            Tool::Copilot => match event {
                "sessionStart" => Some("SessionStart"),
                "userPromptSubmitted" => Some("UserPromptSubmit"),
                "preToolUse" => Some("PreToolUse"),
                "postToolUse" => Some("PostToolUse"),
                "postToolUseFailure" => Some("PostToolUseFailure"),
                "permissionRequest" => Some("PermissionRequest"),
                "agentStop" => Some("Stop"),
                "errorOccurred" => Some("StopFailure"),
                "sessionEnd" => Some("SessionEnd"),
                "notification" => Some("Notification"),
                "subagentStart" => Some("SubagentStart"),
                "subagentStop" => Some("SubagentStop"),
                _ => None,
            },
            Tool::Gemini => match event {
                "BeforeAgent" => Some("UserPromptSubmit"),
                "BeforeTool" => Some("PreToolUse"),
                "AfterTool" => Some("PostToolUse"),
                // Gemini is reported to fire this at the end of every turn,
                // including a pause for a tool call. The tool event that
                // follows puts the pane back to working, so it corrects itself.
                "AfterAgent" => Some("Stop"),
                _ => None,
            },
        }
    }

    /// Markers are quoted from each tool's own source. The published docs do
    /// not carry them, and inventing them is how a confident wrong answer gets
    /// made, so a tool with nothing to quote gets nothing.
    pub(crate) fn markers(self) -> Markers {
        match self {
            Tool::Claude => Markers {
                working: &["esc to interrupt"],
                questions: &["Do you want", "Do you trust"],
                input: &["❯", "│ >"],
                confidence: Confidence::Seen,
            },
            // Its status line reads "• Working (0s • esc to interrupt)", and
            // "Yes, proceed" is the first option of its approval overlay.
            Tool::Codex => Markers {
                working: &["esc to interrupt"],
                questions: &["Yes, proceed"],
                input: &[],
                confidence: Confidence::FromSource,
            },
            // The three questions its tool-confirmation component can ask.
            Tool::Gemini => Markers {
                working: &[],
                questions: &[
                    "Apply this change?",
                    "Do you want to proceed?",
                    "Allow execution of MCP tool",
                ],
                input: &[],
                confidence: Confidence::FromSource,
            },
            // Closed source, and its docs publish no screen text. Its hooks
            // cover every state, so there is nothing to guess at.
            Tool::Copilot => Markers {
                working: &[],
                questions: &[],
                input: &[],
                confidence: Confidence::None,
            },
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_round_trips() {
        for t in Tool::ALL {
            assert_eq!(Tool::parse(t.id()), Some(t));
        }
        assert_eq!(Tool::parse("aider"), None);
        assert_eq!(Tool::parse(""), None);
    }

    #[test]
    fn canonical_events_pass_through_for_every_tool() {
        for t in Tool::ALL {
            assert_eq!(t.normalise("Stop"), Some("Stop"), "{t}");
            assert_eq!(t.normalise("PreToolUse"), Some("PreToolUse"), "{t}");
            assert_eq!(t.normalise("SessionEnd"), Some("SessionEnd"), "{t}");
        }
    }

    #[test]
    fn copilot_camel_case_maps_onto_the_canonical_names() {
        let c = Tool::Copilot;
        assert_eq!(c.normalise("agentStop"), Some("Stop"));
        assert_eq!(c.normalise("userPromptSubmitted"), Some("UserPromptSubmit"));
        assert_eq!(c.normalise("preToolUse"), Some("PreToolUse"));
        assert_eq!(c.normalise("permissionRequest"), Some("PermissionRequest"));
        assert_eq!(c.normalise("subagentStop"), Some("SubagentStop"));
        assert_eq!(c.normalise("userPromptTransformed"), None);
    }

    #[test]
    fn gemini_has_its_own_names_for_the_same_moments() {
        let g = Tool::Gemini;
        assert_eq!(g.normalise("BeforeAgent"), Some("UserPromptSubmit"));
        assert_eq!(g.normalise("BeforeTool"), Some("PreToolUse"));
        assert_eq!(g.normalise("AfterTool"), Some("PostToolUse"));
        assert_eq!(g.normalise("AfterAgent"), Some("Stop"));
        for noise in [
            "BeforeModel",
            "AfterModel",
            "BeforeToolSelection",
            "PreCompress",
        ] {
            assert_eq!(g.normalise(noise), None, "{noise}");
        }
    }

    #[test]
    fn a_tools_own_spelling_does_not_leak_to_another_tool() {
        assert_eq!(Tool::Claude.normalise("AfterAgent"), None);
        assert_eq!(Tool::Claude.normalise("agentStop"), None);
        assert_eq!(Tool::Codex.normalise("BeforeAgent"), None);
    }

    #[test]
    fn copilot_has_nothing_to_read_off_a_screen() {
        let m = Tool::Copilot.markers();
        assert!(m.working.is_empty());
        assert!(m.questions.is_empty());
        assert!(m.input.is_empty());
        assert_eq!(m.confidence, Confidence::None);
    }
}
