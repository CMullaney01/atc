//! The only place that shells out to tmux. Everything else works on the values
//! this hands back, so the rest of atc can be tested with no tmux server.
use crate::process::{base_command, command_lines, is_interpreter};
use std::process::Command;

const SEP: char = '\u{1f}';

#[derive(Debug, Clone)]
pub(crate) struct PaneInfo {
    pub(crate) pane_id: String,
    pub(crate) session: String,
    pub(crate) window_index: u32,
    pub(crate) window_name: String,
    pub(crate) pane_index: u32,
    pub(crate) active: bool,
    pub(crate) window_active: bool,
    pub(crate) session_attached: bool,
    pub(crate) title: String,
    pub(crate) command: String,
    pub(crate) path: String,
    /// Without the "/dev/" prefix, which is how `ps` reports it.
    pub(crate) tty: String,
    /// The full command line, but only for a pane running an interpreter —
    /// empty for every other pane, since nothing else needs it.
    pub(crate) argv: String,
}

impl PaneInfo {
    pub(crate) fn address(&self) -> String {
        format!("{}.{}", self.window_index, self.pane_index)
    }

    /// `pane_active` is per-window, so on its own it is true for one pane in
    /// every window. The pane the user is actually sitting in is the active
    /// pane of the active window of an attached session.
    pub(crate) fn is_current(&self) -> bool {
        self.active && self.window_active && self.session_attached
    }
}

fn run(args: &[&str]) -> Option<String> {
    let out = Command::new("tmux").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

fn fire(args: &[&str]) {
    let _ = Command::new("tmux").args(args).output();
}

pub(crate) fn available() -> bool {
    run(&["-V"]).is_some()
}

pub(crate) fn list_panes() -> Vec<PaneInfo> {
    let fmt = format!(
        "#{{pane_id}}{s}#{{session_name}}{s}#{{window_index}}{s}#{{window_name}}{s}\
         #{{pane_index}}{s}#{{?pane_active,1,0}}{s}#{{?window_active,1,0}}{s}\
         #{{?session_attached,1,0}}{s}#{{pane_title}}{s}\
         #{{pane_current_command}}{s}#{{pane_current_path}}{s}#{{pane_tty}}",
        s = SEP
    );
    let Some(out) = run(&["list-panes", "-a", "-F", &fmt]) else {
        return Vec::new();
    };
    let mut panes: Vec<PaneInfo> = out.lines().filter_map(parse_pane_line).collect();
    fill_command_lines(&mut panes);
    panes
}

/// An agent installed from npm runs as `node`, so its pane says nothing about
/// which agent it is. Read the command line of those panes, and only those.
fn fill_command_lines(panes: &mut [PaneInfo]) {
    if !panes
        .iter()
        .any(|p| is_interpreter(base_command(&p.command)))
    {
        return;
    }
    let lines = command_lines();
    for pane in panes.iter_mut() {
        if let Some(argv) = lines.get(&pane.tty) {
            pane.argv = argv.clone();
        }
    }
}

fn parse_pane_line(line: &str) -> Option<PaneInfo> {
    let f: Vec<&str> = line.split(SEP).collect();
    if f.len() < 12 {
        return None;
    }
    Some(PaneInfo {
        pane_id: f[0].to_string(),
        session: f[1].to_string(),
        window_index: f[2].parse().ok()?,
        window_name: f[3].to_string(),
        pane_index: f[4].parse().ok()?,
        active: f[5] == "1",
        window_active: f[6] == "1",
        session_attached: f[7] == "1",
        title: f[8].to_string(),
        command: f[9].to_string(),
        path: f[10].to_string(),
        tty: f[11].trim_start_matches("/dev/").to_string(),
        argv: String::new(),
    })
}

pub(crate) fn current_pane() -> Option<String> {
    std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())
}

