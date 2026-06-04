// SPDX-License-Identifier: GPL-3.0-only

//! A standalone COSMIC **layer-shell daemon** that shows custom cards when
//! Claude Code sessions want your attention.
//!
//! Why a separate process? The panel applet cannot create its own surface —
//! doing so crashes the applet. A standalone libcosmic app *can* own a
//! layer-shell surface, which gives full control over the cards' look and
//! placement (beyond what the notification daemon exposes).
//!
//! **Triggers are hook-driven.** Claude Code hooks (wired in `settings.json`)
//! run `card-hook.sh <label>`, which spools the hook's JSON into
//! `~/.claude/card-events/`. This daemon polls that spool (1 s) and shows the
//! matching card:
//!   - `stop`        → "Claude finished" (gated on turn length, see below)
//!   - `error`       → "Claude hit an error" (StopFailure: rate limit, etc.)
//!   - `permission`  → "Claude needs permission" (+ tool name)
//!   - `idle`        → "Claude is waiting for you" (Notification/idle_prompt)
//!   - `turnstart`   → (no card) records turn start, to measure turn length
//!
//! Multiple sessions stack as a **row of cards across the top of the screen**,
//! anchored under the panel applet and growing rightward (one card per session;
//! a newer event for the same session updates its card in place). The hook
//! payload only carries `session_id`/`cwd`, so the daemon loads the registry to
//! resolve `session_id → pid → tty → window` for the live task label and for
//! click-to-focus. Cards auto-dismiss after a TTL; clicking one focuses that
//! session.

use crate::config::{self, Config};
use crate::focus;
use crate::ipc;
use crate::sessions::{self, Session};
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::{stream, window::Id, Alignment, Color, Length, Limits, Subscription};
use cosmic::iced_runtime::platform_specific::wayland::layer_surface::{
    IcedMargin, SctkLayerSurfaceSettings,
};
use cosmic::iced_winit::commands::layer_surface::{
    destroy_layer_surface, get_layer_surface, Anchor, KeyboardInteractivity, Layer,
};
use cosmic::prelude::*;
use cosmic::widget;
use futures::channel::mpsc;
use futures_util::SinkExt as _;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::time;

const APP_ID: &str = "io.github.fuzzybear3.CosmicClaudeAttentionCard";

/// How long a card stays up before auto-dismissing.
const CARD_TTL: Duration = Duration::from_secs(10);

/// A "finished" (Stop) card only fires when the turn ran at least this long, so
/// quick back-and-forth replies don't spam a card while you're actively
/// chatting. Set to `Duration::ZERO` to card on every turn.
const FINISHED_MIN_BUSY: Duration = Duration::from_secs(1);

/// Placement of the row of cards, anchored to the top-left so it drops from
/// under the session-tracker applet (which lives in the panel's left wing).
/// Nudge `LEFT` to slide it horizontally, `TOP` for the gap below the panel.
const CARD_MARGIN_LEFT: i32 = 100;
const CARD_MARGIN_TOP: i32 = 8;

/// Each card's fixed width, the gap between them, and how many stack before the
/// oldest is dropped. `MAX_ROW_WIDTH` bounds the layer surface.
const CARD_WIDTH: f32 = 280.0;
const CARD_GAP: f32 = 8.0;
const MAX_CARDS: usize = 4;
const MAX_ROW_WIDTH: f32 = CARD_WIDTH * MAX_CARDS as f32 + CARD_GAP * MAX_CARDS as f32 + 16.0;

/// Stable widget id so the autosize widget can drive the layer surface size.
static AUTOSIZE_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(|| cosmic::widget::Id::new("claude-attention-cards"));

/// Launch the daemon. No main window: it stays alive with zero surfaces and
/// only creates one on demand.
pub fn run() -> cosmic::iced::Result {
    let settings = cosmic::app::Settings::default()
        .no_main_window(true)
        .exit_on_close(false);
    cosmic::app::run::<CardApp>(settings, ())
}

// ─── What kind of attention a card represents ─────────────────────────────────

#[derive(Debug, Clone)]
enum CardKind {
    /// Stop — Claude finished a (long enough) turn.
    Finished,
    /// StopFailure — the turn ended with an API error.
    Error,
    /// PermissionRequest — a tool needs your approval (carries the tool name).
    Permission(Option<String>),
    /// Notification/idle_prompt — Claude has been waiting for your input.
    Waiting,
}

impl CardKind {
    fn headline(&self) -> String {
        match self {
            CardKind::Finished => "Claude finished".to_string(),
            CardKind::Error => "Claude hit an error".to_string(),
            CardKind::Permission(tool) => match tool.as_deref() {
                Some(t) if !t.is_empty() => format!("Needs permission · {t}"),
                _ => "Claude needs permission".to_string(),
            },
            CardKind::Waiting => "Claude is waiting for you".to_string(),
        }
    }

