//! Works out what is running in a pane when no hook has reported it. Without
//! this, a session that atc was never told about would be invisible.
use crate::agent::Tool;
use crate::proc::{base_command, is_interpreter, tool_from_argv};
use crate::state::PaneState;
use crate::tmux::PaneInfo;

/// Long-lived interactive programs. Without this list they would all look like
/// jobs that never finish.
const TOOLS: &[&str] = &[
    "nvim",
    "vim",
    "view",
    "k9s",
    "tredis",
    "lazygit",
    "lazydocker",
    "htop",
    "btop",
    "top",
    "less",
    "man",
    "tig",
    "ssh",
];

const SHELLS: &[&str] = &["zsh", "bash", "fish", "sh", "dash", "ksh"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An AI session with hook state behind it.
    Session,
    /// Looks like an AI session but has never fired a hook, so its state is
    /// genuinely unknown — usually a session started before atc was installed.
    Unreported,
    Tool,
    Shell,
    /// A foreground command in what is otherwise a shell pane.
    Job,
}

impl Kind {
    pub(crate) fn is_ai(self) -> bool {
        matches!(self, Kind::Session | Kind::Unreported)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Kind::Session => "session",
            Kind::Unreported => "unreported",
            Kind::Tool => "tool",
            Kind::Shell => "shell",
            Kind::Job => "job",
        }
    }

    /// Only used for panes with no hook state of their own.
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Kind::Session => "·",
            Kind::Unreported => "?",
            Kind::Tool | Kind::Shell => "·",
            Kind::Job => "◐",
        }
    }

    pub(crate) fn colour(self) -> &'static str {
        match self {
            Kind::Session | Kind::Unreported => "colour239",
            Kind::Tool | Kind::Shell => "colour242",
            Kind::Job => "colour19",
        }
    }
}

/// Claude Code decorates its pane title with "✳".
fn titled_like_claude(title: &str) -> bool {
    title.trim_start().starts_with('✳')
}

/// Which agent a pane is running, going by what tmux reports about it. `None`
/// for anything that is not one.
///
/// A title outlives the program that set it, so a pane an agent has exited
/// still carries its mark while plainly being a shell again. The command wins
/// wherever the two disagree.
pub(crate) fn detect_tool(info: &PaneInfo) -> Option<Tool> {
    let cmd = base_command(&info.command);
    if let Some(t) = Tool::ALL.into_iter().find(|t| t.commands().contains(&cmd)) {
        return Some(t);
    }
    // gemini and copilot are npm packages, so tmux reports them as `node` and
    // they used to land on the status row as ordinary jobs. Their command line
    // is the only thing that says which agent they are.
    if is_interpreter(cmd) {
        return tool_from_argv(&info.argv);
    }
    if SHELLS.contains(&cmd) {
        return None;
    }
    (looks_like_a_version(&info.command) || titled_like_claude(&info.title)).then_some(Tool::Claude)
}

