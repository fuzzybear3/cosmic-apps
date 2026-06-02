// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{self, cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};

/// The applet's app id, which is also the cosmic-config namespace where [`Config`]
/// lives. The attention-card daemon reads this same config, so it's shared here.
pub const APPLET_APP_ID: &str = "io.github.fuzzybear3.CosmicAppletClaudeSessions";

/// Persistent configuration, shared by the applet and the attention-card daemon.
///
/// Stored at:
///   ~/.config/cosmic/io.github.fuzzybear3.CosmicAppletClaudeSessions/v1/
#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    /// Whether to list idle sessions in the popup. When false, only sessions
    /// that are busy or waiting for attention are shown.
    pub show_idle: bool,

    /// When true, the attention-card daemon suppresses a session's card while
    /// that session's terminal is the focused window (you're already looking
    /// at it).
    pub mute_focused: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_idle: true,
            mute_focused: true,
        }
    }
}