    fn icon_name(&self) -> &'static str {
        match self {
            CardKind::Finished => "emblem-ok-symbolic",
            CardKind::Error => "dialog-error-symbolic",
            CardKind::Permission(_) | CardKind::Waiting => "dialog-warning-symbolic",
        }
    }

    /// Accent colour (left strip + icon), from the cosmic palette.
    fn accent(&self, theme: &cosmic::Theme) -> Color {
        let c = theme.cosmic();
        match self {
            CardKind::Finished => Color::from(c.success.base),
            CardKind::Error => Color::from(c.destructive.base),
            CardKind::Permission(_) | CardKind::Waiting => Color::from(c.warning.base),
        }
    }
}

// ─── Spool events (written by card-hook.sh) ───────────────────────────────────

#[derive(Deserialize)]
struct SpoolEvent {
    event: String,
    #[serde(default)]
    hook: HookPayload,
}

#[derive(Default, Deserialize)]
struct HookPayload {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    tool_name: Option<String>,
}

// ─── App model ────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct CardApp {
    core: cosmic::Core,

    /// Channel to the background Wayland thread that activates windows.
    focus_tx: Option<focus::FocusSender>,
    /// Shared tty -> live window title (Claude's task topic), from that thread.
    focus_titles: Option<focus::SharedTitles>,

    /// Turn start time per session id (from `turnstart` events), used to gate
    /// the "finished" card on how long the turn ran.
    turn_start: HashMap<String, Instant>,

    /// Shared config (applet's namespace) — `mute_focused` lives here.
    config: Config,
    /// Shared: the tty of the currently focused window, to mute its card.
    focused_tty: Option<focus::FocusedTty>,

    /// Cards on screen, left → right, oldest → newest. One per session.
    cards: Vec<Card>,
    /// The single layer surface holding the row of cards, if any.
    surface: Option<Id>,
}

/// A visible card.
struct Card {
    /// Session id — dedup key (one card per session) and click target.
    session_id: String,
    /// The session's pid, for click-to-focus; None if it wasn't in the registry.
    pid: Option<i32>,
    kind: CardKind,
    label: String,
    project: String,
    shown_at: Instant,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// 1 s heartbeat: drain the spool, fire/expire cards.
    Refresh,
    /// A card was clicked (by session id): focus that session and dismiss it.
    Focus(String),
    /// The surface was closed by the compositor.
    SurfaceClosed(Id),
    /// The shared config changed (cosmic-config file edited).
    UpdateConfig(Config),
}

type MsgStream = Pin<Box<dyn futures_util::Stream<Item = Message> + Send + 'static>>;

/// 1 s heartbeat driving the spool drain and the auto-dismiss timer.
fn refresh_stream(_: &u8) -> MsgStream {
    Box::pin(stream::channel(
        1,
        |mut tx: mpsc::Sender<Message>| async move {
            let mut timer = time::interval(Duration::from_secs(1));
            timer.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                let _ = tx.send(Message::Refresh).await;
            }
        },
    ))
}

impl cosmic::Application for CardApp {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, Task<cosmic::Action<Self::Message>>) {
        // Drop any events spooled before we started, so a restart doesn't replay
        // stale cards.
        clear_spool();

        let (focus_tx, focus_titles, focused_tty) = match focus::spawn() {
            Some((tx, titles, focused)) => {
                // The daemon owns the only Wayland focus thread; serve its
                // state (titles, focused tty) and click-to-focus to the panel
                // applet over IPC.
                ipc::spawn_server(tx.clone(), titles.clone(), focused.clone());
                (Some(tx), Some(titles), Some(focused))
            }
            None => (None, None, None),
        };

        // Read the applet's config (shared cosmic-config namespace).
        let config = cosmic_config::Config::new(config::APPLET_APP_ID, Config::VERSION)
            .map(|ctx| match Config::get_entry(&ctx) {
                Ok(c) => c,
                Err((_errs, c)) => c,
            })
            .unwrap_or_default();

