// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{self, cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};

/// Persistent configuration for the applet.
///
/// Stored at:
///   ~/.config/cosmic/io.github.fuzzybear3.CosmicAppletClaudeSessions/v1/
#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    /// Whether to list idle sessions in the popup. When false, only sessions
    /// that are busy or waiting for attention are shown.
    pub show_idle: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { show_idle: true }
    }
}
