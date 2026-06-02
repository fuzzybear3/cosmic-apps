// SPDX-License-Identifier: GPL-3.0-only

use crate::config::Config;
use crate::focus;
use crate::sessions::{self, Overall, Session, SessionState};
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::{stream, window::Id, Alignment, Color, Length, Limits, Subscription};
use cosmic::iced_winit::commands::popup::{destroy_popup, get_popup};
use cosmic::prelude::*;
use cosmic::widget;
use futures::channel::mpsc;
use futures_util::SinkExt as _;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::time::Instant;
use tokio::time;

// ─── App model ───────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct AppModel {
    core: cosmic::Core,
    popup: Option<Id>,
    config: Config,

    /// All live interactive sessions, sorted attention → busy → idle.
    sessions: Vec<Session>,

    /// Aggregate state shown as the panel icon.
    overall: Overall,

    /// When each currently-busy session entered the busy state, keyed by
    /// session id. Used to show a running-time counter; cleared when a session
    /// stops being busy.
    busy_since: HashMap<String, Instant>,

    /// Channel to the background Wayland thread that activates windows.
    focus_tx: Option<focus::FocusSender>,

    /// Shared tty -> live window title (Claude's task topic), from that thread.
    focus_titles: Option<focus::SharedTitles>,
}

// ─── Messages ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    Refresh,
    Focus { pid: i32 },
    UpdateConfig(Config),
    SetMuteFocused(bool),
}

// ─── Stream builders ─────────────────────────────────────────────────────────
//
// Module-level `fn` items with concrete return types so they can be coerced to
// `fn(&I) -> S` function pointers for `Subscription::run_with`. Distinct inputs
// (0u8 / 1u8) guarantee distinct subscription identities.

type MsgStream = Pin<Box<dyn futures_util::Stream<Item = Message> + Send + 'static>>;

/// 1 s heartbeat that drives a full re-scan of the registry and the
/// running-time counters.
fn refresh_stream(_: &u8) -> MsgStream {
    Box::pin(stream::channel(
        1,
        |mut tx: mpsc::Sender<Message>| async move {
            let mut timer = time::interval(time::Duration::from_secs(1));
            timer.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let _ = tx.send(Message::Refresh).await;
            }
        },
    ))
}

/// Listens for SIGUSR1 and toggles the popup — the hotkey path. Bind a COSMIC
/// custom shortcut to `pkill -USR1 -f cosmic-applet-claude-sessions`.
fn signal_stream(_: &u8) -> MsgStream {
    Box::pin(stream::channel(
        1,
        |mut tx: mpsc::Sender<Message>| async move {
            use tokio::signal::unix::{signal, SignalKind};
            if let Ok(mut sig) = signal(SignalKind::user_defined1()) {
                loop {
                    sig.recv().await;
                    let _ = tx.send(Message::TogglePopup).await;
                }
            }
        },
    ))
}

