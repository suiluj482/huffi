//! Pluggable data sources for the huffi daemon.
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

pub mod calculator;
pub mod collection;
pub mod desktop;
pub mod test_provider;
pub mod util;

use crate::scoring::{Scoreable, Scored};

pub const ICON_SIZE: u16 = 32;

pub use util::{Action, EntryBuilder, entry};

#[derive(Debug, Clone)]
pub struct EntryMeta {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub comment: Option<String>,
    pub icon: Option<String>,
    pub extra: Option<serde_json::Value>,
    pub action: Action,
}

pub type Entry = Scoreable<EntryMeta>;
pub type ScoredEntry = Scored<EntryMeta>;

/// A data source that provides entries for the user to launch.
///
/// # Trait contract
///
/// - [`id()`](Self::id) — a unique name for this provider (used in log
///   messages, not exposed to the user).
/// - [`prefixes()`](Self::prefixes) — one or more string prefixes that
///   trigger this provider (e.g. `["="]` for the calculator). An empty
///   slice means the provider is always active. Each query is preprocessed
///   once: the longest declared prefix that the input starts with becomes
///   the global prefix for that query.
/// - [`init()`](Self::init) — called once at daemon startup. Use this to
///   do expensive work (scan directories, build data structures) so it
///   doesn't happen on every keystroke.
/// - [`query()`](Self::query) — called on every keystroke with the user's
///   current input. Returns all entries this provider can offer. If a
///   prefix matched, the prefix is passed separately and `query` is the
///   text after the prefix. Otherwise `prefix` is `None` and `query` is
///   the full typed text. A provider whose prefixes don't contain the
///   global prefix is treated like an unprefixed provider: it is called
///   with `prefix: None` and the full typed text.
///
/// Entries that should participate in the scoring model must set a
/// [`history_key`](util::EntryBuilder::history_key). Entries without one
/// will still appear in results but won't influence or be influenced by
/// the usage-history ranking.
///
/// # Example
///
/// ```ignore
/// struct MyProvider { entries: Vec<Entry> }
///
/// impl Provider for MyProvider {
///     fn id(&self) -> &str { "my" }
///     fn prefixes(&self) -> &[&str] { &[] }
///     fn init(&mut self) { /* populate self.entries */ }
///     fn query(&self, _prefix: Option<&str>, _query: &str) -> Vec<Entry> {
///         self.entries.clone()
///     }
/// }
/// ```
///
/// See [`CalculatorProvider`] for a real provider with a prefix trigger.
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn prefixes(&self) -> &[&str];
    fn init(&mut self);
    fn query(&self, prefix: Option<&str>, query: &str) -> Vec<Entry>;
}

pub use collection::{ProviderCollection, ProviderInfo};

pub use calculator::CalculatorProvider;
pub use desktop::DesktopEntryProvider;
pub use test_provider::TestProvider;
pub use util::lookup_icon;
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
                .match_fields(vec![crate::scoring::MatchField { text: "Firefox".into(), weight: 1.0 }]),
            entry("org.gnome.Nautilus.desktop", "Files")
                .comment("Access and organize files")
                .icon("org.gnome.Nautilus")
                .history_key("org.gnome.Nautilus.desktop")
                .match_fields(vec![crate::scoring::MatchField { text: "Files".into(), weight: 1.0 }]),
            entry("org.gnome.Calculator.desktop", "Calculator")
                .icon("accessories-calculator")
                .match_fields(vec![crate::scoring::MatchField { text: "Calculator".into(), weight: 1.0 }]),
        ]
    }

    #[test]
    fn test_provider_returns_entries() {
        let provider = TestProvider::new("test", sample_entries());
        let entries = provider.query(None, "");
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
        let provider = TestProvider::new("test", sample_entries());
        let a = provider.query(None, "");
        let b = provider.query(None, "");
        assert_eq!(a.len(), b.len());
    }
}
