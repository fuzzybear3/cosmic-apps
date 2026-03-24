// SPDX-License-Identifier: GPL-3.0-only

use crate::config::Config;
use crate::fl;
use crate::github::{self, OverallStatus, RepoRuns};
use chrono::{DateTime, Utc};
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::{stream, window::Id, Color, Length, Limits, Subscription};
use cosmic::iced_winit::commands::popup::{destroy_popup, get_popup};
use cosmic::prelude::*;
use cosmic::widget;
use futures::channel::mpsc;
use futures_util::SinkExt as _;
use std::pin::Pin;
use tokio::time;

// ─── App model ───────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct AppModel {
    core: cosmic::Core,
    popup: Option<Id>,
    config: Config,

    /// Fetched run data, one entry per configured repo.
    all_runs: Vec<RepoRuns>,

    /// Pre-computed overall status shown as the panel icon.
    overall: OverallStatus,

    /// HTTP client; Option only to satisfy Default (always Some after init).
    client: Option<reqwest::Client>,

    /// Phase counter for the fallback pulse animation (0.0..=1.0).
    animation_phase: f32,

    /// When the current running workflow started (from its created_at field).
    running_start: Option<DateTime<Utc>>,

    /// Duration of the previous completed run of the same workflow, in seconds.
    /// Used to estimate progress of the currently running run.
    estimated_duration_secs: Option<f32>,

    /// Computed progress 0.0..=1.0 when we have enough data; None = pulse fallback.
    panel_progress: Option<f32>,
}

// ─── Messages ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    Tick,
    RunsFetched(Vec<RepoRuns>),
    UpdateConfig(Config),
    AnimationTick,
}

// ─── Stream builders ─────────────────────────────────────────────────────────
//
// Must be module-level `fn` items (not closures) with concrete return types so
// they can be coerced to `fn(&D) -> S` function pointers for `Subscription::run_with`.

fn github_poll_stream(
    interval: &u64,
) -> Pin<Box<dyn futures_util::Stream<Item = Message> + Send + 'static>> {
    let secs = *interval;
    Box::pin(stream::channel(
        1,
        move |mut tx: mpsc::Sender<Message>| async move {
            let period = time::Duration::from_secs(secs);
            let mut timer = time::interval(period);
            timer.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
            // Skip the first immediate tick; init() fires Tick at startup.
            timer.tick().await;
            loop {
                timer.tick().await;
                let _ = tx.send(Message::Tick).await;
            }
        },
    ))
}

fn animation_stream(
    _: &(),
) -> Pin<Box<dyn futures_util::Stream<Item = Message> + Send + 'static>> {
    Box::pin(stream::channel(
        1,
        |mut tx: mpsc::Sender<Message>| async move {
            let mut timer = time::interval(time::Duration::from_millis(100));
            loop {
                timer.tick().await;
                let _ = tx.send(Message::AnimationTick).await;
            }
        },
    ))
}