pub(crate) fn current_session() -> Option<String> {
    let out = run(&["display-message", "-p", "#{session_name}"])?;
    let s = out.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Pane options carry per-pane state; a pane-id target with `-w` resolves to
/// the containing window, which avoids quoting window names.
pub(crate) fn set_pane_option(pane: &str, key: &str, value: &str) {
    fire(&["set-option", "-p", "-t", pane, key, value]);
}

pub(crate) fn unset_pane_option(pane: &str, key: &str) {
    fire(&["set-option", "-pu", "-t", pane, key]);
}

/// A global option's value, or `None` when it has never been set. `-q` keeps
/// tmux quiet about options it does not know.
pub(crate) fn global_option(key: &str) -> Option<String> {
    let v = run(&["show-options", "-gqv", key])?;
    let v = v.trim().to_string();
    (!v.is_empty()).then_some(v)
}

pub(crate) fn set_window_option(target: &str, key: &str, value: &str) {
    fire(&["set-option", "-w", "-t", target, key, value]);
}

pub(crate) fn unset_window_option(target: &str, key: &str) {
    fire(&["set-option", "-wu", "-t", target, key]);
}

/// Status lines only redraw on their own interval, so nudge every client to
/// repaint the moment state changes.
pub(crate) fn refresh_client() {
    fire(&["refresh-client", "-S"]);
}

pub(crate) fn send_keys(pane: &str, text: &str) {
    fire(&["send-keys", "-t", pane, text, "Enter"]);
}

pub(crate) fn kill_pane(pane: &str) {
    fire(&["kill-pane", "-t", pane]);
}

/// What is on a pane's screen right now, top line first.
///
/// Colour is left out on purpose: the capture is drawn inside a widget that
/// styles it, and tmux's escapes would fight that.
pub(crate) fn capture_pane(pane: &str) -> Vec<String> {
    let raw = run(&["capture-pane", "-p", "-t", pane]).unwrap_or_default();
    trim_blank_tail(&raw)
}

/// A pane's screen is padded with blank lines to its full height. Dropping them
/// lets the caller show the last real line of output at the bottom.
fn trim_blank_tail(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = raw.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

pub(crate) fn focus(pane: &str, session: &str, window_index: u32) {
    fire(&["switch-client", "-t", session]);
    fire(&[
        "select-window",
        "-t",
        &format!("{}:{}", session, window_index),
    ]);
    fire(&["select-pane", "-t", pane]);
}

/// Claude Code sets the pane title to "✳ <task>"; keep just the task.
pub(crate) fn clean_title(title: &str) -> String {
    title
        .trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string()
}

/// `#[` and `#{` are markup in status formats, so a window called "c#1" would
/// otherwise corrupt the row.
pub(crate) fn escape_format(s: &str) -> String {
    s.replace('#', "##")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_keeps_blank_lines_inside_but_not_at_the_end() {
        let got = trim_blank_tail("one\n\ntwo   \n\n\n");
        assert_eq!(got, vec!["one", "", "two"]);
    }

    #[test]
    fn capture_of_an_empty_pane_is_empty() {
        assert!(trim_blank_tail("\n\n\n").is_empty());
        assert!(trim_blank_tail("").is_empty());
    }

    #[test]
    fn parses_a_pane_line() {
        let line = format!(
            "%6{s}myworkspace{s}0{s}nvim{s}1{s}1{s}1{s}1{s}✳ fix-lint{s}2.1.308\
             {s}/Users/c/dev/atc{s}/dev/ttys004",
            s = SEP
        );
        let p = parse_pane_line(&line).expect("should parse");
        assert_eq!(p.pane_id, "%6");
        assert_eq!(p.session, "myworkspace");
        assert_eq!(p.window_index, 0);
        assert_eq!(p.pane_index, 1);
        assert!(p.active);
        assert!(
            p.is_current(),
            "active pane of an active window in an attached session"
        );
        assert_eq!(p.address(), "0.1");
        assert_eq!(p.command, "2.1.308");
        assert_eq!(p.path, "/Users/c/dev/atc");
        // Stored the way `ps` reports it, so a pane can be matched to its
        // process without re-trimming at every use.
        assert_eq!(p.tty, "ttys004");
    }

    #[test]
    fn an_active_pane_in_a_background_window_is_not_current() {
        let line = format!(
            "%9{s}ws{s}2{s}other{s}0{s}1{s}0{s}1{s}title{s}zsh{s}/tmp{s}/dev/ttys009",
            s = SEP
        );
        let p = parse_pane_line(&line).expect("should parse");
        assert!(p.active, "it is active within its own window");
        assert!(!p.is_current(), "but that window is not the active one");
    }

    #[test]
    fn rejects_truncated_lines() {
        assert!(parse_pane_line("%6\u{1f}only\u{1f}three").is_none());
    }

    #[test]
    fn strips_claude_decoration_from_pane_titles() {
        assert_eq!(
            clean_title("✳ fix-pyright-lint-errors"),
            "fix-pyright-lint-errors"
        );
        assert_eq!(clean_title("plain-title"), "plain-title");
        assert_eq!(clean_title(""), "");
    }

    #[test]
    fn escapes_hashes_for_status_format() {
        assert_eq!(escape_format("c#1"), "c##1");
        assert_eq!(escape_format("plain"), "plain");
    }
}
