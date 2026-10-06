use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
    parse_extra_config,
};

/// Per-provider tuning knobs. When provided via
/// `[engine.provider.builtin.runner.extra]`, the fields are parsed from the
/// arbitrary extra config, mirroring [`super::NixConfig`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct RunnerConfig {
    /// Shell history files to load, in order. `None` (the default) uses
    /// `$HISTFILE`, `~/.bash_history` and `~/.zsh_history`, whichever exist.
    /// Later files win when the same command appears in several of them.
    pub history_files: Option<Vec<PathBuf>>,
    /// Keep only the `max_history_lines` most recent distinct commands.
    /// `0` means unlimited. The cap is applied once at init, never per
    /// query, so the engine's history blending always sees the full
    /// returned corpus.
    pub max_history_lines: usize,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            history_files: None,
            max_history_lines: 5000,
        }
    }
}

/// Runs the query as a shell command in the terminal, suggested from the
/// user's shell history.
///
/// Triggered by the `>` prefix: `> cargo test` offers one entry that runs
/// `cargo test`, plus every shell-history command as a fuzzy suggestion.
/// The run-as-typed entry carries a static score of `1.0`; the suggestions
/// carry the command as their single match field, so the engine does all
/// fuzzy matching, normalization and history blending — this provider never
/// scores anything itself.
///
/// The history corpus is loaded once in [`init`](Provider::init), capped to
/// the newest [`RunnerConfig::max_history_lines`] entries and built into
/// entries right there, so [`query`](Provider::query) only clones and
/// filters it; an empty query returns the corpus alone (newest first), a
/// non-empty query adds the run-as-typed entry.
pub struct RunnerProvider {
    config: RunnerConfig,
    /// Program used as `shell -c <command>`; the user's `$SHELL`.
    shell: String,
    /// The history corpus as ready-to-serve entries, newest first, built
    /// once in [`init`](Provider::init).
    corpus: Arc<[Entry]>,
}

impl Default for RunnerProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerProvider {
    pub fn new() -> Self {
        Self {
            config: RunnerConfig::default(),
            shell: user_shell(),
            corpus: Arc::from([]),
        }
    }

    #[cfg(test)]
    fn with_commands(commands: Vec<String>) -> Self {
        Self {
            config: RunnerConfig::default(),
            shell: "sh".into(),
            corpus: build_corpus(&commands, "sh"),
        }
    }
}

impl Provider for RunnerProvider {
    fn meta(&self) -> ProviderMeta {
        // Trigger prefix: `>cargo test` runs `cargo test` in the terminal.
        ProviderMeta::builder("runner")
            .prefix(">")
            .prefix_only(true)
            .build()
    }

    fn init(&mut self, ctx: InitContext) -> ProviderResult {
        match parse_extra_config::<RunnerConfig>(&ctx.extra) {
            Err(result) => return result,
            Ok(Some(config)) => self.config = config,
            Ok(None) => {}
        }

        let files = self
            .config
            .history_files
            .clone()
            .unwrap_or_else(default_history_files);
        let commands = load_history(&files, self.config.max_history_lines);
        self.corpus = build_corpus(&commands, &self.shell);

        ProviderResult::Ok
    }

    fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
        let query = ctx.query.trim();
        let mut entries: Vec<Entry> = self
            .corpus
            .iter()
            .filter(|e| e.entry.title != query)
            .cloned()
            .collect();
        if !query.is_empty() {
            entries.insert(0, run_entry(query, &self.shell));
        }
        entries
    }
}

/// Build the history corpus into entries once, so the keystroke path only
/// clones them.
fn build_corpus(commands: &[String], shell: &str) -> Arc<[Entry]> {
    commands
        .iter()
        .map(|cmd| history_entry(cmd, shell))
        .collect()
}

