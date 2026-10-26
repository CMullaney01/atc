//! Every atc subcommand. The hook subcommand is the one that matters: it runs
//! inside a live coding session, so it must never fail in a way the user sees.
mod agent;
mod bar;
mod doctor;
mod hook;
mod infer;
mod pane;
mod process;
mod state;
mod tmux;
mod ui;

use agent::Tool;
use hook::{HookInput, Outcome};
use state::{PaneState, Status};
use std::io::Read;

const USAGE: &str = "\
atc — air traffic control for tmux AI sessions

usage:
  atc hook <Event> [--tool <agent>]     apply an agent hook (reads JSON on stdin)
            [--kind <matcher>]          agent is claude (default), codex, gemini or copilot
  atc bar [session] [--all]             render the fleet status row
  atc status [--json]                   report every tracked session
  atc state <status> [--title <t>]      report state for the current pane by hand
  atc ui                                open the command center
  atc sync                              refresh every window's glyphs (tmux runs this)
  atc seen <pane-id>                    drop a pane's escalation flag
  atc doctor                            check the installation
  atc install [--tool <agent>]                           print the tmux and hook configuration
  atc ask <question>                    ask Claude about the current fleet

states: idle working waiting blocked error
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };

    match cmd {
        "hook" => cmd_hook(rest),
        "bar" => cmd_bar(rest),
        "status" => cmd_status(rest),
        "state" => cmd_state(rest),
        "ui" => ui::run(),
        "seen" => cmd_seen(rest),
        "sync" => cmd_sync(),
        "doctor" => doctor::run(),
        "install" => {
            let only = flag(rest, "--tool").and_then(Tool::parse);
            if let Some(name) = flag(rest, "--tool") {
                if only.is_none() {
                    eprintln!("atc install: unknown tool '{name}'");
                    std::process::exit(2);
                }
            }
            print!("{}", install_text(only));
        }
        "ask" => cmd_ask(rest),
        "help" | "-h" | "--help" => print!("{USAGE}"),
        other => {
            eprintln!("atc: unknown command '{other}'\n");
            print!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).map(String::as_str)
}

/// The event name, wherever it sits among the flags. The hooks atc registers
/// for the other agents put it after `--tool`, so reading the first argument
/// found `--tool` and silently dropped every one of them.
///
/// Every flag atc takes carries a value, so a flag and its value are skipped
/// together.
fn event_arg(args: &[String]) -> Option<&str> {
    let mut rest = args;
    while let Some(first) = rest.first() {
        if !first.starts_with("--") {
            return Some(first);
        }
        rest = rest.get(2..).unwrap_or_default();
    }
    None
}

/// Hooks run on every tool call in every session, so this must never fail in a
/// way Claude notices: all problems are logged and the exit code stays 0.
/// Which agent called, and what its event is called here. `None` when the
/// agent is unknown, or when it is an event atc has no use for.
fn resolve(args: &[String], event: &str) -> Option<(Tool, &'static str)> {
    let tool = match flag(args, "--tool") {
        Some(name) => match Tool::parse(name) {
            Some(t) => t,
            None => {
                log(&format!("hook called with unknown tool '{name}'"));
                return None;
            }
        },
        None => Tool::Claude,
    };
    Some((tool, tool.normalise(event)?))
}

/// Claude Code has been seen to send a raw control character inside a string,
/// which makes the whole payload invalid JSON. Losing one field is survivable;
/// losing the message that says what a session wants is not, so a failed parse
/// is retried against a cleaned copy before giving up.
fn parse_payload(event: &str, raw: &str) -> HookInput {
    if raw.trim().is_empty() {
        return HookInput::default();
    }
    match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(first) => {
            let cleaned: String = raw
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            match serde_json::from_str(&cleaned) {
                Ok(v) => {
                    log(&format!(
                        "hook payload for {event} needed cleaning: {first}"
                    ));
                    v
                }
                Err(e) => {
                    log(&format!("unparsable hook payload for {event}: {e}"));
                    HookInput::default()
                }
            }
        }
    }
}

