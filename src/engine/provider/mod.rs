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
//! | [`CalculatorProvider`] | `=` prefix | `rink-core` — math expression evaluation |
//! | [`MetaProvider`] | `@` prefix | engine state — uptime, control socket path, pid, version |

pub mod builtin;
pub mod collection;
pub mod config;
pub mod util;

use std::path::Path;
use std::path::PathBuf;

use crate::engine::scoring::{Scoreable, Scored};

pub use collection::PreprocessedQuery;
pub use util::{Action, EntryBuilder, entry};

/// A source for an entry's icon. Providers describe *what* to show without
/// resolving it to a concrete image; the UI is responsible for turning this
/// into a renderable widget (e.g. via the active GTK icon theme).
#[derive(Debug, Clone)]
pub enum Icon {
    /// A freedesktop icon theme name (e.g. `"firefox"`,
    /// `"accessories-calculator"`). Resolved against the active icon theme.
    Name(String),
    /// An explicit path to an icon file (e.g. a PNG or SVG).
    Path(PathBuf),
}

impl From<&str> for Icon {
    fn from(name: &str) -> Self {
        Icon::Name(name.to_owned())
    }
}

impl From<String> for Icon {
    fn from(name: String) -> Self {
        Icon::Name(name)
    }
}

impl From<PathBuf> for Icon {
    fn from(path: PathBuf) -> Self {
        Icon::Path(path)
    }
}

impl From<&std::path::Path> for Icon {
    fn from(path: &std::path::Path) -> Self {
        Icon::Path(path.to_owned())
    }
}

#[derive(Debug, Clone)]
pub struct EntryMeta {
    /// Id unique within the provider; selection looks results up by it.
    pub id: String,
    /// The provider that produced this entry, stamped by
    /// [`ProviderCollection`] when the entry is queried.
    pub provider_id: Option<String>,
    /// Primary label rendered in the entry list.
    pub title: String,
    /// Secondary label shown below the title.
    pub subtitle: Option<String>,
    /// Free-form description; fuzzy-matched but not rendered.
    pub comment: Option<String>,
    /// Icon shown next to the title: a themed name or an explicit file path.
    pub icon: Option<Icon>,
    /// Provider-specific payload, passed through untouched.
    pub extra: Option<serde_json::Value>,
    /// Query the UI applies when the entry is tab-selected, e.g. a
    /// calculator result as `=42`.
    pub set_query: Option<String>,
    /// What happens when the entry is selected.
    pub action: Action,
}

pub type Entry = Scoreable<EntryMeta>;
pub type ScoredEntry = Scored<EntryMeta>;

/// Static metadata describing a provider: its unique id, the string
/// prefixes that trigger it, and whether it is active.
#[derive(Debug, Clone)]
pub struct ProviderMeta {
    /// Id used to identify the provider in logs and select dispatch.
    pub id: String,
    /// Query prefixes that trigger this provider, e.g. `["="]` for the
    /// calculator. Empty means the provider handles every query.
    pub prefixes: Vec<String>,
    /// Whether the provider participates in queries. All built-ins ship
    /// enabled; the flag exists as a hook for future user-config
    /// overrides, and init failures clear it.
    pub enabled: bool,
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
}

impl std::fmt::Display for ProviderResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderResult::Ok => write!(f, "ok"),
            ProviderResult::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            ProviderResult::Other(msg) => write!(f, "{msg}"),
        }
    }
}

/// Context handed to [`Provider::init`] once at startup.
///
/// Wrapped in a struct so future setup inputs (config sections, resolved
/// paths, environment) can be added without breaking existing implementors.
#[derive(Debug, Clone, Copy)]
pub struct InitContext<'a> {
    /// The provider's own data folder (`<data dir>/providers/<provider id>/`,
    /// created unless running in dry-run mode).
    pub data_dir: &'a Path,
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

