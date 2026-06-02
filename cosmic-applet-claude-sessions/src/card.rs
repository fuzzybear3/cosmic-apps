// SPDX-License-Identifier: GPL-3.0-only

//! A standalone COSMIC **layer-shell daemon** that shows a custom "needs your
//! attention" card when a Claude Code session transitions into the waiting
//! state.
//!
//! Why a separate process? The panel applet cannot create its own surface —
//! doing so crashes the applet. A standalone libcosmic app *can* own a
//! layer-shell surface, which gives full control over the card's look,
//! placement and behaviour (beyond what the notification daemon exposes).
//!
//! It runs with no main window, sits idle most of the time, and on a
//! `→waiting` transition spawns a top-right layer surface. Clicking the card
//! focuses the session's terminal window — reusing the exact same native cctk
//! activation as the applet (`focus`). The card auto-dismisses after a short
//! TTL or as soon as the session stops waiting; the persistent "still waiting"
//! indication is the applet's panel icon, not this card.

use crate::focus;
use crate::sessions::{self, Session, SessionState};
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
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::time;

const APP_ID: &str = "io.github.fuzzybear3.CosmicClaudeAttentionCard";

/// How long the card stays up before auto-dismissing. The ongoing "still
/// waiting" state remains visible via the applet's panel icon.
const CARD_TTL: Duration = Duration::from_secs(10);

/// Placement of the card, anchored to the top-left so it drops from under the
/// session-tracker applet (which lives in the panel's left wing). The daemon
/// can't read the applet's exact pixel position (panel applets aren't normal
/// windows), so these are tuned to sit under its icon — nudge `LEFT` to slide
/// the card horizontally, `TOP` for the gap below the panel.
const CARD_MARGIN_LEFT: i32 = 100;
const CARD_MARGIN_TOP: i32 = 8;

/// Stable widget id so the autosize widget can drive the layer surface size.
static AUTOSIZE_ID: LazyLock<cosmic::widget::Id> =
    LazyLock::new(|| cosmic::widget::Id::new("claude-attention-card"));

/// Launch the daemon. No main window: it stays alive with zero surfaces and
/// only creates one on demand.
pub fn run() -> cosmic::iced::Result {
    let settings = cosmic::app::Settings::default()
        .no_main_window(true)
        .exit_on_close(false);
    cosmic::app::run::<CardApp>(settings, ())
}

#[derive(Default)]
pub struct CardApp {
    core: cosmic::Core,

    /// Channel to the background Wayland thread that activates windows.
    focus_tx: Option<focus::FocusSender>,
    /// Shared tty -> live window title (Claude's task topic), from that thread.
    focus_titles: Option<focus::SharedTitles>,

    /// Last seen state per session id, for detecting →waiting transitions.
    prev_states: HashMap<String, SessionState>,

    /// The card currently on screen, if any.
    card: Option<Card>,
}

/// The session a visible card is describing.
struct Card {
    id: Id,
    pid: i32,
    session_id: String,
    label: String,
    project: String,
    shown_at: Instant,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// 1 s heartbeat: re-scan the registry, fire/dismiss the card.
    Refresh,
    /// The card was clicked: focus the session and dismiss.
    Focus,
    /// A surface was closed by the compositor.
    SurfaceClosed(Id),
}

type MsgStream = Pin<Box<dyn futures_util::Stream<Item = Message> + Send + 'static>>;

/// 1 s heartbeat driving the registry re-scan and the auto-dismiss timer.
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
        let (focus_tx, focus_titles) = match focus::spawn() {
            Some((tx, titles)) => (Some(tx), Some(titles)),
            None => (None, None),
        };
        let app = CardApp {
            core,
            focus_tx,
            focus_titles,
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
        match self.card.as_ref().filter(|c| c.id == id) {
            Some(card) => self.card_view(card),
            None => widget::text("").into(),
        }
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::run_with(0u8, refresh_stream)
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::Refresh => return self.refresh(),

            Message::Focus => {
                if let Some(card) = self.card.take() {
                    let tx = self.focus_tx.clone();
                    let pid = card.pid;
                    return Task::batch([
                        destroy_layer_surface(card.id),
                        Task::perform(
                            async move {
                                // Switch tmux to the pane (best-effort), then
                                // ask the Wayland thread to raise the window
                                // mapped to this session's tty.
                                focus::tmux_switch(pid).await;
                                tokio::time::sleep(Duration::from_millis(150)).await;
                                if let (Some(tx), Some(tty)) =
                                    (tx, focus::session_tty(pid).await)
                                {
                                    let _ = tx.send(focus::FocusRequest { tty });
                                }
                            },
                            |_| cosmic::Action::App(Message::Refresh),
                        ),
                    ]);
                }
            }

            Message::SurfaceClosed(id) => {
                if self.card.as_ref().is_some_and(|c| c.id == id) {
                    self.card = None;
                }
            }
        }
        Task::none()
    }
}

