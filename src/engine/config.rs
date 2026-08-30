//! Engine-level configuration: scoring, providers, and external binaries.
//!
//! Mirrors the `[engine]` tables of the config file. Each subsection lives
//! next to the module that consumes it: `ScoringConfig` in
//! `scoring/config.rs`, `ProviderConfig` in `provider/config.rs`, and
//! `ExternalConfig` here (used by `Action::perform` in
//! `provider/util.rs`).

use serde::Deserialize;

use crate::engine::provider::config::ProviderConfig;
use crate::engine::scoring::config::ScoringConfig;

/// Everything the engine reads from the config file.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct EngineConfig {
    /// Ranking-model tuning (`[engine.scoring]`).
    #[serde(default)]
    pub scoring: ScoringConfig,
    /// Per-provider settings (`[engine.provider.*]`).
    #[serde(default)]
    pub provider: ProviderConfig,
    /// External binaries huffi shells out to (`[engine.external]`).
    #[serde(default)]
    pub external: ExternalConfig,
}

/// External binaries huffi shells out to when performing actions.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct ExternalConfig {
    /// For `Terminal=true` desktop entries: the argv items to prepend to the
    /// entry's command, i.e. the terminal binary plus whatever flags it
    /// expects before a command. Terminals differ here — `kitty` and
    /// `gnome-terminal` use `["kitty", "--"]`, `alacritty`/`xterm` use
    /// `["alacritty", "-e"]`, `foot` takes none. The final argv is
    /// `terminal… <entry command>…`.
    pub terminal: Vec<String>,
    /// Clipboard tool used by the calculator, meta, and other providers.
    pub clipboard: String,
}

impl Default for ExternalConfig {
    fn default() -> Self {
        Self {
            terminal: vec!["kitty".into(), "--".into()],
            clipboard: "wl-copy".into(),
        }
    }
}
