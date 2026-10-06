//! User-defined static entries, read from the config file.
//!
//! The rows are declared under `[[engine.provider.builtin.actions.extra.entries]]`
//! in `config.toml` — one table per entry, each carrying a title, optional
//! display fields and keywords, and exactly one action (`exec`,
//! `terminal_exec`, or `clipboard`), plus the theme-facing fields every
//! entry builder offers: details, a layout variant, query suggestions, and
//! history controls.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry, is_detail_key,
    parse_extra_config,
};
use crate::engine::scoring::MatchField;

/// History namespace for configured entries: every entry's history key is
/// `actions.<id>`, so it can never collide with a key from another provider.
const HISTORY_PREFIX: &str = "actions";

/// The `extra` config of the actions provider: the entries themselves plus
/// the fuzzy-match weights applied to them. When provided via
/// `[engine.provider.builtin.actions.extra]`, the fields are parsed from the
/// arbitrary extra config, mirroring [`super::DesktopConfig`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct ActionsConfig {
    /// The configured entries, in config order.
    pub entries: Vec<ActionEntry>,
    /// Fuzzy-match weight for each entry's title.
    pub weight_title: f32,
    /// Fuzzy-match weight for each entry's keywords.
    pub weight_keyword: f32,
}

impl Default for ActionsConfig {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            weight_title: 1.0,
            weight_keyword: 0.8,
        }
    }
}

/// One configured entry: a row the launcher offers verbatim.
///
/// Exactly one of `exec`, `terminal_exec`, and `clipboard` must be set —
/// an entry with none has nothing to do when selected, an entry with two
/// has two contradictory things. `id` is optional: it defaults to a slug of
/// the title, and the history key defaults to `actions.<id>`. The remaining
/// fields mirror [`EntryBuilder`](crate::engine::provider::EntryBuilder)
/// one for one; anything left unset keeps the builder's own default.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ActionEntry {
    /// Stable id within the provider; defaults to a slug of [`title`](Self::title).
    #[serde(default)]
    pub id: Option<String>,
    /// Primary label, and a fuzzy-match field at `weight_title`.
    pub title: String,
    /// Secondary label shown below the title.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Long-form prose, shown only by a theme that declares a `comment`
    /// widget, and never fuzzy-matched.
    #[serde(default)]
    pub comment: Option<String>,
    /// Freedesktop icon theme name, e.g. `"system-lock-screen"`.
    #[serde(default)]
    pub icon: Option<String>,
    /// Explicit icon file path (PNG or SVG), for an icon no theme carries.
    #[serde(default)]
    pub icon_path: Option<String>,
    /// Extra fuzzy-match fields, each at `weight_keyword`.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Named display fields for the row's `detail-<key>` widgets. Keys must
    /// match `[a-z0-9-]+`, values are strings — quote numbers in TOML.
    #[serde(default)]
    pub details: BTreeMap<String, String>,
    /// Row layout variant, e.g. `"info"`: selects a theme template and adds
    /// `variant-<name>` CSS classes. Must match `[a-z0-9-]+`.
    #[serde(default)]
    pub variant: Option<String>,
    /// Query suggestion applied when the row is tab-selected; replaces the
    /// whole query. Mutually exclusive with
    /// [`set_query_keeping_prefix`](Self::set_query_keeping_prefix).
    #[serde(default)]
    pub set_query: Option<String>,
    /// Query suggestion applied under the active prefix, which stays in
    /// front of it. The one to use for refining the query that found the
    /// row, since `prefixes` is user-configurable.
    #[serde(default)]
    pub set_query_keeping_prefix: Option<String>,
    /// Whether launches of this entry are recorded in the history model.
    /// Defaults to `true`; `false` leaves the row unlearnable. Mutually
    /// exclusive with [`history_key`](Self::history_key).
    #[serde(default = "default_true")]
    pub history: bool,
    /// Override the history key, which otherwise derives as `actions.<id>`.
    #[serde(default)]
    pub history_key: Option<String>,
    /// Directory the command runs in, overriding the configured
    /// `working_dir` for this entry. A leading `~/` expands against `$HOME`.
    /// Only meaningful for [`exec`](Self::exec) and
    /// [`terminal_exec`](Self::terminal_exec) — nothing else runs anywhere.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Command to run on selection (no terminal).
    #[serde(default)]
    pub exec: Option<Vec<String>>,
    /// Command to run in the configured terminal on selection.
    #[serde(default)]
    pub terminal_exec: Option<Vec<String>>,
    /// Text to copy to the clipboard on selection.
    #[serde(default)]
    pub clipboard: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Provides the entries a user configured under