impl CardApp {
    fn refresh(&mut self) -> Task<cosmic::Action<Message>> {
        let sessions = sessions::load(true);
        let mut tasks = Vec::new();

        // A session that has *newly* entered the waiting state. Requires a known
        // previous state so pre-existing waiting sessions don't fire a card the
        // moment the daemon starts.
        let newly = sessions.iter().find(|s| {
            matches!(
                self.prev_states.get(&s.session_id).copied(),
                Some(p) if p != SessionState::Attention
            ) && s.state == SessionState::Attention
        });

        if let Some(s) = newly {
            let label = self.session_label(s);
            let project = s.project().to_string();
            if let Some(card) = self.card.as_mut() {
                // A card is already up — swap in the newcomer and reset its timer.
                card.pid = s.pid;
                card.session_id = s.session_id.clone();
                card.label = label;
                card.project = project;
                card.shown_at = Instant::now();
            } else {
                let id = Id::unique();
                self.card = Some(Card {
                    id,
                    pid: s.pid,
                    session_id: s.session_id.clone(),
                    label,
                    project,
                    shown_at: Instant::now(),
                });
                tasks.push(create_card_surface(id));
            }
        }

        self.prev_states = sessions
            .iter()
            .map(|s| (s.session_id.clone(), s.state))
            .collect();

        // Dismiss once the TTL elapses or the carded session stops waiting
        // (resolved or gone). Evaluated after the swap above, so a fresh
        // newcomer is never dismissed on the same tick.
        if let Some(card) = self.card.as_ref() {
            let still_waiting = sessions
                .iter()
                .any(|s| s.session_id == card.session_id && s.state == SessionState::Attention);
            if !still_waiting || card.shown_at.elapsed() >= CARD_TTL {
                let id = card.id;
                self.card = None;
                tasks.push(destroy_layer_surface(id));
            }
        }

        Task::batch(tasks)
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

    fn card_view(&self, card: &Card) -> Element<'_, Message> {
        // Two stacked backgrounds: an amber "warning" layer behind a normal
        // card-coloured panel inset 4px on the left, so the amber shows only as
        // a rounded accent strip down the left. The panel fills the card width
        // (so the amber never leaks into a gap on the right); height has no Fill,
        // so the surface still hugs its content vertically.
        let amber_bg = |theme: &cosmic::Theme| cosmic::iced_widget::container::Style {
            background: Some(Color::from(theme.cosmic().warning.base).into()),
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

        // Symbolic icon tinted with the same amber.
        let icon = widget::icon::from_name("dialog-warning-symbolic")
            .size(28)
            .icon()
            .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(|theme| {
                cosmic::iced_widget::svg::Style {
                    color: Some(Color::from(theme.cosmic().warning.base)),
                }
            })));

        // Bold white headline (default foreground), task, then dim project.
        let mut text_col = widget::column()
            .spacing(3)
            .push(widget::text::title4("Claude needs you"))
            .push(widget::text::body(card.label.clone()));
        if card.project != card.label {
            text_col = text_col.push(
                widget::text::caption(card.project.clone())
                    .class(Color::from_rgb(0.6, 0.6, 0.6)),
            );
        }

        let content = widget::row()
            .push(icon)
            .push(text_col)
            .spacing(14)
            .align_y(Alignment::Center)
            .padding(18);

        let panel = widget::container(content)
            .width(Length::Fill)
            .class(cosmic::theme::Container::custom(panel_bg));

        let card_box = widget::container(panel)
            .padding(cosmic::iced::Padding {
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: 4.0,
            })
            .class(cosmic::theme::Container::custom(amber_bg))
            .clip(true);

        // `mouse_area` makes the whole card clickable without a button's accent
        // tint, so the headline stays white instead of going accent-teal.
        let clickable = widget::mouse_area(card_box).on_press(Message::Focus);

        widget::autosize::autosize(clickable, AUTOSIZE_ID.clone())
            .limits(
                // With the panel set to Length::Fill, the card width is whatever
                // `max_width` is (the panel fills it); shrink this for a narrower
                // card. Height still hugs content.
                Limits::NONE
                    .min_width(280.0)
                    .min_height(1.0)
                    .max_width(360.0)
                    .max_height(260.0),
            )
            .into()
    }
}

/// Create the top-right layer surface for the card. `keyboard_interactivity =
/// None` keeps it from stealing focus (it still receives pointer clicks); the
/// autosize widget drives its size, so `size` is left unset.
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
            .max_width(380.0)
            .max_height(320.0),
        ..Default::default()
    };
    get_layer_surface(settings)
}

/// Strip a leading status glyph (e.g. "✳ ", "⠂ ") from a window title.
fn strip_glyph(s: &str) -> String {
    s.trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string()
}
