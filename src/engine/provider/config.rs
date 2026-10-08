//! Per-provider configuration, loaded from the `[engine.provider]` tables of
//! the config file.
//!
//! The `[engine.provider]` table itself carries cross-provider routing — the
//! [`exclusive_prefixes`](ProviderConfig::exclusive_prefixes) list deciding
//! which prefixes own a query outright and the
//! [`target_prefix`](ProviderConfig::target_prefix) delimiter that introduces
//! a `\<id> ` provider target. Built-in providers are keyed by their
//! [`ProviderMeta::id`] under `[engine.provider.builtin.<id>]`. Each section
//! can override the provider's display name, prefixes, enabled flag, and
//! `prefix_only` flag, and carry an arbitrary `extra` config block that is
//! passed through to the provider at init time.

use std::collections::HashMap;

use serde::Deserialize;

use super::ProviderMeta;

/// The default delimiter that introduces a provider target, e.g. `\desktop `.
/// A query `<delimiter><id> <rest>` scopes to the provider with id `<id>`;
/// a bare or partial delimiter lists the providers. This is the `Default`
/// for [`ProviderConfig::target_prefix`] and the fallback for an empty value.
pub(crate) fn default_target_prefix() -> String {
    "\\".to_string()
}

/// Configuration for every provider. Built-in provider overrides live under
/// `builtin`, keyed by provider id (e.g. `[engine.provider.builtin.desktop]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProviderConfig {
    /// Per-provider overrides for built-in providers, keyed by
    /// [`ProviderMeta::id`].
    #[serde(default)]
    pub builtin: HashMap<String, ProviderOverride>,
    /// Prefixes that own a query outright: when the resolved global prefix
    /// for a query is one of these, only providers declaring that prefix are
    /// queried — every other provider, including providers with no prefixes
    /// of their own, is skipped for that keystroke.
    ///
    /// Empty (the default) means no prefix is exclusive and queries are
    /// shared by every enabled provider, as before. A listed prefix no
    /// provider declares is inert: it never resolves as a global prefix, so
    /// the filter never fires.
    #[serde(default)]
    pub exclusive_prefixes: Vec<String>,
    /// The delimiter that introduces a provider-targeting query: `\<id> `
    /// scopes the query to the provider with id `<id>`, and a bare `<id>`
    /// prefix lists the providers. Defaults to `\`. Must not be empty; an
    /// empty configured value falls back to the default.
    #[serde(default = "default_target_prefix")]
    pub target_prefix: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            builtin: HashMap::new(),
            exclusive_prefixes: Vec::new(),
            target_prefix: default_target_prefix(),
        }
    }
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
