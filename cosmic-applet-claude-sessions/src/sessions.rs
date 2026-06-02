// SPDX-License-Identifier: GPL-3.0-only

//! Reads Claude Code's live session registry from `~/.claude/sessions/*.json`,
//! deriving a per-session state (busy / idle / waiting-for-you) from the
//! `status` field.
//!
//! NOTE: the `sessions/` JSON layout is an internal Claude Code detail (it
//! carries a `version` field) and may change between releases.

use serde::Deserialize;
use std::path::PathBuf;

/// Raw shape of a `~/.claude/sessions/<pid>.json` file. Unknown fields
/// (procStart, version, peerProtocol, entrypoint, jobId, …) are ignored.
#[derive(Debug, Clone, Deserialize)]
struct RawSession {
    pid: i32,
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    kind: String,
    /// Claude's conversation topic; also what it sets the terminal title to.
    /// Often absent until a session has been auto-named.
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Waiting for the user (permission prompt / idle-input), per a hook marker.
    Attention,
    /// Actively working.
    Busy,
    /// Alive but idle.
    Idle,
}

impl SessionState {
    /// Sort key, lower = higher priority (shown first).
    pub fn priority(self) -> u8 {
        match self {
            Self::Attention => 0,
            Self::Busy => 1,
            Self::Idle => 2,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub pid: i32,
    pub session_id: String,
    pub cwd: String,
    /// Topic name (matches the terminal window title); None if unnamed.
    pub name: Option<String>,
    /// Controlling terminal as "pts/N"; matches the "[pts/N]" in the window
    /// title. None if it can't be read.
    pub tty: Option<String>,
    pub state: SessionState,
}

impl Session {
    /// The final path component of `cwd` (the project dir).
    pub fn project(&self) -> &str {
        self.cwd
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(self.cwd.as_str())
    }
}

/// Aggregate state across all sessions, used to choose the panel icon.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Overall {
    Attention,
    Busy,
    Idle,
    #[default]
    None,
}

impl Overall {
    /// Symbolic icon name from the freedesktop/hicolor theme.
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Attention => "dialog-warning-symbolic",
            Self::Busy => "emblem-synchronizing-symbolic",
            Self::Idle => "user-available-symbolic",
            Self::None => "user-offline-symbolic",
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn claude_dir() -> PathBuf {
    home().join(".claude")
}

/// True if a process with this pid currently exists.
fn pid_alive(pid: i32) -> bool {
    PathBuf::from(format!("/proc/{pid}")).exists()
}

/// The controlling terminal of `pid` as "pts/N", read from /proc/<pid>/fd.
/// None if it can't be determined.
fn tty_of(pid: i32) -> Option<String> {
    for fd in ["0", "1", "2"] {
        if let Ok(path) = std::fs::read_link(format!("/proc/{pid}/fd/{fd}")) {
            if let Some(s) = path.to_str() {
                if let Some(i) = s.find("pts/") {
                    return Some(s[i..].to_string());
                }
            }
        }
    }
    None
}

/// Load all live interactive sessions, sorted attention → busy → idle then by
/// project name. `show_idle == false` drops idle sessions from the result.
pub fn load(show_idle: bool) -> Vec<Session> {
    let dir = claude_dir().join("sessions");
    let mut out = Vec::new();

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(raw) = serde_json::from_str::<RawSession>(&text) else {
            continue;
        };

        // Only terminal sessions; skip stale files for dead processes.
        if raw.kind != "interactive" || !pid_alive(raw.pid) {
            continue;
        }

        let state = match raw.status.as_str() {
            "waiting" => SessionState::Attention,
            "busy" => SessionState::Busy,
            _ => SessionState::Idle,
        };

        if state == SessionState::Idle && !show_idle {
            continue;
        }

        out.push(Session {
            pid: raw.pid,
            session_id: raw.session_id,
            cwd: raw.cwd,
            name: raw.name.filter(|s| !s.is_empty()),
            tty: tty_of(raw.pid),
            state,
        });
    }

    out.sort_by(|a, b| {
        a.state
            .priority()
            .cmp(&b.state.priority())
            .then_with(|| a.project().cmp(b.project()))
    });
    out
}

/// Compute the panel-icon aggregate from already-loaded sessions.
/// Note: pass sessions loaded with `show_idle = true` so idle sessions still
/// register here even when hidden from the popup.
pub fn overall(sessions: &[Session]) -> Overall {
    let mut any = false;
    let mut busy = false;
    for s in sessions {
        any = true;
        match s.state {
            SessionState::Attention => return Overall::Attention,
            SessionState::Busy => busy = true,
            SessionState::Idle => {}
        }
    }
    if busy {
        Overall::Busy
    } else if any {
        Overall::Idle
    } else {
        Overall::None
    }
}