fn cmd_hook(args: &[String]) {
    let Some(event) = event_arg(args) else {
        log("hook called without an event name");
        return;
    };
    let kind = flag(args, "--kind");
    let Some((tool, event)) = resolve(args, event) else {
        return;
    };

    let Some(pane_id) = tmux::current_pane() else {
        return; // not running under tmux; nothing to report to
    };

    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        log("could not read hook payload from stdin");
    }
    let input = parse_payload(event, &raw);

    let prev = state::load(&pane_id);
    let was = prev.as_ref().map(|p| (p.status, p.stale));
    match hook::apply(event, kind, &input, &pane_id, prev) {
        Outcome::Update(mut st) => {
            st.source = tool.id().to_string();
            if let Err(e) = state::save(&st) {
                log(&format!("could not save state for {pane_id}: {e}"));
            }
            // Tool events fire constantly; only touch tmux when what the bar
            // actually draws has changed.
            if was != Some((st.status, st.stale)) {
                tmux::set_pane_option(&pane_id, "@atc_state", st.status.label());
                refresh_window_for(&pane_id);
                tmux::refresh_client();
            }
        }
        Outcome::Remove => {
            if let Err(e) = state::remove(&pane_id) {
                log(&format!("could not clear state for {pane_id}: {e}"));
            }
            tmux::unset_pane_option(&pane_id, "@atc_state");
            refresh_window_for(&pane_id);
            tmux::refresh_client();
        }
        Outcome::Ignore => {}
    }
}

/// Window-level glyphs are read straight out of `@atc_win` by the status
/// format, which costs nothing to draw but has to be recomputed here.
fn write_window_glyphs(entries: &[bar::Entry], session: &str, window_index: u32) {
    let target = format!("{session}:{window_index}");
    match bar::window_glyphs(entries, session, window_index, bar::Paint::from_tmux()) {
        Some(glyphs) => tmux::set_window_option(&target, "@atc_win", &glyphs),
        None => tmux::unset_window_option(&target, "@atc_win"),
    }
}

fn refresh_window_for(pane_id: &str) {
    let panes = tmux::list_panes();
    let live: Vec<String> = panes.iter().map(|p| p.pane_id.clone()).collect();
    state::prune(&live);
    let Some(info) = panes.iter().find(|p| p.pane_id == pane_id) else {
        return;
    };
    let states = state::all();
    let entries = bar::inventory(&states, &panes);
    write_window_glyphs(&entries, &info.session, info.window_index);
}

/// Refreshes every window's glyphs, including the ones only a screen reading
/// knows about. tmux runs this once per status redraw.
///
/// It deliberately does not call `tmux::refresh_client()`. Forcing a redraw
/// would re-run the status line, which runs this again, for ever. Writing the
/// options and letting the next redraw collect them is enough.
fn cmd_sync() {
    let panes = tmux::list_panes();
    let live: Vec<String> = panes.iter().map(|p| p.pane_id.clone()).collect();
    state::prune(&live);
    let states = state::all();
    let mut entries = bar::inventory(&states, &panes);
    infer::decorate(&mut entries);

    let mut seen: Vec<(String, u32)> = panes
        .iter()
        .map(|p| (p.session.clone(), p.window_index))
        .collect();
    seen.sort();
    seen.dedup();
    for (session, window_index) in seen {
        write_window_glyphs(&entries, &session, window_index);
    }
}

fn cmd_bar(args: &[String]) {
    let opts = bar::RowOpts {
        scope: if args.iter().any(|a| a == "--all") {
            bar::Scope::Everything
        } else {
            bar::Scope::AiOnly
        },
        paint: bar::Paint::from_tmux(),
    };
    let session = args
        .iter()
        .find(|a| !a.starts_with("--") && !a.is_empty())
        .cloned()
        .or_else(tmux::current_session);
    let states = state::all();
    let panes = tmux::list_panes();
    let mut inv = bar::inventory(&states, &panes);
    infer::decorate(&mut inv);
    println!("{}", bar::render(&inv, session.as_deref(), opts));
}

