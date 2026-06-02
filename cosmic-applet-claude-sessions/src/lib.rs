// SPDX-License-Identifier: GPL-3.0-only

//! Shared library for the Claude-sessions tooling. Two binaries build on it:
//! the panel **applet** (`app`) and the standalone layer-shell **attention-card
//! daemon** (`card`). Both read the same session registry (`sessions`) and use
//! the same native window activation (`focus`).

pub mod app;
pub mod card;
pub mod config;
pub mod focus;
pub mod sessions;
