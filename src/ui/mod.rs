//! The GTK4 frontend: single-instance control socket, launcher window, and
//! background query tasks. The UI drives the [`Engine`](huffi::engine::Engine)
//! in-process.

pub mod app;
pub mod config;
pub mod control;
pub mod tasks;
pub mod theme;