/// A data source that provides entries for the user to launch.
///
/// # Trait contract
///
/// - [`meta()`](Self::meta) — returns a [`ProviderMeta`] with a unique id
///   (used in log messages, not exposed to the user) and one or more
///   string prefixes that trigger this provider (e.g. `["="]` for the
///   calculator). An empty prefix list means the provider is always
///   active. Each query is preprocessed once: the longest declared prefix
///   that the input starts with becomes the global prefix for that query.
/// - [`init()`](Self::init) — called once at startup with an
///   [`InitContext`] carrying the provider's own data folder
///   (`<data dir>/providers/<provider id>/`, created unless running in
///   dry-run mode). Use this to do expensive work (scan directories, build
///   data structures, open storage) so it doesn't happen on every keystroke.
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
///   `prefix: None` and the full typed text. [`QueryContext::original`]
///   always holds the full query as typed, including the prefix.
/// - [`handle()`](Self::handle) — called when one of this provider's
///   entries is selected by the user, in addition to the entry's action.
///   The default implementation does nothing; overrides can update provider
///   state or trigger behavior on selection.
///
/// Entries that should participate in the scoring model must set a
/// [`history_key`](util::EntryBuilder::history_key). Entries without one
/// will still appear in results but won't influence or be influenced by
/// the usage-history ranking.
///
/// Entries built with [`score`](util::EntryBuilder::score) use
/// [`Rank::Score`](crate::engine::scoring::Rank::Score): the value is used as-is
/// (provider contract: `0.0..=1.0`) and is not normalized against fuzzy
/// matches. Entries built with
/// [`match_fields`](util::EntryBuilder::match_fields) are fuzzy-scored and
/// normalized against the best fuzzy match in the whole batch.
///
/// # Example
///
/// ```ignore
/// struct MyProvider { entries: Vec<Entry> }
///
/// impl Provider for MyProvider {
///     fn meta(&self) -> ProviderMeta {
///         ProviderMeta { id: "my".into(), prefixes: vec![], enabled: true }
///     }
///     fn init(&mut self, _ctx: InitContext) -> ProviderResult {
///         ProviderResult::Ok /* populate self.entries here */
///     }
///     fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
///         self.entries.clone()
///     }
/// }
/// ```
///
/// See [`CalculatorProvider`] for a real provider with a prefix trigger.
pub trait Provider: Send {
    fn meta(&self) -> ProviderMeta;
    fn init(&mut self, ctx: InitContext) -> ProviderResult;
    fn query(&mut self, ctx: QueryContext) -> Vec<Entry>;
    fn handle(&mut self, _ctx: HandleContext) {}
}

pub use collection::ProviderCollection;

pub use builtin::{CalculatorProvider, DesktopEntryProvider, MetaProvider, TestProvider};
pub use util::split_command;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entries() -> Vec<Entry> {
        vec![
            entry("firefox.desktop", "Firefox")
                .comment("Browse the World Wide Web")
                .icon("firefox")
                .history_key("firefox.desktop")
                .match_fields(vec![crate::engine::scoring::MatchField {
                    text: "Firefox".into(),
                    weight: 1.0,
                }]),
            entry("org.gnome.Nautilus.desktop", "Files")
                .comment("Access and organize files")
                .icon("org.gnome.Nautilus")
                .history_key("org.gnome.Nautilus.desktop")
                .match_fields(vec![crate::engine::scoring::MatchField {
                    text: "Files".into(),
                    weight: 1.0,
                }]),
            entry("org.gnome.Calculator.desktop", "Calculator")
                .icon("accessories-calculator")
                .match_fields(vec![crate::engine::scoring::MatchField {
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
        });
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn entry_title_is_correct() {
        let entry = &sample_entries()[0].entry;
        assert_eq!(entry.title, "Firefox");
    }

    #[test]
    fn entry_comment_is_correct() {
        let entry = &sample_entries()[0].entry;
        assert_eq!(entry.comment.as_deref(), Some("Browse the World Wide Web"));
    }

    #[test]
    fn entry_no_comment() {
        let entry = &sample_entries()[2].entry;
        assert!(entry.comment.is_none());
    }

    #[test]
    fn test_provider_clone_entries() {
        let mut provider = TestProvider::new("test", sample_entries());
        let ctx = QueryContext {
            prefix: None,
            query: "",
            original: "",
        };
        let a = provider.query(ctx);
        let b = provider.query(ctx);
        assert_eq!(a.len(), b.len());
    }
}
