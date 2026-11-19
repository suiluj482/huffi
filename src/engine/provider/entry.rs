//! The data a provider declares about its results: [`Entry`] and the
//! [`EntryBuilder`] used to construct one, plus the scoring declaration
//! ([`Rank`], [`MatchField`]) and display helpers (`Icon`, `QuerySuggestion`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::action::{Action, ExecMode};

/// One launchable result: everything a provider declares about an entry.
///
/// The provider identity is not stored here: every entry in a
/// [`QueryGroup`](super::collection::QueryGroup) comes from the same provider, so the
/// group carries it once. `rank` and the history key are the provider's
/// declared inputs to scoring; the engine reads them on every keystroke rather
/// than mutating the entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Id unique within the provider; selection looks results up by it.
    pub id: String,
    /// Primary label rendered in the entry list.
    pub title: String,
    /// Secondary label shown below the title.
    pub subtitle: Option<String>,
    /// Long-form prose: a description, a caveat, a definition. Only shown by a
    /// theme that declares a `comment` widget, and not searched unless the
    /// provider adds it to its match fields.
    pub comment: Option<String>,
    /// Icon shown next to the title: a themed name or an explicit file path.
    pub icon: Option<Icon>,
    /// Named display fields, bound by the row renderer onto `detail-<key>`
    /// widgets in the active theme's row template. Keys must match
    /// `[a-z0-9-]+` (see [`is_detail_key`]), since they become part of a GTK
    /// object id; a detail with no matching widget in the template is simply
    /// not shown, and a widget with no matching detail stays hidden. Providers
    /// choose the key vocabulary, so this is the channel for structured results
    /// a title and subtitle can't carry — see [`CalculatorProvider`](super::CalculatorProvider), which uses
    /// it for quantities, dimensionality, and measured properties.
    pub details: BTreeMap<String, String>,
    /// Layout variant for this entry, e.g. `"info"`. Selects a row template by
    /// walking the theme's positions for this variant: `providers/<id>/<variant>`,
    /// then `providers/<id>`, then `variants/<variant>`, then the theme-wide
    /// `entry.ui`. See [`EntryBuilder::variant`].
    ///
    /// Rows also get a provider-independent `variant-<variant>` CSS class as well
    /// as `provider-<id>-<variant>`, so a theme can style one layout across every
    /// provider that reports it.
    pub variant: Option<String>,
    /// Query the UI applies when the entry is tab-selected, e.g. a
    /// calculator result as `42`.
    pub set_query: Option<QuerySuggestion>,
    /// What happens when the entry is selected.
    pub action: Action,
    /// How the entry earns its score: a provider-declared value, or the
    /// fields the matcher fuzzy-matches against.
    pub rank: Rank,
    /// Key this entry's usage history is recorded under. Entries without one
    /// appear in results but neither contribute to nor are influenced by
    /// history ranking.
    pub history_key: Option<String>,
}
/// A source for an entry's icon. Providers describe *what* to show without
/// resolving it to a concrete image; the UI is responsible for turning this
/// into a renderable widget (e.g. via the active GTK icon theme).
#[derive(Debug, Clone, PartialEq, Eq)]
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
/// The query the UI applies when an entry is tab-selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySuggestion {
    /// The text to put in the input. With [`keep_prefix`](Self::keep_prefix)
    /// this is the text *under* the prefix rather than a whole query.
    pub query: String,
    /// Whether the active prefix stays in front of
    /// [`query`](Self::query) when it is applied.
    ///
    /// This is what a provider means when it refines the query that produced
    /// it: it offers the text and leaves the prefix to the engine, the only
    /// party that knows which prefix is active. A provider whose prefixes can
    /// be overridden in the config file must not build the prefix into the
    /// suggestion itself — the override would leave it pointing at a prefix
    /// that no longer triggers anything.
    pub keep_prefix: bool,
}

