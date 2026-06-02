// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod config;
mod focus;
mod sessions;

fn main() -> cosmic::iced::Result {
    // Starts the applet's event loop.
    cosmic::applet::run::<app::AppModel>(())
}