/// The claude binary is named after its version, so a pane running it reports
/// a command like "2.1.308" rather than anything recognisable.
fn looks_like_a_version(cmd: &str) -> bool {
    let mut parts = cmd.split('.');
    let enough = cmd.matches('.').count() >= 2;
    enough && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

pub(crate) fn classify(info: &PaneInfo, state: Option<&PaneState>) -> Kind {
    if state.is_some() {
        return Kind::Session;
    }
    if detect_tool(info).is_some() {
        return Kind::Unreported;
    }
    let cmd = base_command(&info.command);
    if TOOLS.contains(&cmd) {
        Kind::Tool
    } else if SHELLS.contains(&cmd) {
        Kind::Shell
    } else {
        Kind::Job
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(title: &str, command: &str) -> PaneInfo {
        PaneInfo {
            pane_id: "%1".to_string(),
            session: "ws".to_string(),
            window_index: 0,
            window_name: "w".to_string(),
            pane_index: 0,
            active: false,
            window_active: false,
            session_attached: false,
            title: title.to_string(),
            command: command.to_string(),
            path: "/tmp".to_string(),
            tty: "ttys001".to_string(),
            argv: String::new(),
        }
    }

    fn node_pane(argv: &str) -> PaneInfo {
        PaneInfo {
            argv: argv.to_string(),
            ..pane("", "node")
        }
    }

    #[test]
    fn hook_state_wins_over_everything() {
        let st = PaneState::new("%1");
        assert_eq!(classify(&pane("", "zsh"), Some(&st)), Kind::Session);
    }

    #[test]
    fn a_claude_pane_with_no_state_is_unreported_not_a_job() {
        // The real shape: claude's process is named after its version.
        assert_eq!(
            classify(&pane("✳ fix-pyright-lint-errors", "2.1.308"), None),
            Kind::Unreported
        );
        // A pane Claude Code has exited keeps the title it set, so the title
        // alone is not enough: this one is a shell again.
        assert_eq!(classify(&pane("✳ Claude Code", "zsh"), None), Kind::Shell);
        // Version alone is enough, even with no title.
        assert_eq!(classify(&pane("", "2.1.296"), None), Kind::Unreported);
    }

    #[test]
    fn long_lived_tuis_are_tools_not_jobs() {
        for cmd in ["nvim", "k9s", "tredis", "lazygit", "htop"] {
            assert_eq!(classify(&pane("", cmd), None), Kind::Tool, "{cmd}");
        }
    }

    #[test]
    fn shells_are_recognised_including_login_shells() {
        assert_eq!(classify(&pane("", "zsh"), None), Kind::Shell);
        assert_eq!(classify(&pane("", "-zsh"), None), Kind::Shell);
        assert_eq!(classify(&pane("", "/bin/bash"), None), Kind::Shell);
    }

    #[test]
    fn anything_else_in_a_pane_is_a_running_job() {
        for cmd in ["cargo", "pytest", "make"] {
            assert_eq!(classify(&pane("", cmd), None), Kind::Job, "{cmd}");
        }
    }

    #[test]
    fn each_agent_is_recognised_by_its_command() {
        for (cmd, want) in [
            ("codex", Tool::Codex),
            ("gemini", Tool::Gemini),
            ("copilot", Tool::Copilot),
            ("/opt/homebrew/bin/codex", Tool::Codex),
            ("2.1.308", Tool::Claude),
        ] {
            assert_eq!(detect_tool(&pane("", cmd)), Some(want), "{cmd}");
            assert_eq!(classify(&pane("", cmd), None), Kind::Unreported, "{cmd}");
        }
    }

    #[test]
    fn a_shell_is_not_an_agent_whatever_its_title_says() {
        assert_eq!(detect_tool(&pane("✳ Claude Code", "zsh")), None);
        assert_eq!(detect_tool(&pane("", "nvim")), None);
        assert_eq!(detect_tool(&pane("", "cargo")), None);
    }

    #[test]
    fn an_agent_running_under_node_is_still_an_agent() {
        // The bug this guards: gemini and copilot report `node` to tmux, so
        // they were classified as jobs and left off the status row entirely.
        let gemini = node_pane("node /opt/lib/node_modules/@google/gemini-cli/dist/index.js");
        assert_eq!(detect_tool(&gemini), Some(Tool::Gemini));
        assert_eq!(classify(&gemini, None), Kind::Unreported);

        let copilot = node_pane("node /opt/lib/node_modules/@github/copilot/index.js");
        assert_eq!(detect_tool(&copilot), Some(Tool::Copilot));
        assert_eq!(classify(&copilot, None), Kind::Unreported);
    }

    #[test]
    fn an_ordinary_node_process_is_still_a_job() {
        let build = node_pane("node /Users/me/dev/site/build.js");
        assert_eq!(detect_tool(&build), None);
        assert_eq!(classify(&build, None), Kind::Job);
        // No command line read at all, which is every pane that is not an
        // interpreter: nothing to go on, so still a job.
        assert_eq!(classify(&pane("", "node"), None), Kind::Job);
    }

    #[test]
    fn version_detection_does_not_catch_ordinary_names() {
        assert!(!looks_like_a_version("zsh"));
        assert!(!looks_like_a_version("python3.11"));
        assert!(!looks_like_a_version(""));
        assert!(looks_like_a_version("2.1.308"));
    }

    #[test]
    fn only_ai_kinds_report_as_ai() {
        assert!(Kind::Session.is_ai() && Kind::Unreported.is_ai());
        assert!(!Kind::Tool.is_ai() && !Kind::Shell.is_ai() && !Kind::Job.is_ai());
    }
}
