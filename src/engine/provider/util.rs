use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::engine::config::ExternalConfig;
use crate::engine::scoring::{MatchField, Rank};

use super::{Entry, EntryMeta, Icon, ProviderMeta, ProviderResult, QuerySuggestion};

/// How an [`Action::Exec`] launches its args.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    /// Spawn the args directly, no wrapper.
    Direct,
    /// Append the args to the configured `terminal` wrapper; the terminal
    /// closes once the command exits.
    Terminal,
    /// Append the args to the configured `terminal_hold` wrapper; the
    /// terminal stays open after the command exits, so its output remains
    /// readable.
    TerminalHold,
}

impl ExecMode {
    /// The wrapper argv this mode prepends, `None` for [`Direct`](Self::Direct).
    fn wrapper(self, external: &ExternalConfig) -> Option<Vec<String>> {
        match self {
            ExecMode::Direct => None,
            ExecMode::Terminal => Some(external.terminal.clone()),
            ExecMode::TerminalHold => Some(external.terminal_hold.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run `args` as a program, wrapped according to `mode` — directly, in
    /// the configured terminal, or in a terminal that stays open. `cwd` is
    /// the directory to run in; when `None`, resolution falls back to the
    /// configured `working_dir`, then the user's home.
    Exec {
        args: Vec<String>,
        mode: ExecMode,
        cwd: Option<PathBuf>,
    },
    /// Copy `value` to the clipboard on selection. The clipboard binary is
    /// resolved from config when the action is performed.
    Clipboard { value: String },
    /// Replace the query with `suggestion` on selection and keep the
    /// launcher open — the Enter-triggered equivalent of tab-applying a
    /// suggestion. Nothing is spawned; the UI applies the suggestion, so
    /// `select` still records the launch and runs `Provider::handle` like
    /// any other selection.
    SetQuery { suggestion: QuerySuggestion },
    /// Do nothing; the default for entries that never set an action.
    NoOp,
}

impl Action {
    /// The argv to spawn for this action, resolving external binaries
    /// (terminal wrapper, clipboard tool) from `external`. `None` when
    /// there is nothing to run.
    fn argv(&self, external: &ExternalConfig) -> Option<Vec<String>> {
        let args: Vec<String> = match self {
            Action::Exec { args, mode, .. } => {
                let mut cmd = mode.wrapper(external).unwrap_or_default();
                cmd.extend(args.iter().cloned());
                cmd
            }
            Action::Clipboard { value } => {
                vec![external.clipboard.clone(), value.clone()]
            }
            Action::SetQuery { .. } | Action::NoOp => return None,
        };
        (!args.is_empty()).then_some(args)
    }

    /// The suggestion a [`SetQuery`](Self::SetQuery) action applies, or
    /// `None` for every other action.
    pub fn query_suggestion(&self) -> Option<&QuerySuggestion> {
        match self {
            Action::SetQuery { suggestion } => Some(suggestion),
            _ => None,
        }
    }

    /// The entry's own working directory, if it has one.
    fn cwd(&self) -> Option<&Path> {
        match self {
            Action::Exec { cwd, .. } => cwd.as_deref(),
            _ => None,
        }
    }

    /// Perform the action, resolving external binaries (terminal wrapper,
    /// clipboard tool) from `external`.
    pub fn perform(&self, external: &ExternalConfig) {
        let Some(args) = self.argv(external) else {
            return;
        };
        let Some(program) = args.first() else {
            return;
        };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut cmd = Command::new(program);
        cmd.args(&args[1..])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(dir) = resolve_cwd(self.cwd(), external, home.as_deref()) {
            cmd.current_dir(dir);
        }
        if let Err(e) = cmd.spawn() {
            eprintln!("failed to launch {program}: {e}");
        }
    }
}

/// Expand a leading `~/` against `home`, returning any other path as-is.
fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(home) = home
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

/// The directory to spawn an action in: the entry's own cwd (e.g. a
/// desktop file's `Path=`), else the configured `working_dir`, else the
/// user's home — the first candidate that exists. `None` means huffi's own
/// cwd is inherited. Explicit candidates that don't exist are reported
/// rather than silently skipped.
fn resolve_cwd(
    entry_cwd: Option<&Path>,
    external: &ExternalConfig,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let entry = entry_cwd.map(|p| expand_tilde(p, home));
    let configured = external
        .working_dir
        .as_deref()
        .map(|p| expand_tilde(p, home));
    let home = home.map(Path::to_path_buf);

    for (candidate, explicit) in [(entry, true), (configured, true), (home, false)] {
        let Some(dir) = candidate else { continue };
        if dir.is_dir() {
            return Some(dir);
        }
        if explicit {
            eprintln!(
                "huffi: working directory {} does not exist, trying the next candidate",
                dir.display()
            );
        }
    }
    None
}

/// Whether `key` is usable as a named-detail key: a non-empty run of
/// `[a-z0-9-]`, since the row renderer turns it into a `detail-<key>` GTK
/// object id and looks the widget up under exactly that name.
///
/// Enforced by `debug_assert!` in [`EntryBuilder::detail`] rather than a
/// panic. `Provider::query` returns a plain `Vec<Entry>`, so a provider has no
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
        provider_id: None,
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
    provider_id: Option<String>,
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
    pub fn provider(mut self, id: impl Into<String>) -> Self {
        self.provider_id = Some(id.into());
        self
    }

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
    /// See [`EntryMeta::details`].
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
    /// See [`EntryMeta::variant`].
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

    pub fn history_key(mut self, key: impl Into<String>) -> Self {
        self.history_key = Some(key.into());
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
            entry: EntryMeta {
                id: self.id,
                provider_id: self.provider_id,
                title: self.title,
                subtitle: self.subtitle,
                comment: self.comment,
                icon: self.icon,
                details: self.details,
                variant: self.variant,
                set_query: self.set_query,
                action,
            },
            rank: self.rank.unwrap_or(Rank::Score(1.0)),
            history_key: self.history_key,
        }
    }
}

pub fn split_command(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in s.chars() {
        match ch {
            '"' if !in_quotes => {
                in_quotes = true;
            }
            '"' if in_quotes => {
                in_quotes = false;
            }
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        result.push(current);
    }

    result
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

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
        let details = &meta.entry.details;
        assert_eq!(details["quantity"], "length");
        assert_eq!(details["exact"], "1/3");
        assert_eq!(details["dimensions"], "L T^-1");
        assert!(details.keys().all(|k| is_detail_key(k)));
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

    #[test]
    fn set_query_carries_the_whole_query() {
        let entry = entry("id", "title").set_query("=42").score(1.0);
        let suggestion = entry.entry.set_query.expect("a suggestion");
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
        let suggestion = entry.entry.set_query.expect("a suggestion");
        assert!(suggestion.keep_prefix);
        assert_eq!(suggestion.query, "42");
        assert_eq!(suggestion.resolve(Some("#")), "#42");
    }

    /// The flag belongs to the suggestion, so it cannot be set without one.
    #[test]
    fn an_entry_without_a_suggestion_has_nothing_to_resolve() {
        let entry = entry("id", "title").score(1.0);
        assert!(entry.entry.set_query.is_none());
    }

    #[test]
    fn action_argv_resolves_the_configured_wrappers() {
        let external = ExternalConfig::default();
        let exec = Action::Exec {
            args: vec!["ls".into()],
            mode: ExecMode::Direct,
            cwd: None,
        };
        assert_eq!(exec.argv(&external), Some(vec!["ls".to_string()]));

        let terminal = Action::Exec {
            args: vec!["ls".into()],
            mode: ExecMode::Terminal,
            cwd: None,
        };
        assert_eq!(
            terminal.argv(&external),
            Some(vec![
                "kitty".to_string(),
                "--".to_string(),
                "ls".to_string()
            ])
        );

        let hold = Action::Exec {
            args: vec!["sh".into(), "-c".into(), "ls".into()],
            mode: ExecMode::TerminalHold,
            cwd: None,
        };
        assert_eq!(
            hold.argv(&external),
            Some(vec![
                "kitty".to_string(),
                "--hold".to_string(),
                "--".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "ls".to_string(),
            ])
        );

        let clipboard = Action::Clipboard { value: "x".into() };
        assert_eq!(
            clipboard.argv(&external),
            Some(vec!["wl-copy".to_string(), "x".to_string()])
        );

        assert_eq!(Action::NoOp.argv(&external), None);
        assert_eq!(
            Action::SetQuery {
                suggestion: QuerySuggestion::new("42")
            }
            .argv(&external),
            None,
            "a query suggestion spawns nothing"
        );
        assert_eq!(
            Action::Exec {
                args: vec![],
                mode: ExecMode::Direct,
                cwd: None,
            }
            .argv(&external),
            None
        );
    }

    #[test]
    pub fn builders_pick_the_exec_mode() {
        let ls = || vec!["ls".to_string()];
        let direct = entry("id", "title").exec(ls()).score(1.0);
        assert_eq!(
            direct.entry.action,
            Action::Exec {
                args: ls(),
                mode: ExecMode::Direct,
                cwd: None
            }
        );

        let terminal = entry("id", "title").terminal_exec(ls()).score(1.0);
        assert_eq!(
            terminal.entry.action,
            Action::Exec {
                args: ls(),
                mode: ExecMode::Terminal,
                cwd: None
            }
        );

        let hold = entry("id", "title").terminal_hold(ls()).score(1.0);
        assert_eq!(
            hold.entry.action,
            Action::Exec {
                args: ls(),
                mode: ExecMode::TerminalHold,
                cwd: None
            }
        );
    }

    /// `cwd()` is stored on the builder and merged into the exec action in
    /// `build()`, so providers may call it before or after picking a mode.
    #[test]
    fn cwd_merges_into_the_exec_action_in_either_chain_order() {
        let ls = || vec!["ls".to_string()];
        let after = entry("id", "title").exec(ls()).cwd("/tmp").score(1.0);
        let before = entry("id", "title").cwd("/tmp").exec(ls()).score(1.0);
        assert_eq!(after.entry.action, before.entry.action);
        match &after.entry.action {
            Action::Exec { cwd, .. } => assert_eq!(cwd.as_deref(), Some(Path::new("/tmp"))),
            other => panic!("expected exec action, got {other:?}"),
        }
    }

    /// Entry cwd, then configured `working_dir`, then `$HOME`: the first
    /// candidate that exists on disk. Nothing usable means huffi's own cwd
    /// is inherited.
    #[test]
    fn resolve_cwd_walks_entry_config_then_home() {
        let root =
            std::env::temp_dir().join(format!("huffi-cwd-precedence-{}", std::process::id()));
        let entry_dir = root.join("entry");
        let config_dir = root.join("config");
        let home_dir = root.join("home");
        for dir in [&entry_dir, &config_dir, &home_dir] {
            std::fs::create_dir_all(dir).expect("temp cwd dir");
        }
        let missing = root.join("missing");
        let external = |dir: Option<&Path>| ExternalConfig {
            working_dir: dir.map(Path::to_path_buf),
            ..ExternalConfig::default()
        };

        assert_eq!(
            resolve_cwd(
                Some(entry_dir.as_path()),
                &external(Some(config_dir.as_path())),
                Some(home_dir.as_path())
            ),
            Some(entry_dir.clone()),
            "the entry's own cwd wins"
        );
        assert_eq!(
            resolve_cwd(
                Some(missing.as_path()),
                &external(Some(config_dir.as_path())),
                Some(home_dir.as_path())
            ),
            Some(config_dir.clone()),
            "a missing entry cwd falls through to the config"
        );
        assert_eq!(
            resolve_cwd(
                Some(missing.as_path()),
                &external(Some(missing.as_path())),
                Some(home_dir.as_path())
            ),
            Some(home_dir.clone()),
            "missing entry and config fall through to home"
        );
        assert_eq!(
            resolve_cwd(None, &external(None), Some(home_dir.as_path())),
            Some(home_dir.clone()),
            "home alone is enough"
        );
        assert_eq!(
            resolve_cwd(None, &external(None), None),
            None,
            "no candidates: inherit huffi's own cwd"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Both a provider-supplied cwd and the config key accept a leading
    /// `~/`, since neither is written as an absolute path in practice.
    #[test]
    fn resolve_cwd_expands_a_leading_tilde() {
        let home = std::env::temp_dir().join(format!("huffi-cwd-tilde-{}", std::process::id()));
        let projects = home.join("projects");
        std::fs::create_dir_all(&projects).expect("temp home dir");

        let external = ExternalConfig {
            working_dir: Some(PathBuf::from("~/projects")),
            ..ExternalConfig::default()
        };
        assert_eq!(
            resolve_cwd(None, &external, Some(home.as_path())),
            Some(projects.clone())
        );
        assert_eq!(
            resolve_cwd(
                Some(Path::new("~/projects")),
                &ExternalConfig::default(),
                Some(home.as_path())
            ),
            Some(projects),
            "entry cwd expands too"
        );

        std::fs::remove_dir_all(&home).ok();
    }

    /// Tab applies `set_query`, Enter applies `action_set_query`, and the
    /// two carry independent values on the same entry.
    #[test]
    fn the_enter_suggestion_is_independent_of_the_tab_suggestion() {
        let e = entry("id", "title")
            .set_query("tab value")
            .action_set_query("enter value")
            .score(1.0);

        let tab = e.entry.set_query.expect("tab suggestion");
        assert_eq!(tab.query, "tab value");
        assert!(!tab.keep_prefix);
        assert_eq!(
            e.entry.action.query_suggestion().map(|s| s.query.as_str()),
            Some("enter value"),
            "Enter applies its own value, not the tab one"
        );
    }

    #[test]
    fn action_set_query_keeps_the_prefix_when_asked() {
        let e = entry("id", "title")
            .action_set_query_keeping_prefix("u+2603")
            .score(1.0);
        let suggestion = e
            .entry
            .action
            .query_suggestion()
            .expect("a query suggestion");
        assert!(suggestion.keep_prefix);
        assert_eq!(suggestion.resolve(Some(":")), ":u+2603");
        assert!(
            e.entry.set_query.is_none(),
            "the tab suggestion stays unset"
        );
    }

    /// The accessor is how the UI pulls the suggestion out of the row
    /// without matching on action variants itself.
    #[test]
    fn only_the_set_query_action_exposes_a_suggestion() {
        let ls = || vec!["ls".to_string()];
        let with = entry("id", "t").action_set_query("q").score(1.0);
        assert!(with.entry.action.query_suggestion().is_some());

        for no in [
            entry("a", "t").exec(ls()).score(1.0),
            entry("b", "t").clipboard("x").score(1.0),
            entry("c", "t").set_query("tab-only").score(1.0),
            entry("d", "t").score(1.0),
        ] {
            assert!(
                no.entry.action.query_suggestion().is_none(),
                "{:?} should not offer a suggestion",
                no.entry.action
            );
        }
    }
}
