//! Pluggable data sources for the huffi engine.
//!
//! A provider implements the [`Provider`] trait to supply entries that can
//! be fuzzy-matched and launched. Providers are registered in
//! [`ProviderCollection`] and are queried on every keystroke.
//!
//! # Built-in providers
//!
//! | Provider | Trigger | Source |
//! |---|---|---|
//! | [`DesktopEntryProvider`] | (always active) | `freedesktop-desktop-entry` — `.desktop` files |
//! | [`ActionsProvider`] | (always active) | config file — static entries from `[[…extra.entries]]` |
//! | [`CalculatorProvider`] | `=` prefix | `rink-core` — math expression evaluation |
//! | [`MetaProvider`] | `@` prefix | engine state — uptime, control socket path, pid, version |
//! | [`ProvidersProvider`] | `\` prefix | registered providers — list them, then `\<id> ` to scope to one |
//! | [`NixRunProvider`] | `!` prefix | `nix run nixpkgs#<name>` — nixpkgs packages from `nix search` |
//! | [`UnicodeProvider`] | `:` prefix | `unicode_names2`, `emojis` — characters by name, shortcode, or code point |
//! | [`FilesProvider`] | `/` for live listing; history always | the filesystem — a directory's children, with history-driven rows |
//! | [`AiProvider`] | `?` prefix | OpenAI-compatible chat completion (defaults to local Ollama) — type a prompt, append the trigger suffix to send |
//!
//! # Writing a provider
//!
//! Implement [`Provider`] — the trait below is the whole contract. The types it
//! mentions live in [`mod@entry`] (results and scoring declaration), [`action`]
//! (selection behavior), and [`scoring::history`](crate::engine::scoring::history)
//! (history views). Everything a provider needs is re-exported here, so
//! `use huffi::engine::provider::*;` is enough. See
//! `docs/WRITING_A_PROVIDER.md` for the guided version.

pub mod action;
pub mod builtin;
pub mod collection;
pub mod config;
pub mod entry;

use std::path::Path;
use std::sync::Arc;

use crate::engine::scoring::history::{KnownHistory, QueryHistory};

