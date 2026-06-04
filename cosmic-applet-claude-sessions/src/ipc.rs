// SPDX-License-Identifier: GPL-3.0-only

//! IPC between the attention-card **daemon** (server) and the panel **applet**
//! (client). The daemon owns the only Wayland focus thread — the captured
//! tty→window map, the live titles, and the focused-window state. The applet
//! fetches that state once per refresh tick and sends click-to-focus requests.
//!
//! Why: cosmic-panel respawns its applets on every suspend/resume, and a fresh
//! applet can never re-capture the "[pts/N]" titles of sessions already mid-run
//! (Claude owns their titles). The daemon survives (systemd service), so it is
//! the single source of truth and the applet just asks.
//!
//! Protocol: newline-delimited JSON over a unix socket in $XDG_RUNTIME_DIR.
//!   → {"op":"state"}              ← {"titles":{"pts/3":"…"},"focused":"pts/5"}
//!   → {"op":"focus","pid":1234}   ← {"ok":true}

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use crate::focus::{self, FocusSender, FocusedTty, SharedTitles};

/// Runtime dir holding the socket and the persisted window map. Dies with the
/// login session — which matches the lifetime of foreign-toplevel identifiers,
/// so nothing here can go stale across logins.
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("cosmic-claude-sessions")
}

fn sock_path() -> PathBuf {
    runtime_dir().join("ipc.sock")
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    /// Snapshot of the daemon's focus state.
    State,
    /// Focus the session with this pid (tmux pane switch + raise its window).
    Focus { pid: i32 },
}

/// The daemon's focus-state snapshot, served to the applet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonState {
    /// tty "pts/N" → live window title (Claude's task topic; empty string
    /// while the window is at the shell prompt).
    pub titles: HashMap<String, String>,
    /// The focused window's tty, when it maps to a tracked session.
    pub focused: Option<String>,
}

// ─── Server (daemon side) ─────────────────────────────────────────────────────

/// Start the IPC server thread. Fire-and-forget; on bind failure the daemon
/// still works standalone (the applet just falls back to registry labels).
pub fn spawn_server(tx: FocusSender, titles: SharedTitles, focused: FocusedTty) {
    let _ = std::thread::Builder::new()
        .name("claude-ipc".into())
        .spawn(move || {
            if let Err(e) = serve(tx, titles, focused) {
                tracing::warn!("ipc server exited: {e}");
            }
        });
}

fn serve(tx: FocusSender, titles: SharedTitles, focused: FocusedTty) -> std::io::Result<()> {
    let path = sock_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Remove a stale socket from a previous (crashed) run before rebinding.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (tx, titles, focused) = (tx.clone(), titles.clone(), focused.clone());
        std::thread::spawn(move || handle(stream, tx, titles, focused));
    }
    Ok(())
}

/// Serve one client connection: a loop of request lines until EOF/timeout.
fn handle(stream: UnixStream, tx: FocusSender, titles: SharedTitles, focused: FocusedTty) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(&stream);
    let mut writer = &stream;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let Ok(req) = serde_json::from_str::<Request>(&line) else {
            continue;
        };
        let reply = match req {
            Request::State => {
                let state = DaemonState {
                    titles: titles.lock().map(|m| m.clone()).unwrap_or_default(),
                    focused: focused.lock().ok().and_then(|f| f.clone()),
                };
                serde_json::to_string(&state).unwrap_or_default()
            }
            Request::Focus { pid } => {
                focus::focus_session(pid, &tx);
                r#"{"ok":true}"#.to_string()
            }
        };
        if writer.write_all(reply.as_bytes()).is_err() || writer.write_all(b"\n").is_err() {
            return;
        }
    }
}

// ─── Client (applet side) ─────────────────────────────────────────────────────

/// Per-request budget. Short: the daemon is local and the applet polls at 1 s —
/// better to miss one tick than to stall the refresh stream.
const CLIENT_TIMEOUT: Duration = Duration::from_millis(300);

/// One request/response round trip on a fresh connection. Connecting per
/// request keeps the client stateless, so it recovers from daemon restarts
/// without any reconnect logic.
async fn request(line: String) -> Option<String> {
    tokio::time::timeout(CLIENT_TIMEOUT, async move {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
        let stream = tokio::net::UnixStream::connect(sock_path()).await.ok()?;
        let (read, mut write) = stream.into_split();
        write.write_all(line.as_bytes()).await.ok()?;
        write.write_all(b"\n").await.ok()?;
        let mut reply = String::new();
        tokio::io::BufReader::new(read)
            .read_line(&mut reply)
            .await
            .ok()?;
        Some(reply)
    })
    .await
    .ok()
    .flatten()
}

/// Fetch the daemon's focus state. None when the daemon is down — the caller
/// falls back to registry-only labels.
pub async fn fetch_state() -> Option<DaemonState> {
    let reply = request(r#"{"op":"state"}"#.to_string()).await?;
    serde_json::from_str(&reply).ok()
}

/// Ask the daemon to focus the session with this pid. Fire-and-forget: the
/// daemon does the tmux switch and the window raise even if we time out.
pub async fn send_focus(pid: i32) {
    let _ = request(format!(r#"{{"op":"focus","pid":{pid}}}"#)).await;
}
