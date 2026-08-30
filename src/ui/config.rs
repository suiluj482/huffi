//! UI settings, parsed from the `[ui]` section of the config file.
//!
//! [`UiConfig`] is resolved by the application-level
//! [`crate::config::Config`]; its `Default` impl is the single source of
//! truth for fallback values, and the container-level `#[serde(default)]`
//! attribute fills every missing field from it.

use serde::Deserialize;

/// Window geometry and entry-list rendering.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub width: i32,
    pub height: i32,
    /// Results shown per page.
    pub page_size: usize,
    /// Entry icon size in pixels.
    pub icon_size: i32,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            width: 600,
            height: 400,
            page_size: 10,
            icon_size: 24,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_section_keeps_other_defaults() {
        let parsed: UiConfig = toml::from_str("width = 800\n").unwrap();
        assert_eq!(parsed.width, 800);
        assert_eq!(parsed.height, UiConfig::default().height);
        assert_eq!(
            parsed,
            UiConfig {
                width: 800,
                height: 400,
                page_size: 10,
                icon_size: 24,
            }
        );
    }
}
