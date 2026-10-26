//! Checks atc's own installation. A check that says "ok" about a configuration
//! which never actually runs is worse than no check at all, so each one here
//! reports only what it was able to verify, and says so when it cannot tell.
use crate::agent::{Confidence, Tool};
use crate::state::PaneState;
use crate::{process, state, tmux};
use std::collections::BTreeSet;

pub(crate) fn run() {
    let tracked = state::all();
    let problems =
        check_tmux() + check_state_dir() + check_state_files(&tracked) + check_agents(&tracked);

    match tmux::current_pane() {
        Some(p) => println!("ok    running inside tmux pane {p}"),
        None => println!("WARN  not inside tmux, so panes cannot be identified"),
    }
    println!("ok    {} tracked pane(s)", tracked.len());
    report_screen_confidence();

    if problems > 0 {
        std::process::exit(1);
    }
}

/// Each check prints its own line and returns how many problems it found, so
/// doctor reads as the list of checks it runs.
fn check_tmux() -> u32 {
    if tmux::available() {
        println!("ok    tmux found");
        return 0;
    }
    println!("FAIL  tmux not on PATH");
    1
}

fn check_state_dir() -> u32 {
    let dir = state::dir();
    match std::fs::create_dir_all(&dir) {
        Ok(()) => {
            println!("ok    state dir {}", dir.display());
            0
        }
        Err(e) => {
            println!("FAIL  state dir {}: {e}", dir.display());
            1
        }
    }
}

/// A state file atc cannot read is skipped in silence everywhere else, which
/// makes a session look as though it never reported at all.
fn check_state_files(tracked: &[PaneState]) -> u32 {
    let Ok(entries) = std::fs::read_dir(state::dir()) else {
        return 0;
    };
    let files = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .count();
    let unreadable = files.saturating_sub(tracked.len());
    if unreadable == 0 {
        return 0;
    }
    println!("WARN  {unreadable} state file(s) could not be read, so those panes look unreported");
    1
}

fn check_agents(tracked: &[PaneState]) -> u32 {
    Tool::ALL
        .into_iter()
        .map(|tool| check_agent(tool, tracked))
        .sum()
}

fn check_agent(tool: Tool, tracked: &[PaneState]) -> u32 {
    let id = tool.id();
    let reported = tracked.iter().filter(|s| s.source == id).count();
    if !process::on_path(id) {
        // State outlives the agent that wrote it, and an agent can be run from
        // somewhere other than PATH, so what has reported is worth saying
        // either way.
        match reported {
            0 => println!("      {id:<8} not installed"),
            n => println!("      {id:<8} not on PATH, yet {n} pane(s) have reported through it"),
        }
        return 0;
    }
    let shown = tool.config_path();
    let problems = match registration(tool) {
        Registration::Complete => {
            println!("ok    {id:<8} all {} hooks registered", tool.hooks().len());
            0
        }
        Registration::Partial(missing) => {
            println!(
                "WARN  {id:<8} {} of {} hooks registered in {shown} — missing {}",
                tool.hooks().len() - missing.len(),
                tool.hooks().len(),
                missing.join(", ")
            );
            1
        }
        Registration::Absent => {
            println!("WARN  {id:<8} nothing in {shown} runs atc — try 'atc install --tool {id}'");
            1
        }
        Registration::Disabled => {
            println!("WARN  {id:<8} hooks are switched off in {shown}, so none of them run");
            1
        }
        Registration::NoFile => {
            println!("WARN  {id:<8} no {shown} — try 'atc install --tool {id}'");
            1
        }
        Registration::Unreadable(why) => {
            println!("WARN  {id:<8} cannot read {shown}, so its hooks cannot be checked: {why}");
            1
        }
    };
    report_delivery(reported);
    problems
}

/// Registration only says the configuration looks right. The proof is a pane
/// that has actually reported, which is the difference between a hook that is
/// written down and one that runs.
fn report_delivery(reported: usize) {
    let note = match reported {
        0 => "no pane has reported through it yet".to_string(),
        1 => "1 pane has reported through it".to_string(),
        n => format!("{n} panes have reported through it"),
    };
    println!("      {:<8} {note}", "");
}