// ─── cosmic::Application impl ────────────────────────────────────────────────

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "io.github.fuzzybear3.CosmicAppletGithubCi";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let config = cosmic_config::Config::new(Self::APP_ID, Config::VERSION)
            .map(|ctx| match Config::get_entry(&ctx) {
                Ok(cfg) => cfg,
                Err((_errs, cfg)) => cfg,
            })
            .unwrap_or_default();

        let client = Some(github::build_client(&config.token));

        let app = AppModel {
            core,
            config,
            client,
            ..Default::default()
        };

        // Trigger an immediate fetch at startup.
        (app, Task::perform(async {}, |_| cosmic::Action::App(Message::Tick)))
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    // ── Panel button ─────────────────────────────────────────────────────────

    fn view(&self) -> Element<'_, Self::Message> {
        let icon_btn = self.core
            .applet
            .icon_button(self.overall.icon_name())
            .on_press(Message::TogglePopup);

        if self.overall == OverallStatus::Running {
            // Use real progress if we have an estimate, otherwise pulse.
            let progress = self.panel_progress.unwrap_or_else(|| {
                let phase = self.animation_phase * std::f32::consts::TAU;
                (phase.sin() + 1.0) / 2.0
            });

            widget::column()
                .push(icon_btn)
                .push(
                    widget::progress_bar(0.0_f32..=1.0_f32, progress)
                        .girth(Length::Fixed(3.0)),
                )
                .align_x(cosmic::iced::Alignment::Center)
                .into()
        } else {
            icon_btn.into()
        }
    }

    // ── Popup window ─────────────────────────────────────────────────────────

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        let mut content = widget::column().spacing(12).padding(16);

        if self.config.repos.is_empty() {
            content = content.push(
                widget::text(fl!("no-repos-configured")).width(Length::Fill),
            );
        } else {
            for repo_runs in &self.all_runs {
                // ── Repo header ──
                let header = format!("{}/{}", repo_runs.owner, repo_runs.repo);
                content = content.push(
                    widget::text(header).size(14).width(Length::Fill),
                );

                if let Some(ref err) = repo_runs.error {
                    content = content.push(
                        widget::text(format!("  Error: {err}"))
                            .size(11)
                            .class(Color::from_rgb(0.9, 0.3, 0.3)),
                    );
                    continue;
                }

                if repo_runs.runs.is_empty() {
                    content = content.push(widget::text(fl!("no-runs")).size(11));
                    continue;
                }

                // ── Run cards ──
                let mut cards = widget::column().spacing(4);

                for run in repo_runs.runs.iter().take(5) {
                    let status = run.display_status();
                    let dot_color = status_color(status);
                    let branch = run.head_branch.as_deref().unwrap_or("?");
                    let name = run.name.as_deref().unwrap_or("workflow");

                    let card_row = widget::row()
                        .push(
                            widget::text("●")
                                .size(14)
                                .class(dot_color)
                                .width(Length::Fixed(18.0)),
                        )
                        .push(
                            widget::text(name)
                                .size(13)
                                .width(Length::Fill),
                        )
                        .push(
                            widget::text(branch)
                                .size(11)
                                .width(Length::Shrink),
                        )
                        .push(
                            widget::text(status)
                                .size(11)
                                .class(dot_color)
                                .width(Length::Fixed(80.0)),
                        )
                        .spacing(8)
                        .align_y(cosmic::iced::Alignment::Center);

                    let card = widget::container(card_row)
                        .padding([6, 10])
                        .width(Length::Fill);

                    cards = cards.push(card);
                }

                content = content.push(cards);
            }
        }

        self.core.applet.popup_container(content).into()
    }

    // ── Subscriptions ─────────────────────────────────────────────────────────

    fn subscription(&self) -> Subscription<Self::Message> {
        let interval = self.config.refresh_interval();

        // GitHub poll ticker. Identity is keyed on interval so it restarts
        // when the user changes refresh_interval_secs in config.
        let poll = Subscription::run_with(interval, github_poll_stream);

        // Config file watcher.
        let config_watch = self
            .core()
            .watch_config::<Config>(Self::APP_ID)
            .map(|update| Message::UpdateConfig(update.config));

        if self.overall == OverallStatus::Running {
            // Animation ticker — only while a workflow is running.
            let anim = Subscription::run_with((), animation_stream);
            Subscription::batch([poll, config_watch, anim])
        } else {
            Subscription::batch([poll, config_watch])
        }
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
                        .max_width(480.0)
                        .min_width(320.0)
                        .min_height(100.0)
                        .max_height(800.0);
                    get_popup(popup_settings)
                };
            }

            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }

            Message::Tick => {
                if self.config.repos.is_empty() {
                    return Task::none();
                }
                let client = match self.client.clone() {
                    Some(c) => c,
                    None => return Task::none(),
                };
                let repos = self.config.repos.clone();

                return Task::perform(
                    async move {
                        let mut results = Vec::with_capacity(repos.len());
                        for r in &repos {
                            results.push(github::fetch_runs(&client, &r.owner, &r.repo).await);
                        }
                        results
                    },
                    |runs| cosmic::Action::App(Message::RunsFetched(runs)),
                );
            }

            Message::RunsFetched(runs) => {
                self.overall = github::compute_overall(&runs);

                // Find the first running workflow and look for a previous completed
                // run of the same name to use as a duration estimate.
                self.running_start = None;
                self.estimated_duration_secs = None;
                self.panel_progress = None;

                'outer: for repo_runs in &runs {
                    for run in &repo_runs.runs {
                        if !run.is_running() {
                            continue;
                        }
                        // Parse start time of the currently running workflow.
                        if let Some(start) = run.created_at.as_deref()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.with_timezone(&chrono::Utc))
                        {
                            self.running_start = Some(start);
                        }

                        // Look for a previous completed run with the same workflow name.
                        let target_name = run.name.as_deref();
                        for prev in &repo_runs.runs {
                            if prev.id == run.id {
                                continue;
                            }
                            if prev.name.as_deref() == target_name {
                                if let Some(dur) = prev.completed_duration_secs() {
                                    self.estimated_duration_secs = Some(dur);
                                    break;
                                }
                            }
                        }
                        break 'outer;
                    }
                }

                self.all_runs = runs;
            }

            Message::UpdateConfig(new_config) => {
                if new_config.token != self.config.token {
                    self.client = Some(github::build_client(&new_config.token));
                }
                self.config = new_config;
                return self.update(Message::Tick);
            }

            Message::AnimationTick => {
                // Advance the fallback pulse animation phase.
                self.animation_phase = (self.animation_phase + 0.033) % 1.0;

                // If we have timing data, compute real progress.
                if let (Some(start), Some(duration)) =
                    (self.running_start, self.estimated_duration_secs)
                {
                    let elapsed = (chrono::Utc::now() - start).num_seconds() as f32;
                    // Cap at 0.99 so it never shows 100% until the run completes.
                    self.panel_progress = Some((elapsed / duration).clamp(0.0, 0.99));
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

fn status_color(status: &str) -> Color {
    match status {
        "success" => Color::from_rgb(0.20, 0.75, 0.30),
        "failure" | "timed_out" => Color::from_rgb(0.85, 0.20, 0.20),
        "cancelled" => Color::from_rgb(0.60, 0.60, 0.60),
        "in_progress" | "queued" => Color::from_rgb(1.00, 0.65, 0.00),
        _ => Color::from_rgb(0.50, 0.50, 0.50),
    }
}