/// A data source that provides entries for the user to launch.
///
/// # Trait contract
///
/// - [`meta()`](Self::meta) — returns a [`ProviderMeta`] describing this
///   provider. Its `id` is the **stable** identity (e.g. `"desktop"`,
///   `"calculator"`): a short, unique key used in logs, select dispatch,
///   and config lookup, never overridden by config. The remaining fields —
///   display name, trigger prefixes, enabled flag, `prefix_only` — are
///   overwritable by user config; an empty `name` resolves to the `id`, so
///   a provider typically only fills in the `id` plus what differs. An
///   empty prefix list means the provider is always active. Each query is
///   preprocessed once: the longest declared prefix that the input starts
///   with becomes the global prefix for that query.
/// - [`init()`](Self::init) — called once at startup with an
///   [`InitContext`] carrying the provider's own data folder
///   (`<data dir>/providers/<provider id>/`, created unless running in
///   dry-run mode) and any extra per-provider config from the user.
///   Use this to do expensive work (scan directories, build data
///   structures, open storage) so it doesn't happen on every keystroke.
///   Providers never need to locate or create their own folders. Returning
///   [`ProviderResult::Unsupported`] — or any other init error — logs a
///   warning and disables the provider; the engine keeps running.
/// - [`query()`](Self::query) — called on every keystroke with a
///   [`QueryContext`] describing the user's current input. It returns all
///   entries this provider can offer. If a prefix matched,
///   [`QueryContext::prefix`] is `Some` and [`QueryContext::query`] is the
///   text after the prefix. Otherwise `prefix` is `None` and `query` is the
///   full typed text. A provider whose prefixes don't contain the global
///   prefix is treated like an unprefixed provider: it is called with
///   `prefix: None` and the full typed text — unless the global prefix is
///   listed in [`ProviderConfig::exclusive_prefixes`](config::ProviderConfig::exclusive_prefixes),
///   in which case such a provider is not called at all. [`QueryContext::original`]
///   always holds the full query as typed, including the prefix,
///   [`QueryContext::history`] carries the precomputed usage history for
///   that text, and [`QueryContext::known_history_keys`] carries the unscoped
///   list of every used key.
/// - [`handle()`](Self::handle) — called when one of this provider's
///   entries is selected by the user, in addition to the entry's action.
///   The default implementation does nothing; overrides can update provider
///   state or trigger behavior on selection.
///
/// Entries that should participate in the scoring model must set a
/// [`history_key`](entry::EntryBuilder::history_key). Entries without one
/// will still appear in results but won't influence or be influenced by
/// the usage-history ranking.
///
/// Entries built with [`score`](entry::EntryBuilder::score) use
/// [`Rank::Score`]: the value is used as-is
/// (provider contract: `0.0..=1.0`) and is not normalized against fuzzy
/// matches. Entries built with
/// [`match_fields`](entry::EntryBuilder::match_fields) are fuzzy-scored and
/// normalized against the best fuzzy match in the whole batch.
///
/// # Example
///
/// ```ignore
/// struct MyProvider { entries: Arc<[Entry]> }
///
/// impl Provider for MyProvider {
///     fn meta(&self) -> ProviderMeta {
///         ProviderMeta::builder("my").build()
///     }
///     fn init(&mut self, _ctx: InitContext) -> ProviderResult {
///         ProviderResult::Ok /* populate self.entries here */
///     }
///     fn query(&mut self, _ctx: QueryContext) -> Arc<[Entry]> {
///         Arc::clone(&self.entries)
///     }
/// }
/// ```
///
/// The result is a shared slice rather than an owned `Vec`: a provider that
/// answers every keystroke from an index it built once pays one allocation
/// up front instead of one deep clone per query. Callers still see ordinary
/// `Entry` values — [`QueryGroup`] owns the slice, and scoring
/// addresses its rows by position.
///
/// See [`CalculatorProvider`] for a real provider with a prefix trigger.
pub trait Provider: Send {
    fn meta(&self) -> ProviderMeta;
    fn init(&mut self, ctx: InitContext) -> ProviderResult;
    fn query(&mut self, ctx: QueryContext) -> Arc<[Entry]>;
    fn handle(&mut self, _ctx: HandleContext) {}
}

/// Context handed to [`Provider::init`] once at startup.
///
/// Wrapped in a struct so future setup inputs (config sections, resolved
/// paths, environment) can be added without breaking existing implementors.
#[derive(Debug, Clone)]
pub struct InitContext<'a> {
    /// The provider's own data folder (`<data dir>/providers/<provider id>/`,
    /// created unless running in dry-run mode).
    pub data_dir: &'a Path,
    /// Arbitrary per-provider config from `[engine.provider.builtin.<id>.extra]`.
    /// `None` when the user did not set an `extra` section for this provider.
    pub extra: Option<serde_json::Value>,
}

/// Where in the app's lifetime a [`Provider::query`] call is happening.
///
/// Both counters start at 0 and only move forward, so providers can cache
/// expensive data against the dimension that matches its freshness needs:
///
/// - `session` — bump [`begin_session`](crate::engine::Engine::begin_session)
///   when the launcher is revealed for a round of typing. Cache against this
///   to load once per session instead of on every keystroke.
/// - `query` — bump each time providers actually run (a keystroke that
///   missed the engine's query cache). Always the generation of the run
///   currently in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Generation {
    /// Bumped each time the launcher is revealed for a round of typing.
    pub session: u64,
    /// Bumped each time providers actually run.
    pub query: u64,
}