fn cmd_status(args: &[String]) {
    let states = state::all();
    let panes = tmux::list_panes();
    let mut inv = bar::inventory(&states, &panes);
    infer::decorate(&mut inv);

    if args.iter().any(|a| a == "--json") {
        print_status_json(&inv);
    } else {
        print_status_tree(&inv);
    }
}

/// Everything atc knows, for a manager session to reason over.
fn print_status_json(inv: &[bar::Entry]) {
    let rows: Vec<serde_json::Value> = inv
        .iter()
        .map(|e| {
            serde_json::json!({
                "pane_id": e.info.pane_id,
                "session": e.info.session,
                "window": e.info.window_index,
                "pane": e.info.pane_index,
                "address": format!("{}:{}", e.info.session, e.info.address()),
                "window_name": e.info.window_name,
                "kind": e.kind.label(),
                "command": e.info.command,
                "status": e.status().map(|s| s.label()),
                "inferred": e.is_inferred(),
                "source": e.state.map(|s| s.source.clone()),
                "detected": e.tool().map(|t| t.id()),
                "subagents": e.subagents().iter().map(|a| serde_json::json!({
                    "type": a.label(),
                    "age_secs": a.age_secs(),
                })).collect::<Vec<_>>(),
                "stale": e.stale(),
                "age_secs": e.state.map(|s| s.age_secs()),
                "title": e.title(),
                "cwd": e.state.and_then(|s| s.cwd.clone()).unwrap_or_else(|| e.info.path.clone()),
                "tool": e.state.and_then(|s| s.tool.clone()),
                "last_message": e.state.and_then(|s| s.last_message.clone()),
            })
        })
        .collect();
    let doc = serde_json::json!({ "panes": rows });
    println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
}

/// The same tree the popup draws, for a terminal.
fn print_status_tree(inv: &[bar::Entry]) {
    if inv.iter().all(|e| !e.kind.is_ai()) {
        println!("no AI sessions");
    }
    for row in bar::tree(inv, tmux::current_session().as_deref()) {
        match row {
            bar::TreeRow::Session(name) => println!("{name}"),
            bar::TreeRow::Window { index, name } => println!("  {index} {name}"),
            bar::TreeRow::Pane(i) => print_status_pane(&inv[i]),
        }
    }
}

/// One pane, with whatever detail it has under it.
fn print_status_pane(e: &bar::Entry) {
    println!(
        "    {}{} {:<24} {:<11} {:>5}  {}{}",
        e.glyph(),
        if e.is_inferred() {
            bar::INFERRED_MARK
        } else {
            " "
        },
        truncate(&e.title(), 24),
        e.status()
            .map(|s| s.label())
            .unwrap_or_else(|| e.kind.label()),
        e.state
            .map(|s| state::fmt_age(s.age_secs()))
            .unwrap_or_else(|| {
                if e.is_inferred() {
                    bar::INFERRED_MARK
                } else {
                    "-"
                }
                .to_string()
            }),
        e.info.address(),
        if e.stale() { "  (stale)" } else { "" },
    );
    for a in e.subagents() {
        println!(
            "        ⑂ {:<22} {:>5}",
            a.label(),
            state::fmt_age(a.age_secs())
        );
    }
    if let Some(tool) = e.state.and_then(|s| s.tool.as_ref()) {
        println!("        tool: {tool}");
    }
    if let Some(msg) = e.state.and_then(|s| s.last_message.as_ref()) {
        println!("        {}", truncate(msg, 68));
    }
}

fn cmd_state(args: &[String]) {
    let Some(raw) = args.first() else {
        eprintln!("atc state: need a status (idle working waiting blocked error)");
        std::process::exit(2);
    };
    let Some(status) = Status::parse(raw) else {
        eprintln!("atc state: '{raw}' is not a status");
        std::process::exit(2);
    };
    let Some(pane_id) = tmux::current_pane() else {
        eprintln!("atc state: not running inside tmux");
        std::process::exit(1);
    };

    let mut st = state::load(&pane_id).unwrap_or_else(|| PaneState::new(&pane_id));
    if st.status != status {
        st.since = state::now();
        st.stale = false;
    }
    st.status = status;
    st.source = "manual".to_string();
    if let Some(t) = flag(args, "--title") {
        st.title = Some(t.to_string());
    }
    if let Err(e) = state::save(&st) {
        eprintln!("atc state: {e}");
        std::process::exit(1);
    }
    tmux::set_pane_option(&pane_id, "@atc_state", st.status.label());
    refresh_window_for(&pane_id);
    tmux::refresh_client();
}

