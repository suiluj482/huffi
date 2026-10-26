use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
    parse_extra_config,
};

/// Per-provider tuning knobs. When provided via
/// `[engine.provider.builtin.cliphist.extra]`, the fields are parsed from the
/// arbitrary extra config.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default)]
pub struct CliphistConfig {
    /// Keep only the `max_entries` most recent clipboard entries.
    /// `0` means unlimited. The cap is applied once at query time.
    pub max_entries: usize,
}

impl Default for CliphistConfig {
    fn default() -> Self {
        Self { max_entries: 1000 }
    }
}

/// Provides clipboard history from `cliphist` as copy-to-clipboard entries.
///
/// Triggered by the `|` prefix by default (configurable). Each entry copies
/// its stored clipboard content back to the clipboard when selected.
///
/// Clipboard history changes frequently, so it is not cached at init, but
/// spawning `cliphist list` on every keystroke is too slow — entries are
/// fetched on the first query of each launcher session
/// ([`Generation::session`](crate::engine::provider::Generation::session))
/// and reused for the rest of the session, so they may be slightly stale
/// while the launcher stays open.
pub struct CliphistProvider {
    config: CliphistConfig,
    /// Entries fetched for `loaded_session`, reused until the next session.
    entries: Vec<Entry>,
    /// The session `entries` was fetched for; `None` before the first query.
    loaded_session: Option<u64>,
}

impl Default for CliphistProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CliphistProvider {
    pub fn new() -> Self {
        Self {
            config: CliphistConfig::default(),
            entries: Vec::new(),
            loaded_session: None,
        }
    }
}

impl Provider for CliphistProvider {
    fn meta(&self) -> ProviderMeta {
        // Trigger prefix: `|some text` searches clipboard history.
        ProviderMeta::builder("cliphist")
            .prefix("|")
            .prefix_only(true)
            .build()
    }

    fn init(&mut self, ctx: InitContext) -> ProviderResult {
        match parse_extra_config::<CliphistConfig>(&ctx.extra) {
            Err(result) => return result,
            Ok(Some(config)) => self.config = config,
            Ok(None) => {}
        }

        if !cliphist_available() {
            return ProviderResult::Unsupported("cliphist binary not found in PATH".into());
        }

        ProviderResult::Ok
    }

    fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
        if self.loaded_session != Some(ctx.generation.session) {
            self.entries = load_cliphist(self.config.max_entries);
            self.loaded_session = Some(ctx.generation.session);
        }
        self.entries.clone()
    }
}