/// The entry that runs exactly what was typed, first in the result list.
fn run_entry(command: &str, shell: &str) -> Entry {
    entry(format!("run:{command}"), command)
        .icon_name("utilities-terminal")
        .terminal_hold(shell_argv(shell, command))
        .history_key(command)
        .score(1.0)
}

/// A shell-history command offered as a fuzzy suggestion.
fn history_entry(command: &str, shell: &str) -> Entry {
    entry(format!("hist:{command}"), command)
        .icon_name("utilities-terminal")
        .terminal_hold(shell_argv(shell, command))
        .history_key(command)
        .match_field(command)
}

fn shell_argv(shell: &str, command: &str) -> Vec<String> {
    vec![shell.to_string(), "-c".into(), command.to_string()]
}

/// The login shell to run commands with, so a command taken from zsh
/// history runs under zsh rather than a POSIX `sh`.
fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "sh".into())
}

/// History files to read when the config does not name any: `$HISTFILE`
/// first (whatever shell is actually in use), then the common defaults.
fn default_history_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(histfile) = std::env::var("HISTFILE")
        && !histfile.is_empty()
    {
        files.push(PathBuf::from(histfile));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        files.push(home.join(".bash_history"));
        files.push(home.join(".zsh_history"));
    }
    files
}

/// Read the given history files, strip per-line metadata, dedupe keeping the
/// most recent occurrence, cap to the newest `max` commands (0 = unlimited)
/// and return them newest first. Missing files are skipped.
fn load_history(files: &[PathBuf], max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in content.lines() {
            let line = strip_history_metadata(line).trim();
            if !line.is_empty() {
                lines.push(line.to_owned());
            }
        }
    }

    // Walk backwards so the first time a command is seen is its most recent
    // occurrence; that pass already yields newest-first order.
    let mut seen = HashSet::new();
    let mut commands: Vec<String> = lines
        .into_iter()
        .rev()
        .filter(|cmd| seen.insert(cmd.clone()))
        .collect();
    if max > 0 {
        commands.truncate(max);
    }
    commands
}

/// Remove shell-history metadata from a raw line: a bash `#<epoch>`
/// timestamp line (the whole line is metadata, so it is dropped) and the
/// zsh extended-format `: <epoch>:<duration>;<command>` prefix.
fn strip_history_metadata(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('#')
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return "";
    }

    if let Some(rest) = line.strip_prefix(": ")
        && let Some(semi) = rest.find(';')
        && is_zsh_meta(&rest[..semi])
    {
        return &rest[semi + 1..];
    }

    line
}

