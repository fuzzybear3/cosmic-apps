// SPDX-License-Identifier: GPL-3.0-only

//! Native click-to-focus. Raises a session's terminal window by speaking
//! COSMIC's toplevel protocols directly (via cctk) — no external helper, no
//! Python. If the session lives in a tmux pane we switch tmux to that pane
//! first (best-effort, via the `tmux` CLI).
//!
//! A dedicated background thread owns a second Wayland connection: it tracks
//! toplevels (app_id + title) through `ext-foreign-toplevel-list` +
//! `cosmic-toplevel-info`, and on a `FocusRequest` calls
//! `zcosmic_toplevel_manager_v1.activate(toplevel, seat)`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use calloop_wayland_source::WaylandSource;
use sctk::{
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    seat::{Capability, SeatHandler, SeatState},
};
use cosmic_client_toolkit::{
    cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1,
    toplevel_info::{ToplevelInfoHandler, ToplevelInfoState},
    toplevel_management::{ToplevelManagerHandler, ToplevelManagerState},
    wayland_client::{
        globals::registry_queue_init,
        protocol::{wl_output, wl_seat},
        Connection, QueueHandle, WEnum,
    },
    wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1,
};

/// A request to focus the terminal window for a session.
#[derive(Debug, Clone)]
pub struct FocusRequest {
    /// The session's controlling tty, e.g. "pts/10". Resolved against the
    /// captured tty -> window mapping.
    pub tty: String,
}

pub type FocusSender = calloop::channel::Sender<FocusRequest>;

/// Shared map (focus thread -> UI thread) of tty "pts/N" -> the window's current
/// title (Claude's task topic). Empty string while the window is at the shell
/// prompt (title is "[pts/N] …", no active task).
pub type SharedTitles = Arc<Mutex<HashMap<String, String>>>;

/// Spawn the background Wayland thread. Returns a channel for focus requests and
/// a shared map of tty -> live window title. Sends are fire-and-forget; if the
/// compositor lacks the protocols the thread exits and the map stays empty.
pub fn spawn() -> Option<(FocusSender, SharedTitles)> {
    let (tx, channel) = calloop::channel::channel::<FocusRequest>();
    let titles: SharedTitles = Arc::new(Mutex::new(HashMap::new()));
    let titles_thread = titles.clone();
    std::thread::Builder::new()
        .name("claude-focus-wl".into())
        .spawn(move || {
            if let Err(e) = run(channel, titles_thread) {
                tracing::warn!("focus wayland thread exited: {e}");
            }
        })
        .ok()?;
    Some((tx, titles))
}

// ─── tmux ────────────────────────────────────────────────────────────────────

/// Run a command, returning stdout on success. None on spawn failure or
/// non-zero exit.
async fn run_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new(cmd)
        .args(args)
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The controlling tty of a session's pid, e.g. "pts/10". None if it has none.
/// This is the per-session key the window title is matched on.
pub async fn session_tty(pid: i32) -> Option<String> {
    let out = run_cmd("ps", &["-o", "tty=", "-p", &pid.to_string()]).await?;
    let t = out.trim();
    (!t.is_empty() && t != "?").then(|| t.to_string())
}

/// If `pid` runs inside a tmux pane, switch tmux to it. Best-effort; no-op when
/// tmux isn't installed or the session isn't in a pane.
pub async fn tmux_switch(pid: i32) {
    let Some(tty) = session_tty(pid).await else {
        return;
    };

    let want = format!("/dev/{tty}");
    let fmt = "#{pane_tty} #{session_name}:#{window_index}.#{pane_index}";
    let Some(out) = run_cmd("tmux", &["list-panes", "-a", "-F", fmt]).await else {
        return;
    };

    for line in out.lines() {
        let Some((pane_tty, coords)) = line.split_once(' ') else {
            continue;
        };
        if pane_tty != want {
            continue;
        }
        if let Some((session, rest)) = coords.split_once(':') {
            if let Some((window, pane)) = rest.split_once('.') {
                let tw = format!("{session}:{window}");
                let tp = format!("{session}:{window}.{pane}");
                let _ = run_cmd("tmux", &["select-window", "-t", &tw]).await;
                let _ = run_cmd("tmux", &["select-pane", "-t", &tp]).await;
                let _ = run_cmd("tmux", &["switch-client", "-t", session]).await;
            }
        }
        break;
    }
}

// ─── Wayland background connection ───────────────────────────────────────────

struct AppData {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    toplevel_info: ToplevelInfoState,
    toplevel_manager: ToplevelManagerState,
    conn: Connection,
    /// Captured mapping: tty ("pts/N") -> the window's stable foreign-toplevel
    /// `identifier`. Filled whenever a title shows "[pts/N]" (while the shell
    /// owns it), and survives Claude later overwriting the title with a topic.
    tty_to_identifier: HashMap<String, String>,
    /// Published to the UI thread: tty -> current window title (the task topic).
    titles: SharedTitles,
}

