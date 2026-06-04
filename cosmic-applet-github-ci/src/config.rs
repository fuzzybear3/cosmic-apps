// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{self, cosmic_config_derive::CosmicConfigEntry, CosmicConfigEntry};

/// A single Gitea repository to monitor, stored as owner + repo name.
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
    /// Base URL of the Gitea server, without a trailing slash.
    pub server_url: String,

    /// Gitea personal access token (scope: read:repository). Required when
    /// the server has REQUIRE_SIGNIN_VIEW enabled.
    pub token: String,

    /// Repositories to monitor.
    pub repos: Vec<RepoSpec>,

    /// How often to poll Gitea in seconds. 0 defaults to 60.
    pub refresh_interval_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server_url: "http://nmc-pc-457.local:3000".into(),
            token: String::new(),
            repos: vec![RepoSpec {
                owner: "nmc".into(),
                repo: "server-systems".into(),
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