/// `[engine.provider.builtin.actions.extra]`.
///
/// Always active — it ships with no prefix, so a configured action competes
/// for every query like a desktop entry, and usage history decides how high
/// it ranks. An unconfigured provider returns nothing, so it costs one
/// (allocation-free) call per keystroke and no rows.
///
/// The rows are built once in [`Provider::init`]; `query` hands back the
/// cached list, exactly like [`super::DesktopEntryProvider`].
pub struct ActionsProvider {
    entries: Arc<[Entry]>,
}

impl ActionsProvider {
    pub fn new() -> Self {
        Self {
            entries: Arc::from([]),
        }
    }
}

impl Default for ActionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for ActionsProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta::builder("actions").build()
    }

    fn init(&mut self, ctx: InitContext) -> ProviderResult {
        let config = match parse_extra_config::<ActionsConfig>(&ctx.extra) {
            Err(result) => return result,
            Ok(None) => return ProviderResult::Ok,
            Ok(Some(config)) => config,
        };
        let resolved = match resolve(&config.entries) {
            Ok(resolved) => resolved,
            Err(msg) => {
                return ProviderResult::Config {
                    msg: format!("invalid extra config: {msg}"),
                    critical: false,
                };
            }
        };
        let weights = Weights {
            title: config.weight_title,
            keyword: config.weight_keyword,
        };
        self.entries = Arc::from(
            resolved
                .into_iter()
                .map(|(id, cfg)| build(&id, cfg, weights))
                .collect::<Vec<_>>(),
        );
        ProviderResult::Ok
    }

    fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
        self.entries.to_vec()
    }
}

/// The match weights in effect for one `init`.
#[derive(Debug, Clone, Copy)]
struct Weights {
    title: f32,
    keyword: f32,
}

/// Resolve every configured entry to its final id, rejecting anything the
/// launcher could not dispatch or learn from: an entry with no action (or
/// two), an empty command, a `cwd` with no command to run it in, an id no
/// title can supply, and a duplicate id —
/// duplicates would make selection dispatch and history ambiguous.
fn resolve(entries: &[ActionEntry]) -> Result<Vec<(String, &ActionEntry)>, String> {
    let mut seen = HashSet::new();
    let mut resolved = Vec::with_capacity(entries.len());
    for (i, cfg) in entries.iter().enumerate() {
        let label = format!("entries[{i}] ({:?})", cfg.title);
        let actions = usize::from(cfg.exec.is_some())
            + usize::from(cfg.terminal_exec.is_some())
            + usize::from(cfg.clipboard.is_some());
        if actions != 1 {
            return Err(format!(
                "{label}: exactly one of exec, terminal_exec, clipboard is required"
            ));
        }
        if cfg.exec.as_ref().is_some_and(Vec::is_empty)
            || cfg.terminal_exec.as_ref().is_some_and(Vec::is_empty)
        {
            return Err(format!("{label}: command must not be empty"));
        }
        if cfg.cwd.is_some() && cfg.exec.is_none() && cfg.terminal_exec.is_none() {
            return Err(format!("{label}: cwd requires exec or terminal_exec"));
        }
        if cfg
            .cwd
            .as_ref()
            .is_some_and(|cwd| cwd.as_os_str().is_empty())
        {
            return Err(format!("{label}: cwd must not be empty"));
        }
        if cfg.set_query.is_some() && cfg.set_query_keeping_prefix.is_some() {
            return Err(format!(
                "{label}: set_query and set_query_keeping_prefix are mutually exclusive"
            ));
        }
        // Validated here rather than left to `EntryBuilder::detail`, whose
        // `debug_assert!` would panic a debug build on a bad key from config.
        for key in cfg.details.keys() {
            if !is_detail_key(key) {
                return Err(format!("{label}: detail key {key:?} must match [a-z0-9-]+"));
            }
        }
        if let Some(variant) = &cfg.variant {
            // A variant becomes a CSS class (`variant-<variant>`) and a path
            // segment of the theme's template lookup, so it gets the same
            // charset detail keys get — which also rules out `../`.
            if !is_detail_key(variant) {
                return Err(format!(
                    "{label}: variant {variant:?} must match [a-z0-9-]+"
                ));
            }
        }
        if !cfg.history && cfg.history_key.is_some() {
            return Err(format!(
                "{label}: history = false conflicts with history_key"
            ));
        }
        if cfg.history_key.as_deref() == Some("") {
            return Err(format!("{label}: history_key must not be empty"));
        }
        let id = cfg
            .id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| slug(&cfg.title));
        if id.is_empty() {
            return Err(format!(
                "{label}: title yields an empty id, set id explicitly"
            ));
        }
        if !seen.insert(id.clone()) {
            return Err(format!("{label}: duplicate id {id:?}"));
        }
        resolved.push((id, cfg));
    }
    Ok(resolved)
}

