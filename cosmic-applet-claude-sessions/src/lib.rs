// SPDX-License-Identifier: GPL-3.0-only

//! Shared library for the Claude-sessions tooling. Two binaries build on it:
//! the panel **applet** (`app`) and the standalone layer-shell **attention-card
//! daemon** (`card`). Both read the same session registry (`sessions`); the
//! daemon owns the native window activation (`focus`) and serves it to the
//! applet over `ipc`.

pub mod app;
pub mod card;
pub mod config;
pub mod focus;
pub mod ipc;
pub mod sessions;
