//! The GTK4 frontend: single-instance control socket, launcher window, and
//! background query tasks. The UI drives the [`crate::engine`] in-process.

pub mod app;
pub mod control;
pub mod tasks;
pub mod theme;
