//! Per-provider configuration, loaded from the `[engine.provider]` tables of
//! the config file.
//!
//! Built-in providers are keyed by their [`ProviderMeta::id`] under
//! `[engine.provider.builtin.<id>]`. Each section can override the provider's
//! display name, prefixes, enabled flag, and `prefix_only` flag, and carry an
//! arbitrary `extra` config block that is passed through to the provider at
//! init time.

use std::collections::HashMap;

use serde::Deserialize;

use super::ProviderMeta;

/// Configuration for every provider. Built-in provider overrides live under
/// `builtin`, keyed by provider id (e.g. `[engine.provider.builtin.desktop]`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ProviderConfig {
    /// Per-provider overrides for built-in providers, keyed by
    /// [`ProviderMeta::id`].
    #[serde(default)]
    pub builtin: HashMap<String, ProviderOverride>,
}

/// User-facing overrides for a single provider. Every field is optional;
/// omitted fields keep the provider's own defaults.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ProviderOverride {
    /// Human-readable display name for the UI. Defaults to the provider's
    /// id when not set.
    #[serde(default)]
    pub name: Option<String>,
    /// Override whether the provider participates in queries.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Override the trigger prefixes. Empty disables prefix-based activation.
    #[serde(default)]
    pub prefixes: Option<Vec<String>>,
    /// Override whether the provider is only queried when a prefix matches.
    #[serde(default)]
    pub prefix_only: Option<bool>,
    /// Arbitrary per-provider config, passed through as-is to the provider
    /// in [`InitContext::extra`](super::InitContext::extra). Not schema
    /// checked — the provider is responsible for interpreting it.
    #[serde(default)]
    pub extra: Option<serde_json::Value>,
}

impl ProviderOverride {
    /// Merge this config's set fields onto `meta`, returning the combined
    /// result. Fields left unset in the config keep the provider's values;
    /// the provider `id` is never touched.
    pub fn apply(&self, meta: ProviderMeta) -> ProviderMeta {
        ProviderMeta {
            id: meta.id,
            name: self.name.clone().unwrap_or(meta.name),
            enabled: self.enabled.unwrap_or(meta.enabled),
            prefixes: self.prefixes.clone().unwrap_or(meta.prefixes),
            prefix_only: self.prefix_only.unwrap_or(meta.prefix_only),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_preserves_fields_left_unset() {
        let meta = ProviderMeta {
            id: "calc".into(),
            name: "Calculator".into(),
            prefixes: vec!["=".into()],
            enabled: true,
            prefix_only: true,
        };
        let out = ProviderOverride::default().apply(meta.clone());
        assert_eq!(out.id, "calc");
        assert_eq!(out.name, "Calculator");
        assert_eq!(out.prefixes, vec!["="]);
        assert!(out.enabled);
        assert!(out.prefix_only);
    }

    #[test]
    fn apply_overrides_set_fields_and_keeps_id() {
        let meta = ProviderMeta {
            id: "calc".into(),
            name: "Calculator".into(),
            prefixes: vec!["=".into()],
            enabled: true,
            prefix_only: true,
        };
        let ov = ProviderOverride {
            name: Some("Calc".into()),
            enabled: Some(false),
            prefixes: Some(vec!["::".into()]),
            prefix_only: Some(false),
            extra: None,
        };
        let out = ov.apply(meta);
        assert_eq!(out.id, "calc", "the id is never overridden");
        assert_eq!(out.name, "Calc");
        assert!(!out.enabled);
        assert_eq!(out.prefixes, vec!["::"]);
        assert!(!out.prefix_only);
    }
}
