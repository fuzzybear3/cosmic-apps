// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{self, cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};

/// A single GitHub repository to monitor, stored as owner + repo name.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepoSpec {
    pub owner: String,
    pub repo: String,
}

/// Persistent configuration for the applet.
///
/// Stored at:
///   ~/.config/cosmic/io.github.fuzzybear3.CosmicAppletGithubCi/v1/
#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    /// GitHub personal access token. Needs `repo` scope for private repos,
    /// or no special scope for public repos.
    pub token: String,

    /// Repositories to monitor.
    pub repos: Vec<RepoSpec>,

    /// How often to poll GitHub in seconds. 0 defaults to 60.
    pub refresh_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            token: String::new(),
            repos: vec![RepoSpec {
                owner: "fuzzybear3".into(),
                repo: "nihongo_run".into(),
            }],
            refresh_interval_secs: 60,
        }
    }
}

impl Config {
    pub fn refresh_interval(&self) -> u64 {
        if self.refresh_interval_secs == 0 {
            60
        } else {
            self.refresh_interval_secs
        }
    }
}