// ─── cosmic::Application impl ────────────────────────────────────────────────

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "io.github.fuzzybear3.CosmicAppletClaudeSessions";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let config = cosmic_config::Config::new(Self::APP_ID, Config::VERSION)
            .map(|ctx| match Config::get_entry(&ctx) {
                Ok(cfg) => cfg,
                Err((_errs, cfg)) => cfg,
            })
            .unwrap_or_default();

        let (focus_tx, focus_titles) = match focus::spawn() {
            Some((tx, titles, _focused)) => (Some(tx), Some(titles)),
            None => (None, None),
        };
        let app = AppModel {
            core,
            config,
            focus_tx,
            focus_titles,
            ..Default::default()
        };

        // Immediate first scan at startup.
        (
            app,
            Task::perform(async {}, |_| cosmic::Action::App(Message::Refresh)),
        )
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    // ── Panel button ─────────────────────────────────────────────────────────

    fn view(&self) -> Element<'_, Self::Message> {
        self.core
            .applet
            .icon_button(self.overall.icon_name())
            .on_press(Message::TogglePopup)
            .into()
    }

    // ── Popup window ─────────────────────────────────────────────────────────

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        let display: Vec<&Session> = self
            .sessions
            .iter()
            .filter(|s| self.config.show_idle || s.state != SessionState::Idle)
            .collect();

        let mut content = widget::column()
            .spacing(8)
            .padding(12)
            .push(widget::text(format!("Claude sessions ({})", display.len())).size(14));

        if display.is_empty() {
            content = content.push(widget::text("No active Claude sessions").size(12));
        } else {
            for s in display {
                let (color, status) = match s.state {
                    SessionState::Attention => {
                        (Color::from_rgb(0.85, 0.20, 0.20), "waiting".to_string())
                    }
                    SessionState::Busy => (
                        Color::from_rgb(1.00, 0.65, 0.00),
                        format!("busy {}", self.running_time(s)),
                    ),
                    SessionState::Idle => (Color::from_rgb(0.50, 0.50, 0.50), "idle".to_string()),
                };

                // Primary label: the live task (window title), else name, else
                // project. Dim sub-line shows the project dir unless it's already
                // the label.
                let primary = self.session_label(s);
                let sub = if primary.as_str() != s.project() {
                    s.project().to_string()
                } else {
                    String::new()
                };

                let mut info_col = widget::column()
                    .spacing(1)
                    .push(widget::text(primary).size(13));
                if !sub.is_empty() {
                    info_col = info_col.push(
                        widget::text(sub)
                            .size(10)
                            .class(Color::from_rgb(0.60, 0.60, 0.60)),
                    );
                }

                let row = widget::row()
                    .push(
                        widget::text("●")
                            .size(14)
                            .class(color)
                            .width(Length::Fixed(16.0)),
                    )
                    .push(info_col.width(Length::Fill))
                    .push(widget::text(status).size(11).class(color))
                    .spacing(8)
                    .align_y(Alignment::Center);

                content = content.push(
                    widget::button::custom(row)
                        .on_press(Message::Focus { pid: s.pid })
                        .width(Length::Fill)
                        .padding([6, 8]),
                );
            }
        }

        content = content.push(
            widget::toggler(self.config.mute_focused)
                .label("Mute the focused session".to_string())
                .on_toggle(Message::SetMuteFocused),
        );

        self.core.applet.popup_container(content).into()
    }

    // ── Subscriptions ─────────────────────────────────────────────────────────

    fn subscription(&self) -> Subscription<Self::Message> {
        let refresh = Subscription::run_with(0u8, refresh_stream);
        let signal = Subscription::run_with(1u8, signal_stream);
        let config_watch = self
            .core()
            .watch_config::<Config>(Self::APP_ID)
            .map(|update| Message::UpdateConfig(update.config));

        Subscription::batch([refresh, signal, config_watch])
    }

    // ── Message handler ───────────────────────────────────────────────────────

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::TogglePopup => {
                return if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    let new_id = Id::unique();
                    self.popup.replace(new_id);
                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    popup_settings.positioner.size_limits = Limits::NONE
                        .max_width(420.0)
                        .min_width(280.0)
                        .min_height(80.0)
                        .max_height(700.0);
                    get_popup(popup_settings)
                };
            }

            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }

            Message::Refresh => {
                let sessions = sessions::load(true);
                self.overall = sessions::overall(&sessions);

                // Maintain busy_since: keep only still-busy ids, stamp new ones.
                let busy_ids: HashSet<&str> = sessions
                    .iter()
                    .filter(|s| s.state == SessionState::Busy)
                    .map(|s| s.session_id.as_str())
                    .collect();
                self.busy_since.retain(|id, _| busy_ids.contains(id.as_str()));
                for s in &sessions {
                    if s.state == SessionState::Busy {
                        self.busy_since
                            .entry(s.session_id.clone())
                            .or_insert_with(Instant::now);
                    }
                }

                // Attention alerts are owned by the standalone
                // `cosmic-claude-attention-card` daemon (a custom layer-shell
                // card), not the applet. The applet just reflects the current
                // state in its panel icon and popup.
                self.sessions = sessions;
            }

            Message::Focus { pid } => {
                let tx = self.focus_tx.clone();
                let mut tasks = vec![Task::perform(
                    async move {
                        // Switch tmux to the pane first (best-effort), then ask
                        // the Wayland thread to raise the window mapped to this
                        // session's tty (captured from its "[pts/N]" title).
                        focus::tmux_switch(pid).await;
                        // Let the popup finish closing and its focus return
                        // settle before we activate, so activation is the LAST
                        // focus change — otherwise the popup-close refocus can
                        // land on the previous workspace's window.
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                        if let (Some(tx), Some(tty)) = (tx, focus::session_tty(pid).await) {
                            let _ = tx.send(focus::FocusRequest { tty });
                        }
                    },
                    |_| cosmic::Action::App(Message::Refresh),
                )];
                if let Some(p) = self.popup.take() {
                    tasks.push(destroy_popup(p));
                }
                return Task::batch(tasks);
            }

            Message::UpdateConfig(new_config) => {
                self.config = new_config;
                return self.update(Message::Refresh);
            }

            Message::SetMuteFocused(value) => {
                // Persist to cosmic-config; the daemon watches the same key.
                match cosmic_config::Config::new(Self::APP_ID, Config::VERSION) {
                    Ok(ctx) => {
                        let _ = self.config.set_mute_focused(&ctx, value);
                    }
                    Err(_) => self.config.mute_focused = value,
                }
            }
        }

        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

impl AppModel {
    /// "m:ss" since the session became busy, or "0:00" if unknown.
    fn running_time(&self, s: &Session) -> String {
        let secs = self
            .busy_since
            .get(&s.session_id)
            .map(|t| Instant::now().duration_since(*t).as_secs())
            .unwrap_or(0);
        format!("{}:{:02}", secs / 60, secs % 60)
    }

    /// Display label: the live task from the mapped window title (glyph
    /// stripped), else the session's registry name, else the project dir.
    fn session_label(&self, s: &Session) -> String {
        s.tty
            .as_ref()
            .and_then(|tty| {
                self.focus_titles
                    .as_ref()
                    .and_then(|t| t.lock().ok().and_then(|m| m.get(tty).cloned()))
            })
            .map(|t| strip_glyph(&t))
            .filter(|t| !t.is_empty())
            .or_else(|| s.name.clone())
            .unwrap_or_else(|| s.project().to_string())
    }
}

/// Strip a leading status glyph (e.g. "✳ ", "⠂ ") from a window title.
fn strip_glyph(s: &str) -> String {
    s.trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string()
}
