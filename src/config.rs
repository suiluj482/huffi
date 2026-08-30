//! Application-level configuration for huffi.
//!
//! The config file lives at `$XDG_CONFIG_HOME/huffi/config.toml` (default
//! `~/.config/huffi/config.toml`), overridable with `huffi --config PATH`.
//! Every option falls back to a default; command-line flags override the
//! file. A missing file is fine, malformed TOML is a hard error so typos
//! don't silently use defaults.
//!
//! The structs here are the *resolved* configuration. Sections mirror the
//! module structure: `paths` lives here, `ui` lives next to its consumer in
//! [`crate::ui::config`], and the engine subtree (`[engine.scoring]`,
//! `[engine.provider]`, `[engine.external]`) lives in
//! [`huffi::engine::config`]. Each struct's `Default` impl is the single
//! source of truth for fallback values; the container-level
//! `#[serde(default)]` attribute fills every missing field (and every
//! missing subsection) from it, independently per field.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::ui::config::UiConfig;
use huffi::engine::config::EngineConfig;

/// Fully-resolved configuration.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub paths: PathsConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub engine: EngineConfig,
}

/// File system locations: where huffi keeps its data, and where it listens
/// for control messages.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    /// Huffi data folder. History lives at `<data_dir>/history.json` and
    /// each provider gets its own `<data_dir>/providers/<provider id>/`
    /// folder.
    pub data_dir: PathBuf,
    /// Control socket path.
    pub socket: PathBuf,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            socket: default_socket_path(),
        }
    }
}

/// Default data folder: `$XDG_DATA_HOME/huffi`, falling back to
/// `~/.local/share/huffi` (and `/tmp/.local/share/huffi` as a last resort)
/// when `XDG_DATA_HOME` is unset.
pub fn default_data_dir() -> PathBuf {
    data_dir_for(env::var_os("XDG_DATA_HOME"), env::var_os("HOME"))
}

/// Default control socket: `$XDG_RUNTIME_DIR/huffi.sock`, falling back to
/// `/tmp/huffi.sock` when `XDG_RUNTIME_DIR` is unset.
pub fn default_socket_path() -> PathBuf {
    socket_path_for(env::var_os("XDG_RUNTIME_DIR"))
}

/// The user config directory: `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_dir() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

/// The default config file: `$XDG_CONFIG_HOME/huffi/config.toml`.
pub fn default_config_path() -> PathBuf {
    config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("huffi")
        .join("config.toml")
}

impl Config {
    /// Load configuration from `explicit` if given, otherwise from the XDG
    /// default path when it exists. A missing file yields defaults; malformed
    /// TOML is an error.
    pub fn load(explicit: Option<&Path>) -> anyhow::Result<Self> {
        let path = match explicit {
            Some(p) => Some(p.to_path_buf()),
            None => {
                let default = default_config_path();
                default.exists().then_some(default)
            }
        };
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        Self::from_str(&content).with_context(|| format!("invalid config file {}", path.display()))
    }

    fn from_str(content: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(content)?;
        config
            .engine
            .scoring
            .validate()
            .context("invalid [engine.scoring]")?;
        Ok(config)
    }
}

fn data_dir_for(xdg_data: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> PathBuf {
    let data = xdg_data.map(PathBuf::from).unwrap_or_else(|| {
        home.map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".local/share")
    });
    data.join("huffi")
}

fn socket_path_for(xdg_runtime: Option<std::ffi::OsString>) -> PathBuf {
    xdg_runtime
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("huffi.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use huffi::engine::config::ExternalConfig;
    use huffi::engine::provider::config::ProviderConfig;
    use huffi::engine::scoring::config::ScoringConfig;

    #[test]
    fn empty_config_is_defaults() {
        let parsed = Config::from_str("").unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn partial_section_keeps_other_defaults() {
        let parsed = Config::from_str("[paths]\nsocket = \"/tmp/custom.sock\"\n").unwrap();
        assert_eq!(parsed.paths.socket, PathBuf::from("/tmp/custom.sock"));
        assert_eq!(parsed.paths.data_dir, Config::default().paths.data_dir);
        assert_eq!(parsed.ui, Config::default().ui);
        assert_eq!(parsed.engine, Config::default().engine);
    }

    #[test]
    fn partial_engine_section_keeps_sibling_defaults() {
        let parsed = Config::from_str("[engine.scoring]\nboost_weight = 4.0\n").unwrap();
        assert_eq!(parsed.engine.scoring.boost_weight, 4.0);
        assert_eq!(
            parsed.engine.scoring.half_life_days,
            ScoringConfig::default().half_life_days
        );
        assert_eq!(parsed.engine.provider, ProviderConfig::default());
        assert_eq!(parsed.engine.external, ExternalConfig::default());
    }

    #[test]
    fn overrides_apply() {
        let parsed = Config::from_str(
            r#"
[ui]
width = 800
page_size = 25

[engine.scoring]
boost_weight = 4.0
half_life_days = 7

[engine.provider.desktop]
weight_comment = 0.9

[engine.external]
terminal = ["foot"]
"#,
        )
        .unwrap();
        assert_eq!(parsed.ui.width, 800);
        assert_eq!(parsed.ui.height, 400);
        assert_eq!(parsed.ui.page_size, 25);
        assert_eq!(parsed.engine.scoring.boost_weight, 4.0);
        assert_eq!(parsed.engine.scoring.half_life_days, 7.0);
        assert_eq!(parsed.engine.provider.desktop.weight_comment, 0.9);
        assert_eq!(parsed.engine.provider.desktop.weight_name, 1.0);
        assert_eq!(parsed.engine.external.terminal, vec!["foot".to_string()]);
        assert_eq!(parsed.engine.external.clipboard, "wl-copy");
    }

    #[test]
    fn malformed_toml_is_error() {
        assert!(Config::from_str("paths = [").is_err());
        assert!(Config::from_str("[ui]\nwidth = \"not a number\"\n").is_err());
    }

    #[test]
    fn out_of_range_scoring_values_are_rejected() {
        assert!(Config::from_str("[engine.scoring]\nhalf_life_days = 0\n").is_err());
        assert!(Config::from_str("[engine.scoring]\nboost_weight = -1\n").is_err());
        assert!(Config::from_str("[engine.scoring]\nboost_samples = 0\n").is_err());
        assert!(Config::from_str("[engine.scoring]\nconfidence_k = -3.0\n").is_err());
    }

    #[test]
    fn data_dir_defaults_resolve_environment() {
        assert_eq!(
            data_dir_for(Some("/xdg/data".into()), None),
            PathBuf::from("/xdg/data/huffi")
        );
        assert_eq!(
            data_dir_for(None, Some("/home/me".into())),
            PathBuf::from("/home/me/.local/share/huffi")
        );
        assert_eq!(
            data_dir_for(None, None),
            PathBuf::from("/tmp/.local/share/huffi")
        );
    }

    #[test]
    fn socket_defaults_resolve_environment() {
        assert_eq!(
            socket_path_for(Some("/run/user/1000".into())),
            PathBuf::from("/run/user/1000/huffi.sock")
        );
        assert_eq!(socket_path_for(None), PathBuf::from("/tmp/huffi.sock"));
    }
}
