use std::process::Command;

const SEP: char = '\u{1f}';

#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub pane_id: String,
    pub session: String,
    pub window_index: u32,
    pub window_name: String,
    pub pane_index: u32,
    pub active: bool,
    pub title: String,
    pub path: String,
}

impl PaneInfo {
    pub fn address(&self) -> String {
        format!("{}.{}", self.window_index, self.pane_index)
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

pub fn available() -> bool {
    run(&["-V"]).is_some()
}

pub fn list_panes() -> Vec<PaneInfo> {
    let fmt = format!(
        "#{{pane_id}}{s}#{{session_name}}{s}#{{window_index}}{s}#{{window_name}}{s}\
         #{{pane_index}}{s}#{{?pane_active,1,0}}{s}#{{pane_title}}{s}#{{pane_current_path}}",
        s = SEP
    );
    let Some(out) = run(&["list-panes", "-a", "-F", &fmt]) else {
        return Vec::new();
    };
    out.lines().filter_map(parse_pane_line).collect()
}

fn parse_pane_line(line: &str) -> Option<PaneInfo> {
    let f: Vec<&str> = line.split(SEP).collect();
    if f.len() < 8 {
        return None;
    }
    Some(PaneInfo {
        pane_id: f[0].to_string(),
        session: f[1].to_string(),
        window_index: f[2].parse().ok()?,
        window_name: f[3].to_string(),
        pane_index: f[4].parse().ok()?,
        active: f[5] == "1",
        title: f[6].to_string(),
        path: f[7].to_string(),
    })
}

pub fn current_pane() -> Option<String> {
    std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())
}

pub fn current_session() -> Option<String> {
    let out = run(&["display-message", "-p", "#{session_name}"])?;
    let s = out.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Pane options carry per-pane state; a pane-id target with `-w` resolves to
/// the containing window, which avoids quoting window names.
pub fn set_pane_option(pane: &str, key: &str, value: &str) {
    fire(&["set-option", "-p", "-t", pane, key, value]);
}

pub fn unset_pane_option(pane: &str, key: &str) {
    fire(&["set-option", "-pu", "-t", pane, key]);
}

pub fn set_window_option(target: &str, key: &str, value: &str) {
    fire(&["set-option", "-w", "-t", target, key, value]);
}

pub fn unset_window_option(target: &str, key: &str) {
    fire(&["set-option", "-wu", "-t", target, key]);
}

/// Status lines only redraw on their own interval, so nudge every client to
/// repaint the moment state changes.
pub fn refresh_client() {
    fire(&["refresh-client", "-S"]);
}

pub fn send_keys(pane: &str, text: &str) {
    fire(&["send-keys", "-t", pane, text, "Enter"]);
}

pub fn kill_pane(pane: &str) {
    fire(&["kill-pane", "-t", pane]);
}

pub fn focus(pane: &str, session: &str, window_index: u32) {
    fire(&["switch-client", "-t", session]);
    fire(&["select-window", "-t", &format!("{}:{}", session, window_index)]);
    fire(&["select-pane", "-t", pane]);
}

/// Claude Code sets the pane title to "✳ <task>"; keep just the task.
pub fn clean_title(title: &str) -> String {
    title
        .trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string()
}

/// `#[` and `#{` are markup in status formats, so a window called "c#1" would
/// otherwise corrupt the row.
pub fn escape_format(s: &str) -> String {
    s.replace('#', "##")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_pane_line() {
        let line = format!(
            "%6{s}myworkspace{s}0{s}nvim{s}1{s}1{s}✳ fix-lint{s}/Users/c/dev/atc",
            s = SEP
        );
        let p = parse_pane_line(&line).expect("should parse");
        assert_eq!(p.pane_id, "%6");
        assert_eq!(p.session, "myworkspace");
        assert_eq!(p.window_index, 0);
        assert_eq!(p.pane_index, 1);
        assert!(p.active);
        assert_eq!(p.address(), "0.1");
        assert_eq!(p.path, "/Users/c/dev/atc");
    }

    #[test]
    fn rejects_truncated_lines() {
        assert!(parse_pane_line("%6\u{1f}only\u{1f}three").is_none());
    }

    #[test]
    fn strips_claude_decoration_from_pane_titles() {
        assert_eq!(clean_title("✳ fix-pyright-lint-errors"), "fix-pyright-lint-errors");
        assert_eq!(clean_title("plain-title"), "plain-title");
        assert_eq!(clean_title(""), "");
    }

    #[test]
    fn escapes_hashes_for_status_format() {
        assert_eq!(escape_format("c#1"), "c##1");
        assert_eq!(escape_format("plain"), "plain");
    }
}