/// The context of a single [`Provider::query`] invocation.
///
/// Wrapped in a struct so future per-query inputs can be added without
/// breaking existing implementors.
#[derive(Debug, Clone, Copy)]
pub struct QueryContext<'a> {
    /// The global prefix for this query. `Some` only when the provider's
    /// prefixes contain it; otherwise `None` even if some other provider
    /// matched a prefix.
    pub prefix: Option<&'a str>,
    /// The text to match against: the query after the prefix when a prefix
    /// matched, otherwise the full typed text.
    pub query: &'a str,
    /// The full original query as typed, including the prefix.
    pub original: &'a str,
    /// The precomputed usage history for [`original`](Self::original),
    /// shared by every provider answering this query and empty when the
    /// query has no recorded history.
    ///
    /// Keys are looked up under the original query — the same lookup history
    /// scoring performs — so a prefix-triggered provider sees the history of
    /// the full typed text, not of its stripped `query`. Each entry carries
    /// the decayed [`effective`](crate::engine::scoring::history::QueryHistoryEntry::effective)
    /// score, its [`score`](crate::engine::scoring::history::QueryHistoryEntry::score)
    /// normalized against the query's strongest key, and its sample count;
    /// [`confidence`](crate::engine::scoring::history::QueryHistory::confidence)
    /// says how much evidence backs the query as a whole. The final ranking
    /// narrows this to the entries actually returned, so treat these numbers
    /// as a query-level view rather than the engine's own blend.
    pub history: &'a QueryHistory,
    /// Every key ever used, unscoped by the typed query.
    ///
    /// Unlike [`history`](Self::history), this is not looked up under
    /// [`original`](Self::original): it is the same global key list for every
    /// query. The store caches it, so iterating it costs no decay math or map
    /// scan. Use it to reconstruct entries the current query does not surface
    /// but which the user has used before; use [`history`](Self::history) when
    /// the decision depends on the typed text (ranking, filtering by current
    /// relevance).
    ///
    /// Both pressing Enter and accepting a Tab suggestion count as a use. It
    /// carries no scores: it answers "has this key ever been used?", not "how
    /// strongly is it associated with this query?".
    pub known_history_keys: &'a KnownHistory,
    /// Where in the app's lifetime this call is happening; see [`Generation`].
    pub generation: Generation,
}

/// The context of a [`Provider::handle`] notification: which of the
/// provider's entries was selected, and the provider-relative
/// [`QueryContext`] the user's input produced.
#[derive(Debug, Clone, Copy)]
pub struct HandleContext<'a> {
    /// The id of the entry that was selected.
    pub entry_id: &'a str,
    /// The query context, relative to this provider (same semantics as the
    /// [`QueryContext`] it received during the relevant
    /// [`query`](Provider::query) call).
    pub query: QueryContext<'a>,
}

/// Static metadata describing a provider: its stable `id`, display name,
/// the string prefixes that trigger it, whether it is active, and whether
/// it only wants to be queried after a prefix. The `id` is the provider's
/// identity and is never overwritten by user config; every other field is
/// overwritable — the provider supplies defaults, the config file can
/// override them.
///
/// Use [`ProviderMeta::builder`] for ergonomic construction:
/// `ProviderMeta::builder("calculator").prefix("=").prefix_only(true).build()`.
/// The `id` is the only required field; `name` defaults to the `id`,
/// `prefixes` is empty, `enabled` is `true`, and `prefix_only` is `false`.
#[derive(Debug, Clone)]
pub struct ProviderMeta {
    /// Stable, unique identifier for this provider (e.g. `"desktop"`,
    /// `"calculator"`). Used in logs, select dispatch, and config lookup.
    /// Never overridden by user config; the engine rejects an empty id at
    /// registration.
    pub id: String,
    /// Human-readable display name for the UI. An empty string resolves to
    /// the provider's [`id`](Self::id) at registration, unless the name is
    /// overridden by config.
    pub name: String,
    /// Query prefixes that trigger this provider, e.g. `["="]` for the
    /// calculator. Empty means the provider handles every query.
    pub prefixes: Vec<String>,
    /// Whether the provider participates in queries. All built-ins ship
    /// enabled; the flag exists as a hook for future user-config
    /// overrides, and init failures clear it.
    pub enabled: bool,
    /// When set, [`ProviderCollection`] only calls [`Provider::query`] if
    /// the query matched one of this provider's prefixes; otherwise the
    /// provider is skipped entirely. Providers return their entries
    /// unconditionally from [`Provider::query`] and leave prefix gating to
    /// the collection.
    pub prefix_only: bool,
}

