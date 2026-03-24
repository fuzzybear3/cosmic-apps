// SPDX-License-Identifier: GPL-3.0-only

use serde::Deserialize;

/// Overall CI status derived from all monitored repos.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum OverallStatus {
    /// All latest runs succeeded.
    AllGreen,
    /// At least one run is currently queued or in progress.
    Running,
    /// At least one run concluded with failure, cancellation, or timeout.
    Failing,
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
            Self::Unknown => "dialog-question-symbolic",
        }
    }
}

/// A single workflow run as returned by the GitHub Actions API.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowRun {
    pub id: u64,
    pub name: Option<String>,
    pub head_branch: Option<String>,
    /// "queued" | "in_progress" | "completed"
    pub status: Option<String>,
    /// "success" | "failure" | "cancelled" | "timed_out" | "skipped" | null
    pub conclusion: Option<String>,
    /// ISO-8601 timestamp of last update, e.g. "2024-01-15T10:30:00Z"
    pub updated_at: Option<String>,
    pub html_url: String,
}

impl WorkflowRun {
    /// Human-readable status string for display.
    pub fn display_status(&self) -> &str {
        match self.status.as_deref() {
            Some("completed") => self.conclusion.as_deref().unwrap_or("unknown"),
            Some(s) => s,
            None => "unknown",
        }
    }

    pub fn is_failing(&self) -> bool {
        matches!(
            self.conclusion.as_deref(),
            Some("failure") | Some("timed_out") | Some("cancelled")
        )
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self.status.as_deref(),
            Some("in_progress") | Some("queued")
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
}

/// Top-level GitHub API response for the workflow runs endpoint.
#[derive(Deserialize)]
struct RunsResponse {
    workflow_runs: Vec<WorkflowRun>,
}

/// Fetch the 10 most recent workflow runs for a single repository.
///
/// Always returns a `RepoRuns`; API errors are captured in `RepoRuns::error`
/// so callers can display them inline without unwrapping.
pub async fn fetch_runs(client: &reqwest::Client, owner: &str, repo: &str) -> RepoRuns {
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/actions/runs?per_page=10"
    );

    match client.get(&url).send().await {
        Err(e) => RepoRuns {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            runs: vec![],
            error: Some(e.to_string()),
        },
        Ok(resp) => {
            let status = resp.status();
            match resp.json::<RunsResponse>().await {
                Ok(body) => RepoRuns {
                    owner: owner.to_owned(),
                    repo: repo.to_owned(),
                    runs: body.workflow_runs,
                    error: None,
                },
                Err(e) => RepoRuns {
                    owner: owner.to_owned(),
                    repo: repo.to_owned(),
                    runs: vec![],
                    error: Some(format!("HTTP {status}: {e}")),
                },
            }
        }
    }
}

/// Build a `reqwest::Client` pre-configured with GitHub auth headers.
/// Call once at startup; `reqwest::Client` is cheap to clone (Arc-backed).
pub fn build_client(token: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();

    if !token.is_empty() {
        let auth = format!("Bearer {token}");
        if let Ok(val) = reqwest::header::HeaderValue::from_str(&auth) {
            headers.insert(reqwest::header::AUTHORIZATION, val);
        }
    }

    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        "X-GitHub-Api-Version",
        reqwest::header::HeaderValue::from_static("2022-11-28"),
    );

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
