//! What a pane is really running. tmux reports the name of the foreground
//! process, which for an agent installed from npm is `node` — so the name
//! alone cannot tell a gemini session from a webpack build. The script the
//! interpreter was handed can.
use crate::agent::Tool;
use std::collections::HashMap;
use std::process::Command;

/// Runtimes that agents are shipped on top of. A pane running one of these is
/// not identified by its command name, so its command line has to be read.
///
/// Only the JavaScript ones, because that is what the agents are written in:
/// gemini and copilot are npm packages, and node is the one confirmed against
/// a real pane. Nothing is listed here on the chance an agent might use it.
const INTERPRETERS: &[&str] = &["node", "bun", "deno"];

/// tmux reports a bare command name, but be tolerant of paths and the leading
/// dash a login shell carries.
pub(crate) fn base_command(cmd: &str) -> &str {
    cmd.rsplit('/')
        .next()
        .unwrap_or(cmd)
        .trim_start_matches('-')
}

pub(crate) fn is_interpreter(cmd: &str) -> bool {
    INTERPRETERS.contains(&cmd)
}

/// Which agent a command line belongs to, or `None` for an ordinary script.
///
/// Only the script the interpreter was given is examined, never the arguments
/// after it, so `node build.js --out gemini.txt` is not a gemini session.
pub(crate) fn tool_from_argv(argv: &str) -> Option<Tool> {
    let script = argv
        .split_whitespace()
        .skip(1)
        .find(|t| !t.starts_with('-'))?;
    Tool::ALL
        .into_iter()
        .find(|t| path_names(script).any(|segment| names_the_tool(segment, t.id())))
}

/// Every path segment of a script, with the extension dropped so `gemini.js`
/// and `gemini` read the same.
fn path_names(script: &str) -> impl Iterator<Item = &str> {
    script.split('/').map(|seg| {
        [".js", ".mjs", ".cjs"]
            .iter()
            .find_map(|ext| seg.strip_suffix(ext))
            .unwrap_or(seg)
    })
}

/// npm puts the agent's name in the package directory rather than the entry
/// point, so `@github/copilot/index.js` is matched on `copilot`, and a
/// trailing word is allowed for the ones that publish as `<name>-cli`.
fn names_the_tool(segment: &str, tool_id: &str) -> bool {
    segment == tool_id
        || segment
            .strip_prefix(tool_id)
            .is_some_and(|tail| tail.starts_with('-'))
}

/// The command line of every interpreter process, keyed by the terminal it is
/// running on, which is how a pane is matched to it.
///
/// One `ps` for the whole machine rather than one per pane, and only called
/// when some pane is running an interpreter at all.
pub(crate) fn command_lines() -> HashMap<String, String> {
    let Some(out) = ps() else {
        return HashMap::new();
    };
    let mut lines = HashMap::new();
    for line in out.lines() {
        let Some((tty, argv)) = line.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        let argv = argv.trim_start();
        let first = argv.split_whitespace().next().unwrap_or_default();
        if !is_interpreter(base_command(first)) {
            continue;
        }
        lines.entry(tty.to_string()).or_insert(argv.to_string());
    }
    lines
}

/// Whether a command can be run at all, which is how `atc doctor` tells an
/// agent that is not installed from one whose hooks are simply missing.
pub(crate) fn on_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

fn ps() -> Option<String> {
    let out = Command::new("ps")
        .args(["-axo", "tty=,args="])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_installed_agents_are_found_in_the_script_path() {
        // The real shapes: the agent's name is the package directory, and the
        // entry point is called something generic.
        for (argv, want) in [
            (
                "node /opt/homebrew/lib/node_modules/@github/copilot/index.js",
                Tool::Copilot,
            ),
            (
                "node /usr/local/lib/node_modules/@google/gemini-cli/dist/index.js",
                Tool::Gemini,
            ),
            ("node /Users/me/.bun/bin/gemini.js --yolo", Tool::Gemini),
            ("node /opt/node_modules/.bin/codex", Tool::Codex),
            (
                "node /Users/me/.npm-global/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                Tool::Claude,
            ),
        ] {
            assert_eq!(tool_from_argv(argv), Some(want), "{argv}");
        }
    }

    #[test]
    fn an_ordinary_script_is_not_an_agent() {
        for argv in [
            "node /Users/me/dev/site/build.js",
            "node",
            "bun /Users/me/dev/site/serve.ts",
            "",
        ] {
            assert_eq!(tool_from_argv(argv), None, "{argv}");
        }
    }

    #[test]
    fn only_the_script_is_read_not_its_arguments() {
        // Without this rule any command mentioning an agent would register as
        // one, which is how a false positive gets onto the status row.
        assert_eq!(
            tool_from_argv("node build.js --out gemini.txt --log copilot"),
            None
        );
    }

    #[test]
    fn flags_before_the_script_are_skipped() {
        assert_eq!(
            tool_from_argv("node --max-old-space-size=8192 /opt/@github/copilot/index.js"),
            Some(Tool::Copilot)
        );
    }

    #[test]
    fn interpreters_are_recognised_through_a_path() {
        assert!(is_interpreter(base_command("/opt/homebrew/bin/node")));
        assert!(is_interpreter(base_command("node")));
        assert!(!is_interpreter(base_command("cargo")));
        assert!(!is_interpreter(base_command("-zsh")));
    }

    #[test]
    fn base_command_strips_paths_and_the_login_dash() {
        assert_eq!(base_command("/bin/bash"), "bash");
        assert_eq!(base_command("-zsh"), "zsh");
        assert_eq!(base_command("codex"), "codex");
    }
}
