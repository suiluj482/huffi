use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::engine::config::ExternalConfig;
use crate::engine::scoring::{MatchField, Rank};

use super::{Entry, EntryMeta, Icon, ProviderMeta, ProviderResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run `args` as a program. With `terminal`, the args are appended to
    /// the configured terminal wrapper before spawning.
    Exec { args: Vec<String>, terminal: bool },
    /// Copy `value` to the clipboard on selection. The clipboard binary is
    /// resolved from config when the action is performed.
    Clipboard { value: String },
    /// Do nothing; the default for entries that never set an action.
    NoOp,
}

impl Action {
    /// Perform the action, resolving external binaries (terminal wrapper,
    /// clipboard tool) from `external`.
    pub fn perform(&self, external: &ExternalConfig) {
        let args: Vec<String> = match self {
            Action::Exec {
                args,
                terminal: false,
            } => args.clone(),
            Action::Exec {
                args,
                terminal: true,
            } => {
                let mut cmd = external.terminal.clone();
                cmd.extend(args.iter().cloned());
                cmd
            }
            Action::Clipboard { value } => {
                vec![external.clipboard.clone(), value.clone()]
            }
            Action::NoOp => return,
        };
        let Some(program) = args.first() else {
            return;
        };
        let result = Command::new(program)
            .args(&args[1..])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Err(e) = result {
            eprintln!("failed to launch {program}: {e}");
        }
    }
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
    rank: Option<Rank>,
    history_key: Option<String>,
    set_query: Option<String>,
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
            terminal: false,
        });
        self
    }

    pub fn terminal_exec(mut self, args: Vec<String>) -> Self {
        self.action = Some(Action::Exec {
            args,
            terminal: true,
        });
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

    /// Set the query suggestion the UI applies when this entry is tab-selected.
    pub fn set_query(mut self, query: impl Into<String>) -> Self {
        self.set_query = Some(query.into());
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

    fn build(self) -> Entry {
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
                action: self.action.unwrap_or(Action::NoOp),
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
}
