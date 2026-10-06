//! User-defined static entries, read from the config file.
//!
//! The rows are declared under `[[engine.provider.builtin.actions.extra.entries]]`
//! in `config.toml` — one table per entry, each carrying a title, optional
//! display fields and keywords, and exactly one action (`exec`,
//! `terminal_exec`, or `clipboard`). Everything the launcher can do for a
//! `.desktop` file it can do for an entry the user typed by hand.

use std::collections::HashSet;
use std::sync::Arc;

use serde::Deserialize;

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
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
/// the title, and the history key is always `actions.<id>`.
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
/// two), an empty command, an id no title can supply, and a duplicate id —
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

    let mut builder = entry(id, &cfg.title).history_key(format!("{HISTORY_PREFIX}.{id}"));
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
    if let Some(args) = &cfg.exec {
        builder = builder.exec(args.clone());
    } else if let Some(args) = &cfg.terminal_exec {
        builder = builder.terminal_exec(args.clone());
    } else if let Some(value) = &cfg.clipboard {
        builder = builder.clipboard(value.clone());
    }
    builder.match_fields(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            crate::engine::provider::Action::Exec { args, terminal } => {
                assert_eq!(
                    args,
                    &[
                        "xdg-open".to_string(),
                        "/home/me/projects/huffi".to_string()
                    ]
                );
                assert!(!terminal);
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
            crate::engine::provider::Action::Exec { args, terminal } => {
                assert_eq!(args, &["htop".to_string()]);
                assert!(terminal);
            }
            other => panic!("expected terminal Exec, got {other:?}"),
        }
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