/// Derive an entry id from a title: ASCII lowercase, runs of anything else
/// collapsed to one `-`. `"Open project huffi"` → `"open-project-huffi"`.
///
/// The slug is only a default — a title edit re-keys the entry and starts
/// its history over, which is what an explicit `id` is for.
fn slug(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut pending_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() {
            pending_dash = true;
        }
    }
    out
}

/// Build one configured row: display fields as configured, the action
/// verbatim, and the match fields the fuzzy matcher sees — the title plus
/// each keyword. Subtitle and comment are display-only, exactly as for
/// [`super::DesktopEntryProvider`].
fn build(id: &str, cfg: &ActionEntry, weights: Weights) -> Entry {
    let mut fields = vec![MatchField {
        text: cfg.title.clone(),
        weight: weights.title,
    }];
    for keyword in &cfg.keywords {
        let keyword = keyword.trim();
        if !keyword.is_empty() {
            fields.push(MatchField {
                text: keyword.to_owned(),
                weight: weights.keyword,
            });
        }
    }

    let mut builder = entry(id, &cfg.title);
    if cfg.history {
        builder = builder.history_key(
            cfg.history_key
                .clone()
                .unwrap_or_else(|| format!("{HISTORY_PREFIX}.{id}")),
        );
    }
    if let Some(subtitle) = &cfg.subtitle {
        builder = builder.subtitle(subtitle);
    }
    if let Some(comment) = &cfg.comment {
        builder = builder.comment(comment);
    }
    if let Some(icon) = &cfg.icon {
        builder = builder.icon_name(icon);
    }
    if let Some(path) = &cfg.icon_path {
        builder = builder.icon_path(path);
    }
    for (key, value) in &cfg.details {
        builder = builder.detail(key, value);
    }
    if let Some(variant) = &cfg.variant {
        builder = builder.variant(variant);
    }
    if let Some(query) = &cfg.set_query {
        builder = builder.set_query(query);
    } else if let Some(query) = &cfg.set_query_keeping_prefix {
        builder = builder.set_query_keeping_prefix(query);
    }
    if let Some(args) = &cfg.exec {
        builder = builder.exec(args.clone());
    } else if let Some(args) = &cfg.terminal_exec {
        builder = builder.terminal_exec(args.clone());
    } else if let Some(value) = &cfg.clipboard {
        builder = builder.clipboard(value.clone());
    }
    if let Some(cwd) = &cfg.cwd {
        builder = builder.cwd(cwd);
    }
    builder.match_fields(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::ExecMode;

    /// The provider never reads `data_dir`, so the path is a placeholder.
    fn init(extra: Option<serde_json::Value>) -> (ActionsProvider, ProviderResult) {
        let mut provider = ActionsProvider::new();
        let dir = std::path::Path::new("/tmp/huffi-actions-test");
        let result = provider.init(InitContext {
            data_dir: dir,
            extra,
        });
        (provider, result)
    }

    fn config_error(extra: serde_json::Value) -> String {
        match init(Some(extra)).1 {
            ProviderResult::Config { msg, critical } => {
                assert!(
                    !critical,
                    "a bad entries config must not disable the provider"
                );
                msg
            }
            other => panic!("expected a Config error, got {:?}", other),
        }
    }

    #[test]
    fn without_extra_the_provider_serves_nothing() {
        let (mut provider, result) = init(None);
        assert!(matches!(result, ProviderResult::Ok));
        let entries = provider.query(QueryContext {
            prefix: None,
            query: "",
            original: "",
        });
        assert!(entries.is_empty());
    }

    #[test]
    fn entries_are_built_from_config() {
        let (mut provider, result) = init(Some(serde_json::json!({
            "weight_title": 1.0,
            "weight_keyword": 0.5,
            "entries": [
                {
                    "title": "Open project huffi",
                    "subtitle": "Projects",
                    "comment": "The launcher itself",
                    "icon": "folder",
                    "keywords": ["code", "  ", "rust"],
                    "exec": ["xdg-open", "/home/me/projects/huffi"],
                },
                {
                    "id": "date",
                    "title": "Copy date",
                    "clipboard": "2026-10-06",
                },
            ],
        })));
        assert!(matches!(result, ProviderResult::Ok));

        let entries = provider.query(QueryContext {
            prefix: None,
            query: "",
            original: "",
        });
        assert_eq!(entries.len(), 2);

        let first = &entries[0];
        assert_eq!(first.entry.id, "open-project-huffi");
        assert_eq!(first.entry.title, "Open project huffi");
        assert_eq!(first.entry.subtitle.as_deref(), Some("Projects"));
        assert_eq!(first.entry.comment.as_deref(), Some("The launcher itself"));
        assert_eq!(
            first.history_key.as_deref(),
            Some("actions.open-project-huffi")
        );
        match &first.entry.action {
            crate::engine::provider::Action::Exec { args, mode, .. } => {
                assert_eq!(
                    args,
                    &[
                        "xdg-open".to_string(),
                        "/home/me/projects/huffi".to_string()
                    ]
                );
                assert_eq!(*mode, ExecMode::Direct);
            }
            other => panic!("expected Exec, got {other:?}"),
        }
        // Title plus each keyword — the whitespace-only keyword is dropped.
        let fields = match &first.rank {
            crate::engine::scoring::Rank::MatchFields(fields) => fields,
            other => panic!("expected match fields, got {other:?}"),
        };
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].text, "Open project huffi");
        assert_eq!(fields[0].weight, 1.0);
        assert_eq!(fields[1].text, "code");
        assert_eq!(fields[1].weight, 0.5);
        assert_eq!(fields[2].text, "rust");
        assert_eq!(fields[2].weight, 0.5);

        let second = &entries[1];
        assert_eq!(
            second.entry.id, "date",
            "an explicit id wins over the title"
        );
        assert_eq!(second.history_key.as_deref(), Some("actions.date"));
        match &second.entry.action {
            crate::engine::provider::Action::Clipboard { value } => {
                assert_eq!(value, "2026-10-06")
            }
            other => panic!("expected Clipboard, got {other:?}"),
        }
    }

    #[test]
    fn terminal_exec_lands_in_a_terminal() {
        let (mut provider, result) = init(Some(serde_json::json!({
            "entries": [
                { "title": "Htop", "terminal_exec": ["htop"] },
            ],
        })));
        assert!(matches!(result, ProviderResult::Ok));
        let entries = provider.query(QueryContext {
            prefix: None,
            query: "",
            original: "",
        });
        match &entries[0].entry.action {
            crate::engine::provider::Action::Exec { args, mode, .. } => {
                assert_eq!(args, &["htop".to_string()]);
                assert_eq!(*mode, ExecMode::Terminal);
            }
            other => panic!("expected terminal Exec, got {other:?}"),
        }
    }

    /// The configured working directory lands on the exec action, without
    /// disturbing its mode; a terminal therefore starts there too.
    #[test]
    fn cwd_reaches_the_exec_action() {
        let (mut provider, result) = init(Some(serde_json::json!({
            "entries": [
                {
                    "title": "Build",
                    "exec": ["make"],
                    "cwd": "/home/me/projects/huffi",
                },
                {
                    "title": "Shell",
                    "terminal_exec": ["zsh"],
                    "cwd": "/tmp",
                },
            ],
        })));
        assert!(matches!(result, ProviderResult::Ok));
        let entries = provider.query(QueryContext {
            prefix: None,
            query: "",
            original: "",
        });
        match &entries[0].entry.action {
            crate::engine::provider::Action::Exec { mode, cwd, .. } => {
                assert_eq!(*mode, ExecMode::Direct);
                assert_eq!(
                    cwd.as_deref(),
                    Some(std::path::Path::new("/home/me/projects/huffi"))
                );
            }
            other => panic!("expected Exec, got {other:?}"),
        }
        match &entries[1].entry.action {
            crate::engine::provider::Action::Exec { mode, cwd, .. } => {
                assert_eq!(*mode, ExecMode::Terminal);
                assert_eq!(cwd.as_deref(), Some(std::path::Path::new("/tmp")));
            }
            other => panic!("expected terminal Exec, got {other:?}"),
        }
    }

    /// `EntryBuilder::cwd` has nowhere to put the directory when the action
    /// spawns nothing, so the config check has to fire first.
    #[test]
    fn cwd_without_a_command_is_rejected() {
        let msg = config_error(serde_json::json!({
            "entries": [{ "title": "Copy", "clipboard": "x", "cwd": "/tmp" }],
        }));
        assert!(msg.contains("cwd requires exec or terminal_exec"), "{msg}");
    }

    #[test]
    fn an_empty_cwd_is_rejected() {
        let msg = config_error(serde_json::json!({
            "entries": [{ "title": "Nowhere", "exec": ["true"], "cwd": "" }],
        }));
        assert!(msg.contains("cwd must not be empty"), "{msg}");
    }

    /// An entry with nothing to do on selection is a config mistake, not a
    /// row that silently does nothing.
    #[test]
    fn an_entry_without_an_action_is_rejected() {
        let msg = config_error(serde_json::json!({
            "entries": [{ "title": "Nothing" }],
        }));
        assert!(
            msg.contains("exactly one of exec, terminal_exec, clipboard"),
            "{msg}"
        );
        assert!(msg.contains("entries[0]"), "{msg}");
    }

    #[test]
    fn an_entry_with_two_actions_is_rejected() {
        let msg = config_error(serde_json::json!({
            "entries": [{
                "title": "Ambiguous",
                "exec": ["true"],
                "clipboard": "hi",
            }],
        }));
        assert!(
            msg.contains("exactly one of exec, terminal_exec, clipboard"),
            "{msg}"
        );
    }

    #[test]
    fn an_empty_command_is_rejected() {
        let msg = config_error(serde_json::json!({
            "entries": [{ "title": "Nothing", "exec": [] }],
        }));
        assert!(msg.contains("command must not be empty"), "{msg}");
    }

    /// Two entries the title slug resolves to the same id would fight over
    /// selection dispatch and history, so the whole config is refused.
    #[test]
    fn duplicate_ids_are_rejected() {
        let msg = config_error(serde_json::json!({
            "entries": [
                { "title": "Suspend", "exec": ["systemctl", "suspend"] },
                { "title": "Suspend!", "exec": ["systemctl", "suspend", "-i"] },
            ],
        }));
        assert!(msg.contains("duplicate id \"suspend\""), "{msg}");
    }

    /// The theme-facing fields reach the row: named details for
    /// `detail-<key>` widgets, a variant for template + CSS class selection,
    /// and a Tab suggestion under whatever prefix is active.
    #[test]
    fn details_variant_and_suggestions_reach_the_entry() {
        let (mut provider, result) = init(Some(serde_json::json!({
            "entries": [{
                "title": "Volume",
                "exec": ["pavucontrol"],
                "variant": "info",
                "details": { "level": "80%", "sink": "speakers" },
                "set_query_keeping_prefix": "mute",
            }],
        })));
        assert!(matches!(result, ProviderResult::Ok));
        let entries = provider.query(QueryContext {
            prefix: None,
            query: "",
            original: "",
        });
        let row = &entries[0];
        assert_eq!(row.entry.details["level"], "80%");
        assert_eq!(row.entry.details["sink"], "speakers");
        assert_eq!(row.entry.variant.as_deref(), Some("info"));
        let suggestion = row.entry.set_query.as_ref().expect("a suggestion");
        assert!(suggestion.keep_prefix);
        assert_eq!(suggestion.query, "mute");
    }

    /// A detail key reaching `EntryBuilder::detail` would hit its
    /// `debug_assert!` and panic this test; the config check has to fire
    /// first, as a plain config error.
    #[test]
    fn a_bad_detail_key_is_a_config_error_not_a_panic() {
        let msg = config_error(serde_json::json!({
            "entries": [{
                "title": "Broken",
                "exec": ["true"],
                "details": { "Not A Key": "value" },
            }],
        }));
        assert!(msg.contains("detail key \"Not A Key\""), "{msg}");
        assert!(msg.contains("[a-z0-9-]+"), "{msg}");
    }

    /// The variant is a CSS class and a path segment, so it takes the same
    /// charset as a detail key — `../` included in what it must not be.
    #[test]
    fn a_bad_variant_is_rejected() {
        for variant in ["Info Row", "../other"] {
            let msg = config_error(serde_json::json!({
                "entries": [{
                    "title": "Broken",
                    "exec": ["true"],
                    "variant": variant,
                }],
            }));
            assert!(msg.contains("variant"), "{msg}: {variant}");
            assert!(msg.contains("[a-z0-9-]+"), "{msg}: {variant}");
        }
    }

    #[test]
    fn two_query_suggestions_are_rejected() {
        let msg = config_error(serde_json::json!({
            "entries": [{
                "title": "Ambiguous",
                "exec": ["true"],
                "set_query": "a",
                "set_query_keeping_prefix": "b",
            }],
        }));
        assert!(
            msg.contains("set_query and set_query_keeping_prefix are mutually exclusive"),
            "{msg}"
        );
    }

    #[test]
    fn history_false_drops_the_history_key() {
        let (mut provider, result) = init(Some(serde_json::json!({
            "entries": [{
                "title": "Throwaway",
                "clipboard": "x",
                "history": false,
            }],
        })));
        assert!(matches!(result, ProviderResult::Ok));
        let entries = provider.query(QueryContext {
            prefix: None,
            query: "",
            original: "",
        });
        assert!(
            entries[0].history_key.is_none(),
            "an opted-out row must be invisible to the history model"
        );
        assert_eq!(entries[0].entry.id, "throwaway");
    }

    #[test]
    fn history_key_overrides_the_derived_key() {
        let (mut provider, result) = init(Some(serde_json::json!({
            "entries": [{
                "title": "Renamed later",
                "exec": ["true"],
                "history_key": "legacy-key",
            }],
        })));
        assert!(matches!(result, ProviderResult::Ok));
        let entries = provider.query(QueryContext {
            prefix: None,
            query: "",
            original: "",
        });
        assert_eq!(entries[0].history_key.as_deref(), Some("legacy-key"));
    }

    #[test]
    fn history_false_with_a_history_key_is_rejected() {
        let msg = config_error(serde_json::json!({
            "entries": [{
                "title": "Contradiction",
                "exec": ["true"],
                "history": false,
                "history_key": "why",
            }],
        }));
        assert!(
            msg.contains("history = false conflicts with history_key"),
            "{msg}"
        );
    }

    #[test]
    fn an_empty_history_key_is_rejected() {
        let msg = config_error(serde_json::json!({
            "entries": [{
                "title": "Empty",
                "exec": ["true"],
                "history_key": "",
            }],
        }));
        assert!(msg.contains("history_key must not be empty"), "{msg}");
    }

    /// A title with nothing to slug (an emoji, say) cannot supply an id.
    #[test]
    fn a_title_without_letters_needs_an_explicit_id() {
        let msg = config_error(serde_json::json!({
            "entries": [{ "title": "☃", "clipboard": "snow" }],
        }));
        assert!(msg.contains("title yields an empty id"), "{msg}");
    }

    #[test]
    fn a_title_without_letters_is_fine_with_an_explicit_id() {
        let (mut provider, result) = init(Some(serde_json::json!({
            "entries": [{ "id": "snow", "title": "☃", "clipboard": "snow" }],
        })));
        assert!(matches!(result, ProviderResult::Ok));
        let entries = provider.query(QueryContext {
            prefix: None,
            query: "",
            original: "",
        });
        assert_eq!(entries[0].entry.id, "snow");
        assert_eq!(entries[0].history_key.as_deref(), Some("actions.snow"));
    }

    #[test]
    fn slugs_collapse_separators_and_case() {
        assert_eq!(slug("Open project huffi"), "open-project-huffi");
        assert_eq!(slug("  Suspend  "), "suspend");
        assert_eq!(slug("it's-a-test"), "it-s-a-test");
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug("2 + 2"), "2-2");
    }
}