impl QuerySuggestion {
    /// A whole query, replacing whatever is in the input.
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            keep_prefix: false,
        }
    }

    /// The text under the active prefix, which stays in front of it.
    pub fn keeping_prefix(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            keep_prefix: true,
        }
    }

    /// The query to put in the input, given the prefix currently active.
    ///
    /// A [`KeepPrefix`](Self::keep_prefix) suggestion with no prefix in play is
    /// just its text: that happens only for a provider that was not triggered by
    /// a prefix, so it has no prefix to keep.
    pub fn resolve(&self, prefix: Option<&str>) -> String {
        match (self.keep_prefix, prefix) {
            (true, Some(prefix)) => format!("{prefix}{}", self.query),
            _ => self.query.clone(),
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum Rank {
    Score(f32),
    MatchFields(Vec<MatchField>),
}
/// One weighted text field an entry is fuzzy-matched against.
///
/// Part of the provider-facing entry declaration: providers list the texts
/// that should be searchable, and the scorer reads them from `Rank`.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchField {
    pub text: String,
    pub weight: f32,
}
/// Whether `key` is usable as a named-detail key: a non-empty run of
/// `[a-z0-9-]`, since the row renderer turns it into a `detail-<key>` GTK
/// object id and looks the widget up under exactly that name.
///
/// Enforced by `debug_assert!` in [`EntryBuilder::detail`] rather than a
/// panic. `Provider::query` returns an `Arc<[Entry]>`, so a provider has no
/// way to report a bad key; a hard assert would mean one bad key in a
/// third-party provider crashes the launcher on every keystroke. This way
/// development builds and the test suite fail loudly at the point of the
/// mistake, while a release build degrades safely — the renderer skips keys
/// failing this check, so the field is simply not shown.
pub fn is_detail_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
pub fn entry(id: impl Into<String>, title: impl Into<String>) -> EntryBuilder {
    EntryBuilder {
        id: id.into(),
        title: title.into(),
        subtitle: None,
        comment: None,
        icon: None,
        details: BTreeMap::new(),
        variant: None,
        action: None,
        cwd: None,
        rank: None,
        history_key: None,
        set_query: None,
    }
}

pub struct EntryBuilder {
    id: String,
    title: String,
    subtitle: Option<String>,
    comment: Option<String>,
    icon: Option<Icon>,
    details: BTreeMap<String, String>,
    variant: Option<String>,
    action: Option<Action>,
    cwd: Option<PathBuf>,
    rank: Option<Rank>,
    history_key: Option<String>,
    set_query: Option<QuerySuggestion>,
}

impl EntryBuilder {
    /// Set the secondary line under the title.
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    /// Set the long-form description of this entry: prose, a caveat, a
    /// definition.
    ///
    /// The default `entry.ui` does not declare a `comment` widget, so this is
    /// not shown unless a theme asks for it. Emit what you know; whether it
    /// reaches the screen is the theme's call.
    pub fn comment(mut self, s: impl Into<String>) -> Self {
        self.comment = Some(s.into());
        self
    }

    /// Set the icon to a themed icon name (default `impl Into<Icon>` maps
    /// strings to [`Icon::Name`]).
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Set the icon to a themed icon name (freedesktop icon theme).
    pub fn icon_name(mut self, name: impl Into<String>) -> Self {
        self.icon = Some(Icon::Name(name.into()));
        self
    }

    /// Set the icon to an explicit file path (PNG or SVG).
    pub fn icon_path(mut self, path: impl AsRef<Path>) -> Self {
        self.icon = Some(Icon::Path(path.as_ref().to_owned()));
        self
    }

    /// Add a named display field, bound by the row renderer onto a
    /// `detail-<key>` widget in the active theme's row template. Keys must
    /// match `[a-z0-9-]+` (see [`is_detail_key`]). A later detail with the same
    /// key replaces this one.
    ///
    /// See [`Entry::details`].
    pub fn detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let key = key.into();
        debug_assert!(
            is_detail_key(&key),
            "detail key {key:?} must match [a-z0-9-]+ to become a detail-<key> widget id"
        );
        self.details.insert(key, value.into());
        self
    }

    /// Add several named display fields at once. Later keys win.
    pub fn details(mut self, fields: impl IntoIterator<Item = (String, String)>) -> Self {
        self.details.extend(fields);
        for key in self.details.keys() {
            debug_assert!(
                is_detail_key(key),
                "detail key {key:?} must match [a-z0-9-]+ to become a detail-<key> widget id"
            );
        }
        self
    }

    /// Select a row layout for this entry, e.g. `"info"`. A variant names a
    /// *layout* rather than a result kind, so prefer a name another provider
    /// could plausibly want too — that is what lets a theme ship one
    /// `variants/<name>/entry.ui` for every provider reporting it.
    ///
    /// See [`Entry::variant`].
    pub fn variant(mut self, name: impl Into<String>) -> Self {
        self.variant = Some(name.into());
        self
    }

    pub fn exec(mut self, args: Vec<String>) -> Self {
        self.action = Some(Action::Exec {
            args,
            mode: ExecMode::Direct,
            cwd: None,
        });
        self
    }

    pub fn terminal_exec(mut self, args: Vec<String>) -> Self {
        self.action = Some(Action::Exec {
            args,
            mode: ExecMode::Terminal,
            cwd: None,
        });
        self
    }

    /// Like [`terminal_exec`](Self::terminal_exec), but the terminal stays
    /// open after the command exits, so its output remains readable. Uses
    /// the `terminal_hold` wrapper from `[engine.external]` instead of
    /// `terminal`.
    pub fn terminal_hold(mut self, args: Vec<String>) -> Self {
        self.action = Some(Action::Exec {
            args,
            mode: ExecMode::TerminalHold,
            cwd: None,
        });
        self
    }

    /// Run this entry's exec action in `path` rather than the configured
    /// `working_dir`. A leading `~/` expands against `$HOME`. Ignored (in
    /// release builds) if the entry has no exec action — there is nowhere
    /// to run it.
    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    /// Set the action to copy `value` to the clipboard on selection. The
    /// clipboard binary comes from config at perform time.
    pub fn clipboard(mut self, value: impl Into<String>) -> Self {
        self.action = Some(Action::Clipboard {
            value: value.into(),
        });
        self
    }

    /// Set the action to run `args` on selection and pipe its stdout into the
    /// clipboard tool's stdin. The clipboard binary comes from config at
    /// perform time. Use this for exact data that is too large for argv or is
    /// not text, such as a clipboard-history entry or a decoded image.
    pub fn clipboard_command(mut self, args: Vec<String>) -> Self {
        self.action = Some(Action::ClipboardCommand { args });
        self
    }

    pub fn history_key(mut self, key: impl Into<String>) -> Self {
        self.history_key = Some(key.into());
        self
    }

    /// Track history under this entry's own id.
    ///
    /// Equivalent to `.history_key(id)` for providers whose ids already carry
    /// the provider namespace (e.g. `unicode-2603` or `meta-uptime`), so the
    /// key stays unique in the shared history store without repeating it at
    /// the call site. Call it after the id is fixed; the builder captures the
    /// id it was created with.
    pub fn track_history(mut self) -> Self {
        self.history_key = Some(self.id.clone());
        self
    }

    /// Set the query suggestion the UI applies when this entry is
    /// tab-selected. The text replaces the whole query.
    pub fn set_query(mut self, query: impl Into<String>) -> Self {
        self.set_query = Some(QuerySuggestion::new(query));
        self
    }

    /// Set the query suggestion the UI applies when this entry is
    /// tab-selected, as the text *under* the active prefix rather than as a
    /// whole query.
    ///
    /// This is what a provider refining the query that produced it wants: it
    /// offers the text and leaves the prefix to the engine, so a `prefixes`
    /// override in the config file cannot leave the suggestion pointing at a
    /// prefix that no longer triggers anything.
    pub fn set_query_keeping_prefix(mut self, query: impl Into<String>) -> Self {
        self.set_query = Some(QuerySuggestion::keeping_prefix(query));
        self
    }

    /// Set the [`Action::SetQuery`] applied when this entry is
    /// *selected* (Enter): the query is replaced with `query` and the
    /// launcher stays open. Nothing is spawned, but the selection is
    /// recorded like any other.
    ///
    /// Independent of [`set_query`](Self::set_query): that one is what Tab
    /// applies, this one is what Enter applies, and an entry may carry both
    /// with different values (or just one).
    pub fn action_set_query(mut self, query: impl Into<String>) -> Self {
        self.action = Some(Action::SetQuery {
            suggestion: QuerySuggestion::new(query),
        });
        self
    }

    /// Like [`action_set_query`](Self::action_set_query), but only the text
    /// under the active prefix is replaced; the prefix stays in front of it.
    pub fn action_set_query_keeping_prefix(mut self, query: impl Into<String>) -> Self {
        self.action = Some(Action::SetQuery {
            suggestion: QuerySuggestion::keeping_prefix(query),
        });
        self
    }

    pub fn score(mut self, score: f32) -> Entry {
        self.rank = Some(Rank::Score(score));
        self.build()
    }

    pub fn match_fields(mut self, fields: Vec<MatchField>) -> Entry {
        self.rank = Some(Rank::MatchFields(fields));
        self.build()
    }

    /// Convenience for a single fuzzy-match field at weight 1.0.
    pub fn match_field(mut self, text: impl Into<String>) -> Entry {
        self.rank = Some(Rank::MatchFields(vec![MatchField {
            text: text.into(),
            weight: 1.0,
        }]));
        self.build()
    }

    pub fn build(self) -> Entry {
        let mut action = self.action.unwrap_or(Action::NoOp);
        if let Some(cwd) = self.cwd {
            match &mut action {
                Action::Exec { cwd: slot, .. } => *slot = Some(cwd),
                _ => debug_assert!(
                    false,
                    "cwd() on an entry without an exec action has no effect"
                ),
            }
        }
        Entry {
            id: self.id,
            title: self.title,
            subtitle: self.subtitle,
            comment: self.comment,
            icon: self.icon,
            details: self.details,
            variant: self.variant,
            set_query: self.set_query,
            action,
            rank: self.rank.unwrap_or(Rank::Score(1.0)),
            history_key: self.history_key,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_title_is_correct() {
        let entry = entry("firefox.desktop", "Firefox").build();
        assert_eq!(entry.title, "Firefox");
    }

    #[test]
    fn entry_comment_is_correct() {
        let entry = entry("firefox.desktop", "Firefox")
            .comment("Browse the World Wide Web")
            .build();
        assert_eq!(entry.comment.as_deref(), Some("Browse the World Wide Web"));
    }

    #[test]
    fn entry_no_comment() {
        let entry = entry("org.gnome.Calculator.desktop", "Calculator").build();
        assert!(entry.comment.is_none());
    }

    #[test]
    fn detail_keys_must_be_lowercase_word_chars() {
        for good in ["quantity", "exact", "doc", "abs-time", "x1", "9lives", "-"] {
            assert!(is_detail_key(good), "{good:?} should be a valid detail key");
        }
        for bad in [
            "", "Quantity", "abs_time", "abs time", "abs.time", "abs/time", "ümlaut",
        ] {
            assert!(!is_detail_key(bad), "{bad:?} should not be a detail key");
        }
    }

    #[test]
    fn details_reach_the_entry_under_their_keys() {
        let meta = entry("id", "title")
            .detail("quantity", "length")
            .details([
                ("exact".to_owned(), "1/3".to_owned()),
                ("dimensions".to_owned(), "L T^-1".to_owned()),
            ])
            .build();
        let details = &meta.details;
        assert_eq!(details["quantity"], "length");
        assert_eq!(details["exact"], "1/3");
        assert_eq!(details["dimensions"], "L T^-1");
        assert!(details.keys().all(|k| is_detail_key(k)));
    }

    #[test]
    fn track_history_uses_the_entry_id_as_the_key() {
        let tracked = entry("meta-uptime", "Uptime").track_history().build();
        assert_eq!(tracked.history_key.as_deref(), Some("meta-uptime"));

        // It is a normal builder step: a later explicit key still wins.
        let overridden = entry("id", "title")
            .track_history()
            .history_key("explicit")
            .build();
        assert_eq!(overridden.history_key.as_deref(), Some("explicit"));

        let untracked = entry("id", "title").build();
        assert!(untracked.history_key.is_none());
    }
    #[test]
    fn a_whole_query_suggestion_ignores_the_prefix() {
        let suggestion = QuerySuggestion::new("firefox");
        assert!(!suggestion.keep_prefix);
        assert_eq!(suggestion.resolve(Some(":")), "firefox");
        assert_eq!(suggestion.resolve(None), "firefox");
    }

    /// The prefix is whatever the user configured and typed, so the provider
    /// offers only the text and this is where the two are joined.
    #[test]
    fn a_keeping_prefix_suggestion_takes_the_active_prefix() {
        let suggestion = QuerySuggestion::keeping_prefix("u+2603");
        assert!(suggestion.keep_prefix);
        assert_eq!(suggestion.resolve(Some(":")), ":u+2603");
        assert_eq!(suggestion.resolve(Some("~")), "~u+2603");
        assert_eq!(suggestion.resolve(Some("::")), "::u+2603");
    }

    /// Only a provider that was not prefix-triggered gets here, and it has no
    /// prefix to keep.
    #[test]
    fn a_keeping_prefix_suggestion_without_a_prefix_is_the_text() {
        let suggestion = QuerySuggestion::keeping_prefix("u+2603");
        assert_eq!(suggestion.resolve(None), "u+2603");
    }
    #[test]
    fn set_query_carries_the_whole_query() {
        let entry = entry("id", "title").set_query("=42").score(1.0);
        let suggestion = entry.set_query.expect("a suggestion");
        assert!(!suggestion.keep_prefix);
        assert_eq!(suggestion.query, "=42");
    }

    /// A provider refining the query that produced it offers the text alone, so
    /// a `prefixes` override cannot strand the suggestion on a dead prefix.
    #[test]
    fn set_query_keeping_prefix_carries_the_text_only() {
        let entry = entry("id", "title")
            .set_query_keeping_prefix("42")
            .score(1.0);
        let suggestion = entry.set_query.expect("a suggestion");
        assert!(suggestion.keep_prefix);
        assert_eq!(suggestion.query, "42");
        assert_eq!(suggestion.resolve(Some("#")), "#42");
    }

    /// The flag belongs to the suggestion, so it cannot be set without one.
    #[test]
    fn an_entry_without_a_suggestion_has_nothing_to_resolve() {
        let entry = entry("id", "title").score(1.0);
        assert!(entry.set_query.is_none());
    }
    /// Tab applies `set_query`, Enter applies `action_set_query`, and the
    /// two carry independent values on the same entry.
    #[test]
    fn the_enter_suggestion_is_independent_of_the_tab_suggestion() {
        let e = entry("id", "title")
            .set_query("tab value")
            .action_set_query("enter value")
            .score(1.0);

        let tab = e.set_query.expect("tab suggestion");
        assert_eq!(tab.query, "tab value");
        assert!(!tab.keep_prefix);
        assert_eq!(
            e.action.query_suggestion().map(|s| s.query.as_str()),
            Some("enter value"),
            "Enter applies its own value, not the tab one"
        );
    }

    #[test]
    fn action_set_query_keeps_the_prefix_when_asked() {
        let e = entry("id", "title")
            .action_set_query_keeping_prefix("u+2603")
            .score(1.0);
        let suggestion = e.action.query_suggestion().expect("a query suggestion");
        assert!(suggestion.keep_prefix);
        assert_eq!(suggestion.resolve(Some(":")), ":u+2603");
        assert!(e.set_query.is_none(), "the tab suggestion stays unset");
    }

    /// The accessor is how the UI pulls the suggestion out of the row
    /// without matching on action variants itself.
    #[test]
    fn only_the_set_query_action_exposes_a_suggestion() {
        let ls = || vec!["ls".to_string()];
        let with = entry("id", "t").action_set_query("q").score(1.0);
        assert!(with.action.query_suggestion().is_some());

        for no in [
            entry("a", "t").exec(ls()).score(1.0),
            entry("b", "t").clipboard("x").score(1.0),
            entry("c", "t").set_query("tab-only").score(1.0),
            entry("d", "t").score(1.0),
        ] {
            assert!(
                no.action.query_suggestion().is_none(),
                "{:?} should not offer a suggestion",
                no.action
            );
        }
    }
}