/// How completely a tool's own config file registers atc's hooks.
enum Registration {
    Complete,
    /// Wired for some events and not others, which is the shape a half-finished
    /// install leaves behind.
    Partial(Vec<&'static str>),
    /// The file is readable but nothing in it reaches atc.
    Absent,
    /// Codex only: its hooks are turned off, so the rest of the file is moot.
    Disabled,
    NoFile,
    Unreadable(String),
}

fn registration(tool: Tool) -> Registration {
    let Some(path) = expand_home(tool.config_path()) else {
        return Registration::Unreadable("no home directory".into());
    };
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Registration::NoFile,
        Err(e) => return Registration::Unreadable(e.to_string()),
    };
    let tree = match parse_config(tool, &body) {
        Ok(t) => t,
        Err(e) => return Registration::Unreadable(e),
    };
    if hooks_switched_off(&tree) {
        return Registration::Disabled;
    }
    let wired = events_reaching_atc(&tree);
    if wired.is_empty() {
        return Registration::Absent;
    }
    let missing: Vec<&'static str> = tool
        .hooks()
        .iter()
        .map(|spec| spec.event)
        .filter(|event| !wired.contains(*event))
        .collect();
    if missing.is_empty() {
        Registration::Complete
    } else {
        Registration::Partial(missing)
    }
}

/// Codex writes TOML and the rest write JSON, but both describe the same
/// shape, so reading them into one tree means the check below is written once.
fn parse_config(tool: Tool, body: &str) -> Result<serde_json::Value, String> {
    match tool {
        Tool::Codex => toml::from_str(body).map_err(|e| e.to_string()),
        _ => serde_json::from_str(body).map_err(|e| e.to_string()),
    }
}

fn hooks_switched_off(tree: &serde_json::Value) -> bool {
    tree.get("features").and_then(|f| f.get("hooks")) == Some(&serde_json::Value::Bool(false))
}

/// The events whose handlers genuinely reach `atc hook`.
///
/// Each event holds an array of matcher groups, and the handlers live in an
/// array inside each group. Collapsing that to one handler per event produces a
/// group with nothing in it, which the agents read without complaint and then
/// never run — so this walks the full depth rather than searching the text.
fn events_reaching_atc(tree: &serde_json::Value) -> BTreeSet<&str> {
    let Some(events) = tree.get("hooks").and_then(|h| h.as_object()) else {
        return BTreeSet::new();
    };
    events
        .iter()
        .filter(|(_, groups)| {
            groups
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|group| group.get("hooks").and_then(|h| h.as_array()))
                .flatten()
                .filter_map(|handler| handler.get("command").and_then(|c| c.as_str()))
                .any(|command| command.contains("atc hook"))
        })
        .map(|(event, _)| event.as_str())
        .collect()
}

fn expand_home(path: &str) -> Option<std::path::PathBuf> {
    let rest = path.strip_prefix("~/")?;
    Some(std::path::PathBuf::from(std::env::var("HOME").ok()?).join(rest))
}

/// Screen reading is a fallback for sessions that never fired a hook, and how
/// much to trust it differs per agent. Say so rather than let a guess look
/// like a report.
fn report_screen_confidence() {
    for tool in Tool::ALL {
        let note = match tool.markers().confidence {
            Confidence::Seen => "screen rules checked against a live session",
            Confidence::FromSource => "screen rules taken from its source, never run",
            Confidence::None => "no screen rules — relies on its hooks alone",
        };
        println!("      {:<8} {note}", tool.id());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(body: &str) -> serde_json::Value {
        serde_json::from_str(body).expect("valid json")
    }

    #[test]
    fn a_handler_inside_its_group_counts_as_wired() {
        let t = tree(
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"atc hook Stop"}]}]}}"#,
        );
        assert!(events_reaching_atc(&t).contains("Stop"));
    }

    #[test]
    fn a_group_with_no_handler_array_is_not_wired() {
        let t = tree(r#"{"hooks":{"Stop":[{"type":"command","command":"atc hook Stop"}]}}"#);
        assert!(
            events_reaching_atc(&t).is_empty(),
            "this shape is read without complaint and never runs, so it must not pass"
        );
    }

    #[test]
    fn another_tools_hooks_do_not_count_as_ours() {
        let t =
            tree(r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"notify-send"}]}]}}"#);
        assert!(events_reaching_atc(&t).is_empty());
    }

    #[test]
    fn the_generated_codex_config_registers_every_codex_hook() {
        let body = crate::codex_hooks_toml();
        let t = parse_config(Tool::Codex, &body).expect("generated toml must parse");
        let wired = events_reaching_atc(&t);
        for spec in Tool::Codex.hooks() {
            assert!(
                wired.contains(spec.event),
                "codex/{} is not wired",
                spec.event
            );
        }
        assert!(!hooks_switched_off(&t));
    }

    #[test]
    fn switched_off_hooks_are_spotted() {
        let t = parse_config(Tool::Codex, "[features]\nhooks = false\n").expect("toml");
        assert!(hooks_switched_off(&t));
    }
}