impl ProviderMeta {
    /// Create a [`ProviderMetaBuilder`] for this type. The `id` is the
    /// only required field; everything else has sensible defaults.
    pub fn builder(id: impl Into<String>) -> ProviderMetaBuilder {
        ProviderMetaBuilder::new(id)
    }

    /// Validate that an id is set and resolve an empty `name` to the id.
    ///
    /// Called by the engine when a provider is registered. A provider that
    /// returns an empty id is a bug — most likely copied from an example
    /// that forgot the field — and is refused loudly rather than silently
    /// breaking config lookup, logs, and select dispatch.
    pub(crate) fn resolved(mut self) -> anyhow::Result<Self> {
        if self.id.is_empty() {
            anyhow::bail!("provider returned an empty id");
        }
        if self.name.is_empty() {
            self.name = self.id.clone();
        }
        Ok(self)
    }
}

/// The outcome of initializing a provider.
#[derive(Debug)]
pub enum ProviderResult {
    /// The provider initialized successfully.
    Ok,
    /// The provider cannot operate on this system (e.g. no nix installed).
    /// Logged as a warning, and the provider is registered but disabled.
    Unsupported(String),
    /// Any other initialization failure. Also logged and the provider
    /// disabled; the engine keeps running.
    Other(String),
    /// Provider configuration is invalid (e.g. malformed `extra` config).
    /// `critical` decides whether the provider is disabled: a critical
    /// config error disables it, a non-critical one logs a warning and the
    /// provider continues with its defaults.
    Config { msg: String, critical: bool },
}

impl std::fmt::Display for ProviderResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderResult::Ok => write!(f, "ok"),
            ProviderResult::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            ProviderResult::Other(msg) => write!(f, "{msg}"),
            ProviderResult::Config { msg, critical } => {
                if *critical {
                    write!(f, "invalid config: {msg}")
                } else {
                    write!(f, "invalid config (using defaults): {msg}")
                }
            }
        }
    }
}

/// Parse a provider's optional `extra` config into a strongly typed struct.
///
/// Returns `Ok(None)` when no `extra` was provided, `Ok(Some(_))` when it
/// parsed, and `Err(ProviderResult::Config { critical: false })` when it
/// was present but invalid, so the provider can continue with its defaults.
pub fn parse_extra_config<T: serde::de::DeserializeOwned>(
    extra: &Option<serde_json::Value>,
) -> Result<Option<T>, ProviderResult> {
    match extra {
        None => Ok(None),
        Some(value) => match serde_json::from_value::<T>(value.clone()) {
            Ok(config) => Ok(Some(config)),
            Err(e) => Err(ProviderResult::Config {
                msg: format!("invalid extra config: {e}"),
                critical: false,
            }),
        },
    }
}

/// Ergonomic builder for [`ProviderMeta`]. Start with
/// [`ProviderMeta::builder`], chain optional setters, and finish with
/// [`.build()`](ProviderMetaBuilder::build).
pub struct ProviderMetaBuilder {
    id: String,
    name: String,
    prefixes: Vec<String>,
    enabled: bool,
    prefix_only: bool,
}

impl ProviderMetaBuilder {
    pub(crate) fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: String::new(),
            prefixes: Vec::new(),
            enabled: true,
            prefix_only: false,
        }
    }

    /// Human-readable display name. Defaults to the provider id.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Add a trigger prefix (e.g. `"="` for the calculator). Call
    /// multiple times for multiple prefixes.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefixes.push(prefix.into());
        self
    }

    /// Set the trigger prefixes at once.
    pub fn prefixes<I, S>(mut self, prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.prefixes.extend(prefixes.into_iter().map(Into::into));
        self
    }

    /// When `true`, the provider is only queried when a prefix matches.
    /// Defaults to `false`.
    pub fn prefix_only(mut self, prefix_only: bool) -> Self {
        self.prefix_only = prefix_only;
        self
    }

    /// Whether the provider participates in queries. Defaults to `true`.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn build(self) -> ProviderMeta {
        ProviderMeta {
            id: self.id,
            name: self.name,
            prefixes: self.prefixes,
            enabled: self.enabled,
            prefix_only: self.prefix_only,
        }
    }
}

