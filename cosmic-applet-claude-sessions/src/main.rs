// SPDX-License-Identifier: GPL-3.0-only

use cosmic_applet_claude_sessions::app::AppModel;

fn main() -> cosmic::iced::Result {
    // Starts the applet's event loop.
    cosmic::applet::run::<AppModel>(())
}