        let app = CardApp {
            core,
            focus_tx,
            focus_titles,
            focused_tty,
            config,
            ..Default::default()
        };
        (
            app,
            Task::perform(async {}, |_| cosmic::Action::App(Message::Refresh)),
        )
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::SurfaceClosed(id))
    }

    /// Never shown — there is no main window.
    fn view(&self) -> Element<'_, Self::Message> {
        widget::text("").into()
    }

    fn view_window(&self, id: Id) -> Element<'_, Self::Message> {
        if self.surface != Some(id) || self.cards.is_empty() {
            return widget::text("").into();
        }
        let mut row = widget::row().spacing(CARD_GAP).align_y(Alignment::Start);
        for card in &self.cards {
            row = row.push(card_element(card));
        }
        widget::autosize::autosize(row, AUTOSIZE_ID.clone())
            .limits(
                Limits::NONE
                    .min_width(1.0)
                    .min_height(1.0)
                    .max_width(MAX_ROW_WIDTH)
                    .max_height(260.0),
            )
            .into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let refresh = Subscription::run_with(0u8, refresh_stream);
        let config_watch = self
            .core()
            .watch_config::<Config>(config::APPLET_APP_ID)
            .map(|update| Message::UpdateConfig(update.config));
        Subscription::batch([refresh, config_watch])
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::Refresh => return self.refresh(),

            Message::Focus(sid) => {
                if let Some(pos) = self.cards.iter().position(|c| c.session_id == sid) {
                    let card = self.cards.remove(pos);
                    let mut tasks = Vec::new();
                    if let Some(pid) = card.pid {
                        let tx = self.focus_tx.clone();
                        tasks.push(Task::perform(
                            async move {
                                // Let the card dismissal settle, then run the
                                // shared focus flow (tmux switch + raise).
                                tokio::time::sleep(Duration::from_millis(150)).await;
                                if let Some(tx) = tx {
                                    let _ = tokio::task::spawn_blocking(move || {
                                        focus::focus_session(pid, &tx)
                                    })
                                    .await;
                                }
                            },
                            |_| cosmic::Action::App(Message::Refresh),
                        ));
                    }
                    // Tear the surface down if that was the last card.
                    tasks.push(self.sync_surface());
                    return Task::batch(tasks);
                }
            }

            Message::SurfaceClosed(id) => {
                if self.surface == Some(id) {
                    self.surface = None;
                    self.cards.clear();
                }
            }

            Message::UpdateConfig(config) => self.config = config,
        }
        Task::none()
    }
}

impl CardApp {
    fn refresh(&mut self) -> Task<cosmic::Action<Message>> {
        let sessions = sessions::load(true);

        for ev in drain_spool() {
            let sid = ev.hook.session_id;
            let cwd = ev.hook.cwd;
            let kind = match ev.event.as_str() {
                "turnstart" => {
                    if !sid.is_empty() {
                        self.turn_start.insert(sid, Instant::now());
                    }
                    continue;
                }
                "stop" => {
                    let long = self
                        .turn_start
                        .remove(&sid)
                        .map(|t| t.elapsed() >= FINISHED_MIN_BUSY)
                        // Unknown turn length (daemon started mid-turn): notify.
                        .unwrap_or(true);
                    if !long {
                        continue;
                    }
                    CardKind::Finished
                }
                "error" => CardKind::Error,
                "permission" => CardKind::Permission(ev.hook.tool_name),
                "idle" => CardKind::Waiting,
                _ => continue,
            };

            let sess = sessions.iter().find(|s| s.session_id == sid);

            // Mute the card if you're already focused on this session's window.
            if self.config.mute_focused {
                let session_tty = sess.and_then(|s| s.tty.clone());
                let focused = self
                    .focused_tty
                    .as_ref()
                    .and_then(|f| f.lock().ok().and_then(|g| g.clone()));
                if session_tty.is_some() && session_tty == focused {
                    continue;
                }
            }

            let label = sess
                .map(|s| self.session_label(s))
                .unwrap_or_else(|| project_of(&cwd));
            let project = sess
                .map(|s| s.project().to_string())
                .unwrap_or_else(|| project_of(&cwd));
            let pid = sess.map(|s| s.pid);
            self.upsert_card(sid, pid, kind, label, project);
        }

        // Forget turn timers for sessions that are gone.
        self.turn_start
            .retain(|sid, _| sessions.iter().any(|s| &s.session_id == sid));

        // Expire old cards, then cap the count (dropping the oldest).
        self.cards.retain(|c| c.shown_at.elapsed() < CARD_TTL);
        if self.cards.len() > MAX_CARDS {
            let excess = self.cards.len() - MAX_CARDS;
            self.cards.drain(0..excess);
        }

        self.sync_surface()
    }

    /// Insert a card for a session, or update the existing one in place (resets
    /// its dismiss timer).
    fn upsert_card(
        &mut self,
        sid: String,
        pid: Option<i32>,
        kind: CardKind,
        label: String,
        project: String,
    ) {
        if let Some(c) = self.cards.iter_mut().find(|c| c.session_id == sid) {
            c.pid = pid;
            c.kind = kind;
            c.label = label;
            c.project = project;
            c.shown_at = Instant::now();
        } else {
            self.cards.push(Card {
                session_id: sid,
                pid,
                kind,
                label,
                project,
                shown_at: Instant::now(),
            });
        }
    }