/// Called from tmux's after-select-pane hook: looking at a pane means you are
/// no longer ignoring it, but it still wants a reply, so only `stale` clears.
fn cmd_seen(args: &[String]) {
    let Some(pane_id) = args.first() else {
        return;
    };
    let Some(mut st) = state::load(pane_id) else {
        return;
    };
    if !st.stale {
        return;
    }
    st.stale = false;
    if state::save(&st).is_ok() {
        refresh_window_for(pane_id);
        tmux::refresh_client();
    }
}

fn cmd_ask(args: &[String]) {
    if args.is_empty() {
        eprintln!("atc ask: need a question");
        std::process::exit(2);
    }
    let question = args.join(" ");
    let states = state::all();
    let panes = tmux::list_panes();
    let inv = bar::inventory(&states, &panes);

    let mut fleet = String::new();
    for e in inv.iter().filter(|e| e.kind.is_ai()) {
        fleet.push_str(&format!(
            "- {}:{} [{}] {} (for {}){}\n",
            e.info.session,
            e.info.address(),
            e.status().map(|s| s.label()).unwrap_or("state unknown"),
            e.title(),
            e.state
                .map(|s| state::fmt_age(s.age_secs()))
                .unwrap_or_else(|| "unknown".to_string()),
            e.state
                .and_then(|s| s.last_message.as_ref())
                .map(|m| format!("\n    last said: {m}"))
                .unwrap_or_default(),
        ));
    }
    if fleet.is_empty() {
        fleet.push_str("(no sessions are currently tracked)\n");
    }

    let prompt = format!(
        "These are my current AI coding sessions in tmux:\n\n{fleet}\nQuestion: {question}\n\n\
         Answer briefly, and name the tmux address of any session I should go to."
    );
    let status = std::process::Command::new("claude")
        .arg("-p")
        .arg(&prompt)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => std::process::exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("atc ask: could not run claude: {e}");
            std::process::exit(1);
        }
    }
}

fn install_text(only: Option<Tool>) -> String {
    let mut out = String::new();
    if only.is_none() {
        out.push_str(&format!(
            "# tmux — source this, or copy it into your tmux.conf\n\n{}\n",
            include_str!("../tmux/atc.conf")
        ));
    }
    for tool in Tool::ALL {
        if only.is_some_and(|t| t != tool) {
            continue;
        }
        out.push_str(&agent_install_text(tool));
        out.push('\n');
    }
    out
}

/// Each agent keeps its hooks somewhere different and in a different format,
/// so this is four separate snippets rather than one with the paths swapped.
fn agent_install_text(tool: Tool) -> String {
    match tool {
        Tool::Claude => format!(
            "# claude — merge into the \"hooks\" object in {}\n\n{}\n",
            Tool::Claude.config_path(),
            hooks_json(Tool::Claude)
        ),
        Tool::Codex => format!(
            "# codex — add to {}. Recent versions run hooks by default; the\n\
             # flag below is harmless there and still needed by older ones.\n\
             # A project .codex/config.toml is only read for a trusted project.\n\n{}\n",
            Tool::Codex.config_path(),
            codex_hooks_toml()
        ),
        Tool::Gemini => format!(
            "# gemini — merge into the \"hooks\" object in {}.\n\
             # Hooks run only in folders you have trusted.\n\n{}\n",
            Tool::Gemini.config_path(),
            hooks_json(Tool::Gemini)
        ),
        Tool::Copilot => format!(
            "# copilot — save as {}\n\n{}\n",
            Tool::Copilot.config_path(),
            hooks_json(Tool::Copilot)
        ),
    }
}

