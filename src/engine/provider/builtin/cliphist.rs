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
/// Clipboard history changes frequently, so entries are fetched fresh on each
/// query rather than cached at init.
pub struct CliphistProvider {
    config: CliphistConfig,
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

    fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
        load_cliphist(self.config.max_entries)
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
        // cliphist list format: "<id>\t<content>"
        let mut parts = line.splitn(2, '\t');
        let _id = parts.next().unwrap_or("");
        let content = parts.next().unwrap_or("").trim_end_matches('\r');
        if content.is_empty() {
            continue;
        }
        // Create a short title - truncate to reasonable length
        let title = if content.len() > 100 {
            let mut end = 100;
            // Try to cut at word boundary
            while end > 50 && !content.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &content[..end])
        } else {
            content.to_string()
        };

        // The full content is both what gets copied and what the engine
        // fuzzy-matches; the title is only a truncation of it for display.
        let entry = entry(format!("cliphist:{}", count), title)
            .icon_name("edit-paste-symbolic")
            .clipboard(content)
            .match_field(content);

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

    fn copy_value(row: &Entry) -> String {
        let Action::Clipboard { value } = &row.entry.action else {
            panic!("expected a clipboard action, got {:?}", row.entry.action);
        };
        value.clone()
    }

    fn match_text(row: &Entry) -> String {
        let Rank::MatchFields(fields) = &row.rank else {
            panic!("expected match fields, got {:?}", row.rank);
        };
        assert_eq!(fields.len(), 1, "one field: the full content");
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
    fn clips_copy_their_full_content() {
        let rows = entries_from_list("1\tcargo test\n", 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entry.title, "cargo test");
        assert_eq!(copy_value(&rows[0]), "cargo test");
    }

    /// The engine fuzzy-matches the *full* content, not the truncated title,
    /// so a long pasted command is still reachable by its tail.
    #[test]
    fn rows_carry_a_match_field_of_the_full_content() {
        let rows = entries_from_list("1\tcargo build --release\n", 0);
        assert_eq!(match_text(&rows[0]), "cargo build --release");
        assert_eq!(copy_value(&rows[0]), "cargo build --release");
    }

    #[test]
    fn max_entries_caps_the_rows() {
        let list = "1\tone\n2\ttwo\n3\tthree\n";
        assert_eq!(entries_from_list(list, 2).len(), 2);
        assert_eq!(entries_from_list(list, 0).len(), 3);
    }

    #[test]
    fn empty_lines_are_skipped() {
        let rows = entries_from_list("1\t\n\t\n\thello\n", 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entry.title, "hello");
    }

    #[test]
    fn a_long_content_truncates_only_the_title() {
        let long = "x".repeat(200);
        let rows = entries_from_list(&format!("1\t{long}\n"), 0);
        assert!(rows[0].entry.title.ends_with("..."));
        assert!(rows[0].entry.title.len() < 200);
        // Matched and copied text stay whole.
        assert_eq!(match_text(&rows[0]), long);
        assert_eq!(copy_value(&rows[0]), long);
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
