# cosmic-applet-claude-sessions

A COSMIC desktop panel applet that tracks your running **Claude Code** sessions — which are
working, which are **waiting for your attention**, and how long the current turn has been running.

**Panel button** shows an icon for the most-urgent state across all sessions:
- Red / warning — at least one session is **waiting** for you (permission prompt or idle input)
- Spinning — at least one session is **busy**
- Available — sessions exist, all idle
- Offline — no live sessions

**Click** (or hotkey) opens a popup listing each live session by project (the basename of its
`cwd`), sorted attention → busy → idle, with a running-time counter while busy. **Clicking a row
focuses that session's terminal.**

## How it works

- **Roster & status** come from Claude Code's own live registry, `~/.claude/sessions/<pid>.json`
  (polled once per second; dead pids are filtered out). This layout is an internal Claude Code
  detail and may change between releases.
- **Attention cards** are the job of a companion process, `cosmic-claude-attention-card` — a
  layer-shell daemon (run as a systemd `--user` service) that drains a hook-fed event spool
  (`~/.claude/card-events/`) and pops a clickable card per session (finished / error / needs
  permission / waiting).
- **The daemon is also the focus authority.** It owns the only Wayland thread that maps sessions
  to windows and activates them; the applet is a thin client that fetches that state (and sends
  click-to-focus requests) over a unix socket in `$XDG_RUNTIME_DIR/cosmic-claude-sessions/`.
  This matters because cosmic-panel respawns its applets on every suspend/resume — the daemon
  survives, so the mapping does too. The daemon additionally persists the map to
  `window-map.json` in the same dir, so even its own restarts keep it.

## Requirements

- [COSMIC desktop](https://github.com/pop-os/cosmic-epoch)
- Rust (nightly) + Cargo

Click-to-focus is fully native (COSMIC toplevel protocols) — no external tools required.

## Build & install (no sudo)

```bash
cargo build --release            # from the workspace root or this dir
install -Dm0755 ../target/release/cosmic-applet-claude-sessions \
    ~/.local/bin/cosmic-applet-claude-sessions
install -Dm0644 resources/app.desktop \
    ~/.local/share/applications/io.github.fuzzybear3.CosmicAppletClaudeSessions.desktop
update-desktop-database ~/.local/share/applications/
```

Then **System Settings → Desktop → Panel (or Dock) → Add Applet → "Claude Sessions"**.
If it doesn't appear, restart the panel: `killall cosmic-panel`.

## Hotkey

The applet toggles its popup on `SIGUSR1`. Bind a COSMIC custom shortcut
(**Settings → Keyboard → Shortcuts → Custom**) to:

```bash
pkill -USR1 -f cosmic-applet-claude-sessions
```

## Click-to-focus & tmux

Clicking a session raises its terminal **natively** — a background Wayland connection speaks
COSMIC's toplevel-management protocol (`zcosmic_toplevel_manager_v1.activate`), the same mechanism
the built-in dock uses. No external helper, no Python.

**Mapping a session to its window** uses the controlling **tty** as the key. A Wayland window
exposes no pid/cwd/tty, so a zsh hook paints `[pts/N]` into the title at the shell prompt (see
Setup). The daemon records `pts/N → window` whenever it sees such a title — keyed on the window's
stable foreign-toplevel `identifier`, so the mapping **survives Claude later overwriting the title**
with its task topic. To focus, it derives the tty from the pid (`/proc/<pid>/fd/0`), looks up the
cached window, and activates it. This handles multiple sessions in one project, and lets Claude
keep auto-naming sessions and showing `✳ task` titles. The applet gets all of this from the
daemon over IPC, so it needs no Wayland focus connection of its own.

The focus action is **tmux-aware**: if the session lives in a tmux pane, it matches the session's
pty to the pane and runs `tmux select-window`/`select-pane`/`switch-client` before raising the
window.

### Setup the applet depends on
- **zsh title hook** (`~/.zshrc`): paint the tty into the title at the prompt, and keep oh-my-zsh
  from overriding it:
  ```zsh
  DISABLE_AUTO_TITLE="true"
  autoload -Uz add-zsh-hook
  _tty_window_title() { printf '\e]2;[%s] %s\a' "${TTY#/dev/}" "${PWD/#$HOME/~}"; }
  add-zsh-hook precmd _tty_window_title
  add-zsh-hook preexec _tty_window_title
  ```
  (Leave `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` unset so Claude still auto-names sessions.)

## Configuration

Stored at `~/.config/cosmic/io.github.fuzzybear3.CosmicAppletClaudeSessions/v1/`
(watched live — no restart needed):

```bash
CONFIG_DIR="$HOME/.config/cosmic/io.github.fuzzybear3.CosmicAppletClaudeSessions/v1"
mkdir -p "$CONFIG_DIR"
printf 'true' > "$CONFIG_DIR/show_idle"      # list idle sessions too (false = busy/waiting only)
printf 'true' > "$CONFIG_DIR/mute_focused"   # daemon: skip the card for the focused session
```

## Project structure

```
cosmic-applet-claude-sessions/
├── Cargo.toml
├── resources/{app.desktop, app.metainfo.xml}
└── src/
    ├── main.rs       # applet entry point
    ├── bin/cosmic-claude-attention-card.rs   # daemon entry point
    ├── lib.rs        # shared library for both binaries
    ├── config.rs     # persistent config (CosmicConfigEntry), shared namespace
    ├── sessions.rs   # read ~/.claude/sessions, derive per-session state
    ├── focus.rs      # native (cctk) window activation + tmux switch (daemon-side)
    ├── ipc.rs        # daemon ⇄ applet: focus state + click-to-focus over a unix socket
    ├── card.rs       # attention-card daemon (layer-shell UI + event spool)
    └── app.rs        # applet UI + message loop (cosmic::Application)
```
