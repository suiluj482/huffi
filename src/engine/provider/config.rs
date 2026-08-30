//! Per-provider configuration, loaded from the `[engine.provider]` tables of
//! the config file.

use serde::Deserialize;

use super::builtin::desktop::DesktopConfig;

/// Configuration for every provider. Each built-in provider's own
/// sub-configuration lives next to it (e.g. `DesktopConfig` in
/// `builtin/desktop.rs`, told apart as `[engine.provider.desktop]`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub desktop: DesktopConfig,
}
