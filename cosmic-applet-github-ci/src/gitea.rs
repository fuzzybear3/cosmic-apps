// SPDX-License-Identifier: GPL-3.0-only

use serde::Deserialize;

/// Overall CI status derived from all monitored repos.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum OverallStatus {
    /// All latest runs succeeded.
    AllGreen,
    /// At least one run is currently queued or in progress.
    Running,
    /// At least one run concluded with failure or cancellation.
    Failing,
    /// Server unreachable at the network level (off-network, down, or timeout).
    Offline,
    /// No repos configured, no token set, or no data fetched yet.
    #[default]
    Unknown,
}

impl OverallStatus {
    /// Symbolic icon name from the hicolor/freedesktop theme.
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::AllGreen => "emblem-ok-symbolic",
            Self::Running => "emblem-synchronizing-symbolic",
            Self::Failing => "emblem-important-symbolic",
            Self::Offline => "network-offline-symbolic",
            Self::Unknown => "dialog-question-symbolic",
        }
    }
}

/// A single action task as returned by the Gitea Actions API
/// (`/repos/{owner}/{repo}/actions/tasks`). Unlike GitHub, Gitea reports a
/// single combined `status` field instead of `status` + `conclusion`.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowRun {
    pub id: u64,
    pub name: Option<String>,
    pub head_branch: Option<String>,
    /// "success" | "failure" | "cancelled" | "skipped" | "running" |
    /// "waiting" | "blocked" | "unknown"
    pub status: Option<String>,
    /// ISO-8601 — when the task was created/queued.
    pub created_at: Option<String>,
    /// ISO-8601 — when the task was last updated (end time for completed runs).
    pub updated_at: Option<String>,
    /// ISO-8601 — when the task actually started executing.
    pub run_started_at: Option<String>,
    pub url: Option<String>,
}

fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&chrono::Utc))
}

impl WorkflowRun {
    /// When this run started executing, falling back to queue time.
    pub fn start_ts(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.run_started_at
            .as_deref()
            .or(self.created_at.as_deref())
            .and_then(parse_ts)
    }

    /// Duration in seconds for a completed run (updated_at − run_started_at).
    /// Returns None if the run is not completed or timestamps are missing.
    pub fn completed_duration_secs(&self) -> Option<f32> {
        if !self.is_completed() {
            return None;
        }
        let start = self.start_ts()?;
        let end = parse_ts(self.updated_at.as_deref()?)?;
        let secs = (end - start).num_seconds();
        if secs > 0 { Some(secs as f32) } else { None }
    }
}

impl WorkflowRun {
    /// Human-readable status string for display.
    pub fn display_status(&self) -> &str {
        self.status.as_deref().unwrap_or("unknown")
    }

    pub fn is_completed(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("success") | Some("failure") | Some("cancelled") | Some("skipped")
        )
    }

    pub fn is_failing(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("failure") | Some("cancelled")
        )
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("running") | Some("waiting") | Some("blocked")
        )
    }
}

/// All fetched runs for a single repository, plus error info if the fetch failed.
#[derive(Debug, Clone)]
pub struct RepoRuns {
    pub owner: String,
    pub repo: String,
    pub runs: Vec<WorkflowRun>,
    pub error: Option<String>,
    /// True when the request never reached the server (DNS/connect/timeout),
    /// as opposed to an API-level error. Drives the Offline status.
    pub offline: bool,
}

/// Top-level Gitea API response for the action tasks endpoint.
#[derive(Deserialize)]
struct RunsResponse {
    workflow_runs: Vec<WorkflowRun>,
}

/// Fetch the 10 most recent action tasks for a single repository.
///
/// Always returns a `RepoRuns`; API errors are captured in `RepoRuns::error`
/// so callers can display them inline without unwrapping.
pub async fn fetch_runs(
    client: &reqwest::Client,
    server_url: &str,
    owner: &str,
    repo: &str,
) -> RepoRuns {
    let server = server_url.trim_end_matches('/');
    let url = format!(
        "{server}/api/v1/repos/{owner}/{repo}/actions/tasks?limit=10"
    );

    match client.get(&url).send().await {
        // `send()` only fails at the transport level (DNS, connect, timeout),
        // so treat any send error as the server being unreachable.
        Err(e) => RepoRuns {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            runs: vec![],
            error: Some(e.to_string()),
            offline: true,
        },
        Ok(resp) => {
            let status = resp.status();
            match resp.json::<RunsResponse>().await {
                Ok(body) => RepoRuns {
                    owner: owner.to_owned(),
                    repo: repo.to_owned(),
                    runs: body.workflow_runs,
                    error: None,
                    offline: false,
                },
                Err(e) => RepoRuns {
                    owner: owner.to_owned(),
                    repo: repo.to_owned(),
                    runs: vec![],
                    error: Some(format!("HTTP {status}: {e}")),
                    offline: false,
                },
            }
        }
    }
}

/// Build a `reqwest::Client` pre-configured with Gitea auth headers.
/// Call once at startup; `reqwest::Client` is cheap to clone (Arc-backed).
pub fn build_client(token: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();

    if !token.is_empty() {
        let auth = format!("token {token}");
        if let Ok(val) = reqwest::header::HeaderValue::from_str(&auth) {
            headers.insert(reqwest::header::AUTHORIZATION, val);
        }
    }

    reqwest::Client::builder()
        .user_agent("cosmic-applet-github-ci/0.1.0")
        .default_headers(headers)
        .build()
        .expect("failed to build reqwest client")
}

/// Compute the aggregate status across all repos.
pub fn compute_overall(all_runs: &[RepoRuns]) -> OverallStatus {
    if all_runs.is_empty() {
        return OverallStatus::Unknown;
    }

    // Every repo failed at the network level → we're off the Gitea network.
    if all_runs.iter().all(|r| r.offline) {
        return OverallStatus::Offline;
    }

    let mut any_run_seen = false;
    let mut any_failing = false;
    let mut any_running = false;

    for repo_runs in all_runs {
        if let Some(latest) = repo_runs.runs.first() {
            any_run_seen = true;
            if latest.is_failing() {
                any_failing = true;
            }
            if latest.is_running() {
                any_running = true;
            }
        }
    }

    if !any_run_seen {
        OverallStatus::Unknown
    } else if any_failing {
        OverallStatus::Failing
    } else if any_running {
        OverallStatus::Running
    } else {
        OverallStatus::AllGreen
    }
}