fn run(
    channel: calloop::channel::Channel<FocusRequest>,
    titles: SharedTitles,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init::<AppData>(&conn)?;
    let qh = event_queue.handle();

    let registry_state = RegistryState::new(&globals);
    let toplevel_info = ToplevelInfoState::try_new(&registry_state, &qh)
        .ok_or("cosmic-toplevel-info protocol unavailable")?;
    let toplevel_manager = ToplevelManagerState::try_new(&registry_state, &qh)
        .ok_or("cosmic-toplevel-management protocol unavailable")?;

    let mut app = AppData {
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        toplevel_info,
        toplevel_manager,
        registry_state,
        conn: conn.clone(),
        tty_to_identifier: HashMap::new(),
        titles,
    };

    let mut event_loop = calloop::EventLoop::<AppData>::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue)
        .insert(handle.clone())
        .map_err(|e| format!("wayland source: {e}"))?;
    handle
        .insert_source(channel, |event, _, app| {
            if let calloop::channel::Event::Msg(req) = event {
                activate_matching(app, &req);
            }
        })
        .map_err(|e| format!("channel source: {e}"))?;

    loop {
        event_loop.dispatch(None, &mut app)?;
    }
}

/// Extract "pts/N" from a title like "[pts/10] ~/dir". None if not present.
fn extract_pts(title: &str) -> Option<String> {
    let start = title.find("[pts/")?;
    let rest = &title[start + 1..];
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

/// Activate the window mapped to the request's tty. The tty -> identifier
/// mapping was captured while the shell painted "[pts/N]" into the title; we
/// resolve the (stable) identifier to a current toplevel handle, so it still
/// works after Claude has overwritten the title with its topic.
fn activate_matching(app: &mut AppData, req: &FocusRequest) {
    let Some(seat) = app.seat_state.seats().next() else {
        return;
    };
    let Some(identifier) = app.tty_to_identifier.get(&req.tty).cloned() else {
        return;
    };
    let handle = app
        .toplevel_info
        .toplevels()
        .find(|info| info.identifier == identifier)
        .and_then(|info| info.cosmic_toplevel.clone());

    if let Some(handle) = handle {
        app.toplevel_manager.manager.activate(&handle, &seat);
        let _ = app.conn.flush();
    }
}

// ─── Wayland handler boilerplate ─────────────────────────────────────────────

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers!(OutputState, SeatState);
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for AppData {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl ToplevelInfoHandler for AppData {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info
    }
    fn new_toplevel(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        self.update_toplevel(conn, qh, handle);
    }
    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        let Some((title, identifier)) = self
            .toplevel_info
            .info(handle)
            .map(|info| (info.title.clone(), info.identifier.clone()))
        else {
            return;
        };

        if let Some(pts) = extract_pts(&title) {
            // Shell-owned title "[pts/N] …": capture the tty -> window mapping,
            // and mark this window as having no active task.
            self.tty_to_identifier.insert(pts.clone(), identifier);
            if let Ok(mut m) = self.titles.lock() {
                m.insert(pts, String::new());
            }
        } else {
            // A non-shell title is Claude's task topic. Publish it for whichever
            // tty maps to this (already-captured) window.
            let pts = self
                .tty_to_identifier
                .iter()
                .find(|(_, id)| **id == identifier)
                .map(|(p, _)| p.clone());
            if let Some(pts) = pts {
                if let Ok(mut m) = self.titles.lock() {
                    m.insert(pts, title);
                }
            }
        }
    }
    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        handle: &ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ) {
        let Some(identifier) = self.toplevel_info.info(handle).map(|info| info.identifier.clone())
        else {
            return;
        };
        let stale: Vec<String> = self
            .tty_to_identifier
            .iter()
            .filter(|(_, id)| **id == identifier)
            .map(|(pts, _)| pts.clone())
            .collect();
        self.tty_to_identifier.retain(|_, v| *v != identifier);
        if let Ok(mut m) = self.titles.lock() {
            for pts in stale {
                m.remove(&pts);
            }
        }
    }
}

impl ToplevelManagerHandler for AppData {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        &mut self.toplevel_manager
    }
    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: Vec<WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>>,
    ) {
    }
}

sctk::delegate_registry!(AppData);
sctk::delegate_output!(AppData);
sctk::delegate_seat!(AppData);
cosmic_client_toolkit::delegate_toplevel_info!(AppData);
cosmic_client_toolkit::delegate_toplevel_manager!(AppData);