// -- Re-exports ----------------------------------------------------------
// Provider authors can import everything they need from this module. The
// remaining items are engine-internal and are re-exported for the engine/UI.

pub use action::{Action, ExecMode, split_command};
pub use builtin::{
    ActionsProvider, AiProvider, CalculatorProvider, CliphistProvider, DesktopEntryProvider,
    FilesProvider, MetaProvider, NixRunProvider, ProvidersProvider, RunnerProvider, TestProvider,
    UnicodeProvider,
};
pub use collection::ProviderCollection;
pub use collection::{PreprocessedQuery, QueryGroup, matches_prefix, matches_query};
pub use entry::{
    Entry, EntryBuilder, Icon, MatchField, QuerySuggestion, Rank, entry, is_detail_key,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    fn sample_entries() -> Vec<Entry> {
        vec![
            entry("firefox.desktop", "Firefox")
                .comment("Browse the World Wide Web")
                .icon("firefox")
                .history_key("firefox.desktop")
                .match_fields(vec![crate::engine::provider::MatchField {
                    text: "Firefox".into(),
                    weight: 1.0,
                }]),
            entry("org.gnome.Nautilus.desktop", "Files")
                .comment("Access and organize files")
                .icon("org.gnome.Nautilus")
                .history_key("org.gnome.Nautilus.desktop")
                .match_fields(vec![crate::engine::provider::MatchField {
                    text: "Files".into(),
                    weight: 1.0,
                }]),
            entry("org.gnome.Calculator.desktop", "Calculator")
                .icon("accessories-calculator")
                .match_fields(vec![crate::engine::provider::MatchField {
                    text: "Calculator".into(),
                    weight: 1.0,
                }]),
        ]
    }

    #[test]
    fn test_provider_returns_entries() {
        let mut provider = TestProvider::new("test", sample_entries());
        let entries = provider.query(QueryContext {
            prefix: None,
            query: "",
            original: "",
            history: &QueryHistory::EMPTY,
            known_history_keys: &KnownHistory::EMPTY,
            generation: Generation::default(),
        });
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_provider_clone_entries() {
        let mut provider = TestProvider::new("test", sample_entries());
        let ctx = QueryContext {
            prefix: None,
            query: "",
            original: "",
            history: &QueryHistory::EMPTY,
            known_history_keys: &KnownHistory::EMPTY,
            generation: Generation::default(),
        };
        let a = provider.query(ctx);
        let b = provider.query(ctx);
        assert_eq!(a.len(), b.len());
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(default)]
    struct DummyConfig {
        weight: f32,
    }

    impl Default for DummyConfig {
        fn default() -> Self {
            Self { weight: 1.0 }
        }
    }

    #[test]
    fn parse_extra_config_none_when_absent() {
        let extra: Option<serde_json::Value> = None;
        assert_eq!(parse_extra_config::<DummyConfig>(&extra).unwrap(), None);
    }

    #[test]
    fn parse_extra_config_some_when_valid() {
        let extra = Some(serde_json::json!({ "weight": 2.5 }));
        assert_eq!(
            parse_extra_config::<DummyConfig>(&extra).unwrap(),
            Some(DummyConfig { weight: 2.5 })
        );
    }

    #[test]
    fn parse_extra_config_non_critical_error_when_invalid() {
        let extra = Some(serde_json::json!({ "weight": "not a number" }));
        match parse_extra_config::<DummyConfig>(&extra) {
            Err(ProviderResult::Config {
                critical: false, ..
            }) => {}
            other => panic!("expected non-critical Config error, got {other:?}"),
        }
    }
}