/// Whether the text before a candidate `;` is zsh's `<epoch>:<duration>`.
fn is_zsh_meta(meta: &str) -> bool {
    let mut parts = meta.split(':');
    let valid =
        |s: Option<&str>| s.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
    valid(parts.next()) && valid(parts.next()) && parts.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::{Action, ExecMode, HandleContext, QueryContext};
    use crate::engine::scoring::Rank;

    fn query_ctx(query: &str) -> QueryContext<'_> {
        QueryContext {
            prefix: Some(">"),
            query,
            original: query,
        }
    }

    fn temp_history(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("huffi-runner-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn meta_triggers_on_greater_than_prefix_only() {
        let meta = RunnerProvider::new().meta();
        assert_eq!(meta.id, "runner");
        assert_eq!(meta.prefixes, vec![">".to_string()]);
        assert!(meta.prefix_only);
        assert!(meta.enabled);
    }

    #[test]
    fn strips_bash_timestamp_line() {
        assert_eq!(strip_history_metadata("#1699999999"), "");
        assert_eq!(
            strip_history_metadata("#1699999999 git status"),
            "#1699999999 git status"
        );
        assert_eq!(
            strip_history_metadata("# not a timestamp"),
            "# not a timestamp"
        );
    }

    #[test]
    fn strips_zsh_extended_prefix_only_for_meta_lines() {
        assert_eq!(
            strip_history_metadata(": 1699999999:0;git status"),
            "git status"
        );
        assert_eq!(strip_history_metadata(": 1699999999:12;ls -la"), "ls -la");
        // A command that merely starts with ": " keeps its text.
        assert_eq!(strip_history_metadata(": echo hi"), ": echo hi");
        assert_eq!(strip_history_metadata(": ab:c;d"), ": ab:c;d");
    }

    #[test]
    fn load_history_dedupes_keeping_most_recent() {
        let file = temp_history("dedupe", "ls\ngit status\nls\n");
        let commands = load_history(std::slice::from_ref(&file), 0);
        assert_eq!(commands, vec!["ls".to_string(), "git status".to_string()]);
    }

    #[test]
    fn load_history_keeps_newest_first_and_caps() {
        let file = temp_history("cap", "a\nb\nc\nd\n");
        let commands = load_history(std::slice::from_ref(&file), 2);
        assert_eq!(commands, vec!["d".to_string(), "c".to_string()]);
    }

    #[test]
    fn load_history_zero_means_unlimited() {
        let file = temp_history("unlimited", "a\nb\nc\n");
        let commands = load_history(std::slice::from_ref(&file), 0);
        assert_eq!(commands.len(), 3);
    }

    #[test]
    fn load_history_skips_missing_files_and_blank_lines() {
        let missing = PathBuf::from("/nonexistent/huffi-history");
        let file = temp_history("blanks", "\nls\n\n  \ngit status\n");
        let commands = load_history(&[missing, file], 0);
        assert_eq!(commands, vec!["git status".to_string(), "ls".to_string()]);
    }

    #[test]
    fn load_history_prefers_later_files_on_duplicates() {
        let first = temp_history("first", "old\nshared\n");
        let second = temp_history("second", "shared\nnew\n");
        let commands = load_history(&[first, second], 0);
        assert_eq!(
            commands,
            vec!["new".to_string(), "shared".to_string(), "old".to_string()]
        );
    }

    #[test]
    fn empty_query_returns_only_the_corpus_newest_first() {
        let mut p = RunnerProvider::with_commands(vec!["ls".into(), "git status".into()]);
        let entries = p.query(query_ctx(""));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entry.title, "ls");
        assert_eq!(entries[1].entry.title, "git status");
        assert!(matches!(entries[0].rank, Rank::MatchFields(_)));
    }

    #[test]
    fn non_empty_query_prepends_the_run_entry() {
        let mut p = RunnerProvider::with_commands(vec!["git status".into(), "ls".into()]);
        let entries = p.query(query_ctx("git s"));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].entry.title, "git s");
        assert_eq!(entries[0].entry.id, "run:git s");
        assert!(matches!(entries[0].rank, Rank::Score(s) if s == 1.0));
        assert_eq!(entries[0].history_key.as_deref(), Some("git s"));
        assert_eq!(entries[1].entry.id, "hist:git status");
    }

    #[test]
    fn corpus_entry_matching_the_query_exactly_is_skipped() {
        let mut p = RunnerProvider::with_commands(vec!["git status".into(), "ls".into()]);
        let entries = p.query(query_ctx("git status"));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].entry.id, "run:git status");
        assert_eq!(entries[1].entry.id, "hist:ls");
    }

    #[test]
    fn query_is_trimmed_before_matching_and_running() {
        let mut p = RunnerProvider::with_commands(vec!["ls".into()]);
        let entries = p.query(query_ctx("  "));
        assert_eq!(entries.len(), 1, "blank query is an empty query");

        let entries = p.query(query_ctx("  ls  "));
        assert_eq!(
            entries.len(),
            1,
            "corpus copy of the exact query is skipped"
        );
        assert_eq!(entries[0].entry.id, "run:ls");
        assert_eq!(entries[0].history_key.as_deref(), Some("ls"));
    }

    #[test]
    fn entries_run_the_command_in_the_terminal_via_the_shell() {
        let mut p = RunnerProvider::with_commands(vec!["git status".into()]);
        let entries = p.query(query_ctx("git s"));
        for e in &entries {
            match &e.entry.action {
                Action::Exec { args, mode, .. } => {
                    assert_eq!(*mode, ExecMode::TerminalHold);
                    assert_eq!(
                        args,
                        &vec!["sh".to_string(), "-c".to_string(), e.entry.title.clone()]
                    );
                }
                other => panic!("expected TerminalHold exec, got {other:?}"),
            }
            assert_eq!(e.history_key.as_deref(), Some(e.entry.title.as_str()));
        }
    }

    #[test]
    fn extra_config_parses_history_files_and_cap() {
        let extra = serde_json::json!({
            "history_files": ["/home/user/.zsh_history"],
            "max_history_lines": 42
        });
        let config: RunnerConfig = serde_json::from_value(extra).unwrap();
        assert_eq!(
            config.history_files,
            Some(vec![PathBuf::from("/home/user/.zsh_history")])
        );
        assert_eq!(config.max_history_lines, 42);
    }

    #[test]
    fn extra_config_defaults_apply_to_partial_input() {
        let config: RunnerConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(config, RunnerConfig::default());
        assert_eq!(config.max_history_lines, 5000);
        assert_eq!(config.history_files, None);
    }

    #[test]
    fn init_reads_configured_history_files() {
        let file = temp_history("init", "ls\ngit status\n");
        let mut p = RunnerProvider::new();
        let extra = serde_json::json!({ "history_files": [file] });
        let result = p.init(InitContext {
            data_dir: std::path::Path::new("/tmp"),
            extra: Some(extra),
        });
        assert!(matches!(result, ProviderResult::Ok));
        let titles: Vec<&str> = p.corpus.iter().map(|e| e.entry.title.as_str()).collect();
        assert_eq!(titles, vec!["git status", "ls"]);
    }

    #[test]
    fn invalid_extra_config_is_non_critical() {
        let mut p = RunnerProvider::new();
        let extra = serde_json::json!({ "max_history_lines": "lots" });
        let result = p.init(InitContext {
            data_dir: std::path::Path::new("/tmp"),
            extra: Some(extra),
        });
        assert!(matches!(
            result,
            ProviderResult::Config {
                critical: false,
                ..
            }
        ));
    }

    #[test]
    fn handle_is_a_noop() {
        let mut p = RunnerProvider::new();
        p.handle(HandleContext {
            entry_id: "run:ls",
            query: query_ctx("ls"),
        });
    }

    /// Profiling harness (run: `cargo test --release profile_runner --
    /// --ignored --nocapture`). Compares the per-keystroke cost of cloning
    /// the prebuilt corpus against building the entries from raw strings on
    /// every query, which is what `query()` did before the corpus was
    /// cached.
    #[test]
    #[ignore]
    fn profile_query_cost() {
        use std::time::Instant;

        let commands: Vec<String> = (0..5000)
            .map(|i| format!("git -C ~/src/repo-{i} status --porcelain branch-{i}"))
            .collect();
        let mut p = RunnerProvider::with_commands(commands.clone());

        let iterations = 200;
        for needle in ["", "git", "git stat", "repo-42"] {
            let t = Instant::now();
            for _ in 0..iterations {
                std::hint::black_box(p.query(query_ctx(needle)));
            }
            let prebuilt = t.elapsed() / iterations;

            let t = Instant::now();
            for _ in 0..iterations {
                let mut entries: Vec<Entry> = commands
                    .iter()
                    .filter(|c| c.as_str() != needle)
                    .map(|c| history_entry(c, "sh"))
                    .collect();
                if !needle.is_empty() {
                    entries.insert(0, run_entry(needle, "sh"));
                }
                std::hint::black_box(entries);
            }
            let built = t.elapsed() / iterations;

            eprintln!(
                "[prof] query {needle:?}: prebuilt {prebuilt:?} vs build-per-query {built:?}"
            );
        }
    }
}
