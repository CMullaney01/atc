//! Every atc subcommand. The hook subcommand is the one that matters: it runs
//! inside a live coding session, so it must never fail in a way the user sees.
mod bar;
mod hook;
mod infer;
mod pane;
mod state;
mod tmux;
mod ui;

use hook::{HookInput, Outcome};
use state::{PaneState, Status};
use std::io::Read;

const USAGE: &str = "\
atc — air traffic control for tmux AI sessions

usage:
  atc hook <Event> [--kind <matcher>]   apply a Claude Code hook (reads JSON on stdin)
  atc bar [session] [--all]             render the fleet status row
  atc status [--json]                   report every tracked session
  atc state <status> [--title <t>]      report state for the current pane by hand
  atc ui                                open the command center
  atc seen <pane-id>                    drop a pane's escalation flag
  atc doctor                            check the installation
  atc install                           print the tmux and hook configuration
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
        "doctor" => cmd_doctor(),
        "install" => print!("{}", install_text()),
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

/// Hooks run on every tool call in every session, so this must never fail in a
/// way Claude notices: all problems are logged and the exit code stays 0.
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
    let Some(event) = args.first() else {
        log("hook called without an event name");
        return;
    };
    let kind = flag(args, "--kind");

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
        Outcome::Update(st) => {
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
fn refresh_window_for(pane_id: &str) {
    let panes = tmux::list_panes();
    let live: Vec<String> = panes.iter().map(|p| p.pane_id.clone()).collect();
    state::prune(&live);
    let Some(info) = panes.iter().find(|p| p.pane_id == pane_id) else {
        return;
    };
    let target = format!("{}:{}", info.session, info.window_index);
    let states = state::all();
    match bar::window_glyph(&states, &panes, &info.session, info.window_index) {
        Some(glyph) => tmux::set_window_option(&target, "@atc_win", &glyph),
        None => tmux::unset_window_option(&target, "@atc_win"),
    }
}

fn cmd_bar(args: &[String]) {
    let scope = if args.iter().any(|a| a == "--all") {
        bar::Scope::Everything
    } else {
        bar::Scope::AiOnly
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
    println!("{}", bar::render(&inv, session.as_deref(), scope));
}

fn cmd_status(args: &[String]) {
    let states = state::all();
    let panes = tmux::list_panes();
    let mut inv = bar::inventory(&states, &panes);
    infer::decorate(&mut inv);
    let json = args.iter().any(|a| a == "--json");

    if json {
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
        return;
    }

    if inv.iter().all(|e| !e.kind.is_ai()) {
        println!("no AI sessions");
    }
    for row in bar::tree(&inv, tmux::current_session().as_deref()) {
        match row {
            bar::TreeRow::Session(name) => println!("{name}"),
            bar::TreeRow::Window { index, name } => println!("  {index} {name}"),
            bar::TreeRow::Pane(i) => {
                let e = &inv[i];
                println!(
                    "    {}{} {:<24} {:<11} {:>5}  {}{}",
                    e.glyph(),
                    if e.is_inferred() { "~" } else { " " },
                    truncate(&e.title(), 24),
                    e.status()
                        .map(|s| s.label())
                        .unwrap_or_else(|| e.kind.label()),
                    e.state
                        .map(|s| state::fmt_age(s.age_secs()))
                        .unwrap_or_else(|| if e.is_inferred() { "~" } else { "-" }.to_string()),
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
        }
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

fn cmd_doctor() {
    let mut problems = 0;

    match tmux::available() {
        true => println!("ok    tmux found"),
        false => {
            println!("FAIL  tmux not on PATH");
            problems += 1;
        }
    }

    let dir = state::dir();
    match std::fs::create_dir_all(&dir) {
        Ok(()) => println!("ok    state dir {}", dir.display()),
        Err(e) => {
            println!("FAIL  state dir {}: {e}", dir.display());
            problems += 1;
        }
    }

    let settings = std::env::var("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".claude/settings.json"))
        .unwrap_or_default();
    match std::fs::read_to_string(&settings) {
        Ok(body) if body.contains("atc hook") => {
            println!("ok    hooks registered in {}", settings.display())
        }
        Ok(_) => {
            println!(
                "WARN  no 'atc hook' entries in {} — run 'atc install'",
                settings.display()
            );
            problems += 1;
        }
        Err(e) => {
            println!("WARN  cannot read {}: {e}", settings.display());
            problems += 1;
        }
    }

    match tmux::current_pane() {
        Some(p) => println!("ok    running inside tmux pane {p}"),
        None => println!("WARN  not inside tmux, so panes cannot be identified"),
    }

    let tracked = state::all().len();
    println!("ok    {tracked} tracked pane(s)");

    if problems > 0 {
        std::process::exit(1);
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

fn install_text() -> String {
    format!(
        "# 1. tmux — source this, or copy it into your tmux.conf\n\n{}\n\
         # 2. ~/.claude/settings.json — merge into the existing \"hooks\" object\n\n{}\n",
        include_str!("../tmux/atc.conf"),
        hooks_json()
    )
}

fn hooks_json() -> String {
    let events = [
        ("SessionStart", None),
        ("UserPromptSubmit", None),
        ("PreToolUse", None),
        ("PostToolUse", None),
        ("PermissionRequest", None),
        ("Stop", None),
        ("StopFailure", None),
        ("PostToolUseFailure", None),
        ("SessionEnd", None),
        ("SubagentStart", None),
        ("SubagentStop", None),
        ("Notification", Some("permission_prompt")),
        ("Notification", Some("idle_prompt")),
    ];
    let mut out = String::from("{\n  \"hooks\": {\n");
    let mut notification: Vec<String> = Vec::new();
    let mut plain: Vec<String> = Vec::new();
    for (event, kind) in events {
        match kind {
            Some(k) => notification.push(format!(
                "      {{ \"matcher\": \"{k}\", \"hooks\": [{{ \"type\": \"command\", \"command\": \"atc hook Notification --kind {k}\" }}] }}"
            )),
            None => plain.push(format!(
                "    \"{event}\": [\n      {{ \"hooks\": [{{ \"type\": \"command\", \"command\": \"atc hook {event}\" }}] }}\n    ]"
            )),
        }
    }
    plain.push(format!(
        "    \"Notification\": [\n{}\n    ]",
        notification.join(",\n")
    ));
    out.push_str(&plain.join(",\n"));
    out.push_str("\n  }\n}");
    out
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
    fn generated_hooks_json_is_valid_and_covers_both_notification_matchers() {
        let v: serde_json::Value = serde_json::from_str(&hooks_json()).expect("valid JSON");
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
