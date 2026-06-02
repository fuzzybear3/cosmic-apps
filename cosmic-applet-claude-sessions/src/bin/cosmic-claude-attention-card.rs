// SPDX-License-Identifier: GPL-3.0-only

//! Standalone attention-card daemon. Runs in the background with no visible
//! surface and pops a custom layer-shell card when a Claude Code session starts
//! waiting for you. Launch it once at login (see the README); it complements
//! the panel applet.

fn main() -> cosmic::iced::Result {
    cosmic_applet_claude_sessions::card::run()
}