    /// Create the surface when the first card appears, destroy it when the last
    /// one goes. (When it merely changes, `view_window` re-renders + autosize
    /// resizes — no surface op needed.)
    fn sync_surface(&mut self) -> Task<cosmic::Action<Message>> {
        if self.cards.is_empty() {
            match self.surface.take() {
                Some(id) => destroy_layer_surface(id),
                None => Task::none(),
            }
        } else if self.surface.is_none() {
            let id = Id::unique();
            self.surface = Some(id);
            create_card_surface(id)
        } else {
            Task::none()
        }
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

/// Build one fixed-width card. Accent layer (left strip + icon) coloured per
/// kind, behind a card-coloured panel inset 4px on the left; the panel fills
/// the width so the accent only shows as the left strip.
fn card_element(card: &Card) -> Element<'_, Message> {
    let accent_kind = card.kind.clone();
    let accent_bg = move |theme: &cosmic::Theme| cosmic::iced_widget::container::Style {
        background: Some(accent_kind.accent(theme).into()),
        border: cosmic::iced::Border {
            radius: theme.cosmic().corner_radii.radius_s.into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let panel_bg = |theme: &cosmic::Theme| {
        let c = theme.cosmic();
        cosmic::iced_widget::container::Style {
            icon_color: Some(Color::from(c.background.component.on)),
            text_color: Some(Color::from(c.background.component.on)),
            background: Some(Color::from(c.background.component.base).into()),
            border: cosmic::iced::Border {
                radius: c.corner_radii.radius_s.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    };

    let icon_kind = card.kind.clone();
    let icon = widget::icon::from_name(card.kind.icon_name())
        .size(28)
        .icon()
        .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(move |theme| {
            cosmic::iced_widget::svg::Style {
                color: Some(icon_kind.accent(theme)),
            }
        })));

    let mut text_col = widget::column()
        .spacing(3)
        .push(widget::text::title4(card.kind.headline()))
        .push(widget::text::body(card.label.clone()));
    if !card.project.is_empty() && card.project != card.label {
        text_col = text_col.push(
            widget::text::caption(card.project.clone()).class(Color::from_rgb(0.6, 0.6, 0.6)),
        );
    }

    let content = widget::row()
        .push(icon)
        .push(text_col.width(Length::Fill))
        .spacing(14)
        .align_y(Alignment::Center)
        .padding(18);

    let panel = widget::container(content)
        .width(Length::Fill)
        .class(cosmic::theme::Container::custom(panel_bg));

    let card_box = widget::container(panel)
        .width(Length::Fixed(CARD_WIDTH))
        .padding(cosmic::iced::Padding {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 4.0,
        })
        .class(cosmic::theme::Container::custom(accent_bg))
        .clip(true);

    // `mouse_area` makes the whole card clickable without a button's accent
    // tint, so the headline stays white instead of going accent-coloured.
    widget::mouse_area(card_box)
        .on_press(Message::Focus(card.session_id.clone()))
        .into()
}

/// Create the top-left layer surface holding the row of cards.
/// `keyboard_interactivity = None` keeps it from stealing focus (it still
/// receives pointer clicks); the autosize widget drives its size.
fn create_card_surface(id: Id) -> Task<cosmic::Action<Message>> {
    let settings = SctkLayerSurfaceSettings {
        id,
        layer: Layer::Top,
        keyboard_interactivity: KeyboardInteractivity::None,
        anchor: Anchor::TOP.union(Anchor::LEFT),
        margin: IcedMargin {
            top: CARD_MARGIN_TOP,
            right: 0,
            bottom: 0,
            left: CARD_MARGIN_LEFT,
        },
        namespace: "claude-attention".to_string(),
        size: None,
        exclusive_zone: 0,
        size_limits: Limits::NONE
            .min_width(1.0)
            .min_height(1.0)
            .max_width(MAX_ROW_WIDTH)
            .max_height(260.0),
        ..Default::default()
    };
    get_layer_surface(settings)
}

// ─── Spool ─────────────────────────────────────────────────────────────────

fn spool_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".claude")
        .join("card-events")
}

/// Read, parse and remove every spooled event, oldest first (filenames are
/// nanosecond-timestamped).
fn drain_spool() -> Vec<SpoolEvent> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(spool_dir()) else {
        return out;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    for f in files {
        if let Ok(text) = std::fs::read_to_string(&f) {
            if let Ok(ev) = serde_json::from_str::<SpoolEvent>(&text) {
                out.push(ev);
            }
        }
        let _ = std::fs::remove_file(&f);
    }
    out
}

/// Delete any spooled events (called at startup).
fn clear_spool() {
    if let Ok(entries) = std::fs::read_dir(spool_dir()) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The final path component of a cwd (the project dir).
fn project_of(cwd: &str) -> String {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(cwd)
        .to_string()
}

/// Strip a leading status glyph (e.g. "✳ ", "⠂ ") from a window title.
fn strip_glyph(s: &str) -> String {
    s.trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string()
}
