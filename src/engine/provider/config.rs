//! Per-provider configuration, loaded from the `[engine.provider]` tables of
//! the config file.
//!
//! Built-in providers are keyed by their [`id`](super::Provider::id) under
//! `[engine.provider.builtin.<id>]`. Each section can override the provider's
//! display name, prefixes, and enabled flag, and carry an arbitrary `extra`
//! config block that is passed through to the provider at init time.

use std::collections::HashMap;

use serde::Deserialize;

/// Configuration for every provider. Built-in provider overrides live under
/// `builtin`, keyed by provider id (e.g. `[engine.provider.builtin.desktop]`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ProviderConfig {
    /// Per-provider overrides for built-in providers, keyed by
    /// [`Provider::id`](super::Provider::id).
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
