//! Keeps what each pane is doing in one small file per pane. The status row, the
//! popup and the hooks all read the same picture, with no daemon to keep running.
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Idle,
    Working,
    Waiting,
    Blocked,
    Error,
}

impl Status {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Status::Idle => "·",
            Status::Working => "◐",
            Status::Waiting => "●",
            Status::Blocked => "!",
            Status::Error => "✗",
        }
    }

    /// Drawn on the status bar, which is light unless you have restyled it, so
    /// these are dark enough to read against it.
    pub(crate) fn colour(self) -> &'static str {
        match self {
            Status::Idle => "colour239",
            Status::Working => "colour19",
            Status::Waiting => "colour88",
            Status::Blocked => "colour90",
            Status::Error => "colour160",
        }
    }

    /// Highest urgency wins when collapsing a window's panes into one glyph.
    pub(crate) fn urgency(self) -> u8 {
        match self {
            Status::Idle => 0,
            Status::Working => 1,
            Status::Waiting => 2,
            Status::Blocked => 3,
            Status::Error => 4,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Working => "working",
            Status::Waiting => "waiting",
            Status::Blocked => "blocked",
            Status::Error => "error",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Status> {
        match s {
            "idle" => Some(Status::Idle),
            "working" => Some(Status::Working),
            "waiting" => Some(Status::Waiting),
            "blocked" => Some(Status::Blocked),
            "error" => Some(Status::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PaneState {
    pub(crate) pane_id: String,
    pub(crate) status: Status,
    pub(crate) since: u64,
    #[serde(default)]
    pub(crate) stale: bool,
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default)]
    pub(crate) tool: Option<String>,
    #[serde(default)]
    pub(crate) last_message: Option<String>,
    #[serde(default = "default_source")]
    pub(crate) source: String,
}

fn default_source() -> String {
    "claude".to_string()
}

impl PaneState {
    pub(crate) fn new(pane_id: &str) -> Self {
        PaneState {
            pane_id: pane_id.to_string(),
            status: Status::Idle,
            since: now(),
            stale: false,
            session_id: None,
            title: None,
            cwd: None,
            tool: None,
            last_message: None,
            source: default_source(),
        }
    }

    pub(crate) fn age_secs(&self) -> u64 {
        now().saturating_sub(self.since)
    }
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn dir() -> PathBuf {
    if let Ok(d) = std::env::var("ATC_STATE_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local/state/atc/panes")
}

/// tmux pane ids look like "%6"; keep filenames boring.
fn file_stem(pane_id: &str) -> String {
    pane_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn path_for(pane_id: &str) -> PathBuf {
    dir().join(format!("{}.json", file_stem(pane_id)))
}

pub(crate) fn load(pane_id: &str) -> Option<PaneState> {
    let raw = fs::read_to_string(path_for(pane_id)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub(crate) fn save(st: &PaneState) -> io::Result<()> {
    let d = dir();
    fs::create_dir_all(&d)?;
    let body = serde_json::to_string(st).map_err(io::Error::other)?;
    let tmp = d.join(format!(
        ".{}.{}.tmp",
        file_stem(&st.pane_id),
        std::process::id()
    ));
    fs::write(&tmp, body)?;
    fs::rename(&tmp, path_for(&st.pane_id))
}

pub(crate) fn remove(pane_id: &str) -> io::Result<()> {
    match fs::remove_file(path_for(pane_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Removes state for panes tmux no longer reports. SessionEnd covers the
/// ordinary case; this catches panes killed outright or sessions that crashed.
pub(crate) fn prune(live_pane_ids: &[String]) -> usize {
    let mut removed = 0;
    for st in all() {
        if !live_pane_ids.contains(&st.pane_id) && remove(&st.pane_id).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub(crate) fn all() -> Vec<PaneState> {
    let Ok(entries) = fs::read_dir(dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(st) = serde_json::from_str::<PaneState>(&raw) {
                out.push(st);
            }
        }
    }
    out
}

pub(crate) fn fmt_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_orders_blocked_above_waiting() {
        assert!(Status::Blocked.urgency() > Status::Waiting.urgency());
        assert!(Status::Error.urgency() > Status::Blocked.urgency());
        assert!(Status::Waiting.urgency() > Status::Working.urgency());
    }

    #[test]
    fn status_round_trips_through_parse() {
        for s in [
            Status::Idle,
            Status::Working,
            Status::Waiting,
            Status::Blocked,
            Status::Error,
        ] {
            assert_eq!(Status::parse(s.label()), Some(s));
        }
    }

    #[test]
    fn prune_removes_only_panes_that_are_gone() {
        let dir = std::env::temp_dir().join(format!("atc-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Tests run in parallel, so this must stay the only test that touches
        // ATC_STATE_DIR.
        unsafe { std::env::set_var("ATC_STATE_DIR", &dir) };

        save(&PaneState::new("%1")).unwrap();
        save(&PaneState::new("%2")).unwrap();
        assert_eq!(all().len(), 2);

        let removed = prune(&["%1".to_string()]);
        assert_eq!(removed, 1);
        let left: Vec<String> = all().into_iter().map(|s| s.pane_id).collect();
        assert_eq!(left, vec!["%1".to_string()]);

        std::fs::remove_dir_all(&dir).ok();
        unsafe { std::env::remove_var("ATC_STATE_DIR") };
    }

    #[test]
    fn pane_id_becomes_a_safe_filename() {
        assert_eq!(file_stem("%6"), "_6");
        assert_eq!(file_stem("../etc/passwd"), "___etc_passwd");
    }
}