/// Codex takes an array of matcher groups per event, each holding its own
/// array of handlers. Flattening that to one table per event registers a group
/// with no handler in it, which codex accepts in silence and never runs.
fn codex_hooks_toml() -> String {
    let mut out = String::from("[features]\nhooks = true\n");
    for spec in Tool::Codex.hooks() {
        out.push_str(&format!(
            "\n[[hooks.{event}]]\n\n[[hooks.{event}.hooks]]\ntype = \"command\"\ncommand = \"{command}\"\n",
            event = spec.event,
            command = spec.command(Tool::Codex)
        ));
    }
    out
}

fn hooks_json(tool: Tool) -> String {
    let mut by_event: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for spec in tool.hooks() {
        let command = spec.command(tool);
        let entry = match spec.kind {
            Some(k) => format!(
                "      {{ \"matcher\": \"{k}\", \"hooks\": [{{ \"type\": \"command\", \"command\": \"{command}\" }}] }}"
            ),
            None => format!(
                "      {{ \"hooks\": [{{ \"type\": \"command\", \"command\": \"{command}\" }}] }}"
            ),
        };
        by_event.entry(spec.event).or_default().push(entry);
    }
    let body: Vec<String> = by_event
        .iter()
        .map(|(event, entries)| format!("    \"{event}\": [\n{}\n    ]", entries.join(",\n")))
        .collect();
    format!("{{\n  \"hooks\": {{\n{}\n  }}\n}}", body.join(",\n"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars()
        .take(max.saturating_sub(1))
        .chain("…".chars())
        .collect()
}

fn log(msg: &str) {
    use std::io::Write;
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let path = std::path::PathBuf::from(home).join(".local/state/atc/atc.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{} {}", state::now(), msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_strings_intact() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn flag_reads_the_following_argument() {
        let args: Vec<String> = ["--kind", "idle_prompt"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag(&args, "--kind"), Some("idle_prompt"));
        assert_eq!(flag(&args, "--missing"), None);
    }

    #[test]
    fn every_registered_hook_command_parses_back_to_its_event() {
        // The seam the other test missed: it checked the event name alone,
        // while the command line atc registers puts it after --tool, where
        // the argument reader never looked.
        for tool in Tool::ALL {
            for spec in tool.hooks() {
                let cmd = spec.command(tool);
                let args: Vec<String> = cmd
                    .split_whitespace()
                    .skip(2) // "atc hook"
                    .map(str::to_string)
                    .collect();
                let event = event_arg(&args);
                assert_eq!(
                    event,
                    Some(spec.event),
                    "reading the event back out of: {cmd}"
                );
                assert!(
                    resolve(&args, event.unwrap_or_default()).is_some(),
                    "handler ignores its own registration: {cmd}"
                );
            }
        }
    }

    #[test]
    fn the_event_is_found_whichever_side_of_the_flags_it_sits() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(event_arg(&args(&["Stop"])), Some("Stop"));
        assert_eq!(
            event_arg(&args(&["--tool", "gemini", "AfterAgent"])),
            Some("AfterAgent")
        );
        assert_eq!(
            event_arg(&args(&["Notification", "--kind", "idle_prompt"])),
            Some("Notification")
        );
        // A flag whose value is missing must not be read as the event.
        assert_eq!(event_arg(&args(&["--tool"])), None);
        assert_eq!(event_arg(&args(&[])), None);
    }

    #[test]
    fn every_registered_hook_is_one_the_handler_acts_on() {
        for tool in Tool::ALL {
            for spec in tool.hooks() {
                let event = tool.normalise(spec.event).unwrap_or_else(|| {
                    panic!("{tool} registers {} but nothing maps it", spec.event)
                });
                let mut input = HookInput::default();
                if event.starts_with("Subagent") {
                    input.agent_id = Some("a1".to_string());
                }
                if event == "Notification" && spec.kind.is_none() {
                    input.notification_type = Some("ToolPermission".to_string());
                }
                let outcome = hook::apply(
                    event,
                    spec.kind,
                    &input,
                    "%1",
                    Some(state::PaneState::new("%1")),
                );
                assert_ne!(
                    outcome,
                    Outcome::Ignore,
                    "{tool} registers {} -> {event}, which the handler ignores",
                    spec.event
                );
            }
        }
    }

    #[test]
    fn copilots_camel_case_payload_is_read() {
        let input = parse_payload(
            "PreToolUse",
            r#"{"sessionId":"s1","cwd":"/tmp","toolName":"bash","agentId":"a1","agentType":"Explore"}"#,
        );
        assert_eq!(input.session_id.as_deref(), Some("s1"));
        assert_eq!(input.cwd.as_deref(), Some("/tmp"));
        assert_eq!(input.tool_name.as_deref(), Some("bash"));
        assert_eq!(input.agent_id.as_deref(), Some("a1"));
        assert_eq!(input.agent_type.as_deref(), Some("Explore"));
    }

    #[test]
    fn every_agents_install_block_is_well_formed() {
        for (tool, text) in [
            (Tool::Gemini, hooks_json(Tool::Gemini)),
            (Tool::Copilot, hooks_json(Tool::Copilot)),
        ] {
            let v: serde_json::Value =
                serde_json::from_str(&text).unwrap_or_else(|e| panic!("{tool}: {e}"));
            let hooks = v["hooks"].as_object().expect("a hooks object");
            assert!(!hooks.is_empty(), "{tool} registered nothing");
            for (event, entries) in hooks {
                let cmd = entries[0]["hooks"][0]["command"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{tool}/{event} has no command"));
                assert!(
                    cmd.contains(&format!("--tool {tool}")) && cmd.ends_with(event),
                    "{tool}/{event} command is wrong: {cmd}"
                );
            }
        }
    }

    /// Parsed rather than read as text: the shape this rejects is one codex
    /// accepts in silence and never runs, which no check on the text can see.
    #[test]
    fn the_codex_install_block_is_well_formed() {
        let generated = codex_hooks_toml();
        let doc: toml::Value =
            toml::from_str(&generated).expect("the codex config must be valid TOML");
        assert_eq!(
            doc.get("features").and_then(|f| f.get("hooks")),
            Some(&toml::Value::Boolean(true)),
            "the feature flag must survive, for the codex versions that need it"
        );
        let hooks = doc.get("hooks").expect("a hooks table");
        for spec in Tool::Codex.hooks() {
            let want = spec.command(Tool::Codex);
            let groups = hooks
                .get(spec.event)
                .and_then(toml::Value::as_array)
                .unwrap_or_else(|| panic!("codex/{} registers no group", spec.event));
            let mut handlers = groups
                .iter()
                .filter_map(|g| g.get("hooks").and_then(toml::Value::as_array))
                .flatten();
            assert!(
                handlers.any(|h| {
                    h.get("type").and_then(toml::Value::as_str) == Some("command")
                        && h.get("command").and_then(toml::Value::as_str) == Some(want.as_str())
                }),
                "codex/{} has no command handler inside its group",
                spec.event
            );
        }
    }

    #[test]
    fn asking_for_one_agent_prints_only_that_agent() {
        let only = install_text(Some(Tool::Gemini));
        assert!(only.contains("~/.gemini/settings.json"));
        assert!(!only.contains("~/.codex/config.toml"));
        assert!(
            !only.contains("status-left"),
            "a single agent needs no tmux config"
        );

        let all = install_text(None);
        for tool in Tool::ALL {
            assert!(all.contains(&format!("# {tool} —")), "{tool} missing");
        }
    }

    #[test]
    fn generated_hooks_json_is_valid_and_covers_both_notification_matchers() {
        let v: serde_json::Value =
            serde_json::from_str(&hooks_json(Tool::Claude)).expect("valid JSON");
        let hooks = &v["hooks"];
        assert!(hooks["Stop"].is_array());
        let notif = hooks["Notification"]
            .as_array()
            .expect("notification array");
        assert_eq!(notif.len(), 2);
        let matchers: Vec<&str> = notif.iter().filter_map(|n| n["matcher"].as_str()).collect();
        assert!(matchers.contains(&"permission_prompt"));
        assert!(matchers.contains(&"idle_prompt"));
    }
}
