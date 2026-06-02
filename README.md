# cosmic-apps

A collection of custom COSMIC desktop panel applets.

## Applets

- [cosmic-applet-github-ci](cosmic-applet-github-ci/) — GitHub Actions workflow status
- [cosmic-applet-claude-sessions](cosmic-applet-claude-sessions/) — running Claude Code sessions & which need attention

---

# cosmic-applet-github-ci

A COSMIC desktop panel applet that shows GitHub Actions CI/CD workflow status in the top bar.

**Panel button** displays a status icon that changes based on the latest workflow run:
- Green — all passing
- Yellow / spinning — a workflow is currently running, with a progress bar estimating completion based on the previous run's duration
- Red — a run failed
- Gray — no data yet or no token set

**Click** to open a popup listing recent runs per repo, showing workflow name, branch, and status with colored indicators.

## Requirements

- [COSMIC desktop](https://github.com/pop-os/cosmic-epoch)
- Rust (nightly) + Cargo
- `just` (optional, for build recipes) — install with `cargo install just`

## Build

```bash
git clone https://github.com/fuzzybear3/cosmic-apps
cd cosmic-apps/cosmic-applet-github-ci
cargo build --release
```

## Install

### User install (no sudo required)

```bash
install -Dm0755 ../target/release/cosmic-applet-github-ci ~/.local/bin/cosmic-applet-github-ci
install -Dm0644 resources/app.desktop \
    ~/.local/share/applications/io.github.fuzzybear3.CosmicAppletGithubCi.desktop
update-desktop-database ~/.local/share/applications/
```

Or if you have `just`:

```bash
just install-local
```

### System-wide install (requires sudo)

```bash
sudo just install
# installs to /usr/bin and /usr/share/applications
```

After installing, restart the COSMIC panel:

```bash
killall cosmic-panel
```

Then go to **System Settings → Desktop → Panel → Add Applet** and select **GitHub CI Status**.

## Configuration

Config is stored in plain files at:

```
~/.config/cosmic/io.github.fuzzybear3.CosmicAppletGithubCi/v1/
```

Create it with:

```bash
CONFIG_DIR="$HOME/.config/cosmic/io.github.fuzzybear3.CosmicAppletGithubCi/v1"
mkdir -p "$CONFIG_DIR"

# GitHub personal access token (leave empty for public repos)
printf '"ghp_YOURTOKEN"' > "$CONFIG_DIR/token"

# Repositories to monitor
printf '[{"owner":"fuzzybear3","repo":"nihongo_run"}]' > "$CONFIG_DIR/repos"

# Poll interval in seconds (default: 60)
printf '60' > "$CONFIG_DIR/refresh_interval_secs"
```

The applet watches these files with inotify and reloads instantly when they change — no restart needed.

### GitHub token

- **Public repos**: no token required (GitHub allows 60 unauthenticated requests/hour)
- **Private repos**: create a [fine-grained personal access token](https://github.com/settings/tokens) with read access to Actions

### Adding more repos

Edit the `repos` file to add more repositories:

```bash
printf '[{"owner":"fuzzybear3","repo":"nihongo_run"},{"owner":"pop-os","repo":"cosmic-epoch"}]' \
    > "$CONFIG_DIR/repos"
```

## Project structure

```
cosmic-applet-github-ci/
├── Cargo.toml              # dependencies: libcosmic, reqwest, chrono, tokio
├── i18n.toml               # localization config
├── justfile                # build/install recipes
├── i18n/en/
│   └── cosmic_applet_github_ci.ftl   # UI strings
├── resources/
│   ├── app.desktop         # registers the applet with COSMIC panel
│   └── app.metainfo.xml    # appstream metadata
└── src/
    ├── main.rs             # entry point
    ├── i18n.rs             # localization boilerplate
    ├── config.rs           # persistent config (CosmicConfigEntry)
    ├── github.rs           # GitHub API client and types
    └── app.rs              # UI and message logic (cosmic::Application)
```