fn cliphist_available() -> bool {
    Command::new("cliphist")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn load_cliphist(max: usize) -> Vec<Entry> {
    let Ok(out) = Command::new("cliphist").arg("list").output() else {
        return Vec::new();
    };

    if !out.status.success() {
        return Vec::new();
    }

    let Ok(text) = String::from_utf8(out.stdout) else {
        return Vec::new();
    };

    entries_from_list(&text, max)
}

fn entries_from_list(text: &str, max: usize) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut count = 0;
    for line in text.lines() {
        if max > 0 && count >= max {
            break;
        }
        // cliphist list format: "<id>\t<preview>". The preview has whitespace
        // collapsed and is truncated by cliphist, and images are represented
        // by a description such as "[[ binary data 2 MiB png 2736x2272 ]]".
        // Only `cliphist decode <id>` yields the original bytes, so selection
        // must decode by id rather than copy the preview.
        let mut parts = line.splitn(2, '\t');
        let id = parts.next().unwrap_or("").trim();
        let preview = parts.next().unwrap_or("").trim_end_matches('\r');
        if id.is_empty() || preview.is_empty() {
            continue;
        }

        // cliphist already truncates the preview to its configured width, so
        // use it verbatim instead of shortening it again. It is what the
        // engine fuzzy-matches; copying goes back to cliphist for the exact
        // text or binary data.
        let entry = entry(format!("cliphist:{id}"), preview)
            .icon_name("edit-paste-symbolic")
            .clipboard_command(vec!["cliphist".into(), "decode".into(), id.into()])
            .match_field(preview);

        entries.push(entry);
        count += 1;
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::Action;
    use crate::engine::scoring::Rank;

    fn copy_command(row: &Entry) -> Vec<String> {
        let Action::ClipboardCommand { args } = &row.entry.action else {
            panic!(
                "expected a cliphist decode command, got {:?}",
                row.entry.action
            );
        };
        args.clone()
    }

    fn match_text(row: &Entry) -> String {
        let Rank::MatchFields(fields) = &row.rank else {
            panic!("expected match fields, got {:?}", row.rank);
        };
        assert_eq!(fields.len(), 1, "one field: the preview");
        fields[0].text.clone()
    }

    #[test]
    fn prefix_is_pipe_and_gated_on_it() {
        let meta = CliphistProvider::new().meta();
        assert_eq!(meta.id, "cliphist");
        assert_eq!(meta.prefixes, vec!["|"]);
        assert!(meta.prefix_only);
    }

    #[test]
    fn selecting_an_entry_decodes_its_original_content() {
        let rows = entries_from_list("1\tcargo test\n", 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entry.id, "cliphist:1");
        assert_eq!(rows[0].entry.title, "cargo test");
        assert_eq!(
            copy_command(&rows[0]),
            vec![
                "cliphist".to_string(),
                "decode".to_string(),
                "1".to_string()
            ]
        );
    }

    /// Matching uses cliphist's preview; selecting sends the id back to
    /// cliphist so the preview's collapsed whitespace and truncation never
    /// reach the clipboard.
    #[test]
    fn rows_carry_a_match_field_of_the_preview() {
        let rows = entries_from_list("1\tcargo build --release\n", 0);
        assert_eq!(match_text(&rows[0]), "cargo build --release");
        assert_eq!(
            copy_command(&rows[0]),
            vec![
                "cliphist".to_string(),
                "decode".to_string(),
                "1".to_string()
            ]
        );
    }

    #[test]
    fn max_entries_caps_the_rows() {
        let list = "1\tone\n2\ttwo\n3\tthree\n";
        assert_eq!(entries_from_list(list, 2).len(), 2);
        assert_eq!(entries_from_list(list, 0).len(), 3);
    }

    #[test]
    fn empty_lines_are_skipped() {
        let rows = entries_from_list("1\t\n2\t\n3\thello\n", 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entry.id, "cliphist:3");
        assert_eq!(rows[0].entry.title, "hello");
    }

    #[test]
    fn the_title_is_cliphist_preview_verbatim() {
        let preview = format!("{}…", "x".repeat(100));
        let rows = entries_from_list(&format!("7\t{preview}\n"), 0);
        assert_eq!(rows[0].entry.title, preview);
        assert_eq!(match_text(&rows[0]), preview);
        assert_eq!(
            copy_command(&rows[0]),
            vec![
                "cliphist".to_string(),
                "decode".to_string(),
                "7".to_string()
            ]
        );
    }

    #[test]
    fn image_entries_decode_the_image_data() {
        let preview = "[[ binary data 2 MiB png 2736x2272 ]]";
        let rows = entries_from_list(&format!("42\t{preview}\n"), 0);
        assert_eq!(rows[0].entry.id, "cliphist:42");
        assert_eq!(rows[0].entry.title, preview);
        assert_eq!(match_text(&rows[0]), preview);
        assert_eq!(
            copy_command(&rows[0]),
            vec![
                "cliphist".to_string(),
                "decode".to_string(),
                "42".to_string()
            ]
        );
    }

    #[test]
    fn extra_config_is_applied_on_init() {
        let mut provider = CliphistProvider::new();
        provider.init(InitContext {
            data_dir: &std::env::temp_dir(),
            extra: Some(serde_json::json!({ "max_entries": 42 })),
        });
        assert_eq!(provider.config.max_entries, 42);
    }
}
