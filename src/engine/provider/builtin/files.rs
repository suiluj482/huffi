//! Browse the filesystem from the launcher.
//!
//! Triggered by the `/` prefix. Because `/` is also the start of every path,
//! the text under the prefix *is* path-shaped. Everything before the last
//! `/` is the **extended prefix** — a path, local to this provider:
//!
//! - `/etc/ho` → extended prefix `/etc` — the current entries are that path
//!   itself and its direct children, and `ho` filters them.
//! - `/etc/`   → extended prefix `/etc` — a trailing `/` switches the
//!   listing with no filter.
//! - `/`       → extended prefix [`root`](FilesConfig::root) itself.
//!
//! Offering the extended prefix itself as an entry is what makes `Enter` open
//! the directory mid-typing (`/etc/ho` → `etc`) while its `Tab` suggestion
//! drops the filter; the children follow from the same rule, so `/etc/hosts`
//! offers `hosts` as a direct child without a special case — the full typed
//! text is always a child of its extended prefix.
//!
//! Every entry is either a directory or a file, and each offers a
//! keeping-prefix [`Tab`](super::super::QuerySuggestion) suggestion: a
//! directory completes to `<path>/` so the listing follows it, a file
//! completes to its whole path. Selecting an entry with `Enter` opens a terminal
//! *in* a directory (the entry's `cwd` makes the shell start there, no inner
//! `cd`), or hands a file to `xdg-open` (`open` on macOS).
//!
//! # Entries from history keys
//!
//! **History entries** are rebuilt from the usage history as well as from the
//! current listing: the store records a launch under *every* prefix of the
//! query, so a path launched at `/etc/ho` is reachable from `ctx.history`
//! for `/etc/` and `/` alike, with no engine-side lookup. On each query the
//! provider takes the keys under the original query and keeps the `files-`
//! namespace — keys from other providers share the same history and are
//! ignored.
//!
//! Current and history paths are merged into one deduplicated list (the
//! current listing wins) before any entry is built, so a path is never offered
//! twice. Each surviving path that still exists on disk becomes an entry — a
//! deleted file in history is stale, not an entry.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
    parse_extra_config,
};
use crate::engine::scoring::MatchField;

/// Trigger prefix: `/etc` lists `/etc`.
const PREFIX: &str = "/";

/// History namespace for filesystem entries: every entry's history key is
/// `files-<path>`, so a path can never collide with a key from another
/// provider.
const HISTORY_PREFIX: &str = "files";

/// Per-provider tuning knobs. When provided via
/// `[engine.provider.builtin.files.extra]`, the fields are parsed from the
/// arbitrary extra config, mirroring [`super::DesktopConfig`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct FilesConfig {
    /// Directory a bare `/` lists, and the base every typed path is relative
    /// to. Defaults to `/`, so the text under the prefix reads as an absolute
    /// path.
    pub root: PathBuf,
    /// Offer a `..` entry that walks to the parent directory.
    pub show_parent: bool,
    /// Include dotfiles in the listing. Defaults to `true`, so `/` is a full
    /// view of a directory.
    pub show_hidden: bool,
    /// Command handing a file entry to the desktop environment's default
    /// app. Defaults to `xdg-open`.
    pub open_command: String,
    /// Fuzzy-match weight of an entry's full path. Defaults above the name
    /// weight: for paths, the whole path is the more distinguishing text.
    pub weight_path: f32,
    /// Fuzzy-match weight of an entry's name.
    pub weight_name: f32,
    /// Rebuild entries from the paths recorded in the usage history.
    pub history_entries: bool,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/"),
            show_parent: true,
            show_hidden: true,
            open_command: "xdg-open".into(),
            weight_path: 1.0,
            weight_name: 0.3,
            history_entries: true,
        }
    }
}

/// Browses the filesystem, one directory per query, with history-driven entries.
///
/// Triggered by the `/` prefix and only queried under it. The listing is a
/// single `read_dir` of the resolved directory; the last directory read is
/// cached so that filtering inside it (typing, then deleting a character)
/// re-filters instead of rescanning.
pub struct FilesProvider {
    config: FilesConfig,
    /// The last directory read and its children, reused for the next query
    /// when the resolved directory is unchanged.
    cache: Option<(PathBuf, Vec<PathBuf>)>,
}

impl Default for FilesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl FilesProvider {
    pub fn new() -> Self {
        Self {
            config: FilesConfig::default(),
            cache: None,
        }
    }

    #[cfg(test)]
    fn with_root(root: impl Into<PathBuf>) -> Self {
        Self {
            config: FilesConfig {
                root: root.into(),
                ..FilesConfig::default()
            },
            cache: None,
        }
    }

    /// The extended prefix of `query`: everything before its last `/`,
    /// read as a path local to this provider and resolved against
    /// [`root`](FilesConfig::root). Nothing before a separator resolves to
    /// `root`; a trailing `/` carries no filter, matching the same rule.
    ///
    /// `/etc/ho` is `/etc`, `/etc/` is `/etc`, `/` and `etc` are `root`.
    fn extended_prefix(&self, query: &str) -> PathBuf {
        let relative = query.strip_prefix('/').unwrap_or(query);
        match relative.rfind('/') {
            Some(i) => self.config.root.join(&relative[..i]),
            None => self.config.root.clone(),
        }
    }

    /// The direct children of `dir`, sorted by name: the cached list when
    /// `dir` is the directory last read, otherwise a fresh scan (also
    /// stored), so filtering inside it re-filters instead of rescanning.
    /// An unreadable directory scans as empty.
    fn children(&mut self, dir: &Path) -> Vec<PathBuf> {
        let stale = !matches!(&self.cache, Some((cached, _)) if cached == dir);
        if stale {
            let mut paths = match std::fs::read_dir(dir) {
                Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
                Err(_) => Vec::new(),
            };
            paths.sort();
            self.cache = Some((dir.to_path_buf(), paths));
        }
        self.cache.as_ref().expect("cache just filled").1.clone()
    }

    /// The current paths behind a query on `dir`: the extended prefix
    /// itself (it is an entry when it exists) followed by its direct
    /// children, with hidden ones dropped when [`show_hidden`](FilesConfig::show_hidden)
    /// is off. Paths — not entries — because dedup runs on plain paths
    /// before any entry is built.
    fn current_paths(&self, dir: &Path, children: &[PathBuf]) -> Vec<PathBuf> {
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(
            children
                .iter()
                .filter(|child| self.config.show_hidden || !title(child).starts_with('.'))
                .cloned(),
        );
        paths
    }

    /// The remembered paths for `ctx`: the entries of the history under the
    /// original query that live in the `files-` namespace, decoded back to
    /// paths in history order. History from other providers shares the same
    /// `ctx.history` and is skipped here.
    fn history_paths(&self, ctx: &QueryContext) -> Vec<PathBuf> {
        if !self.config.history_entries {
            return Vec::new();
        }
        let namespace = format!("{HISTORY_PREFIX}-");
        ctx.history
            .keys()
            .filter_map(|key| Some(PathBuf::from(key.strip_prefix(&namespace)?)))
            .collect()
    }

    /// The entry for a path that exists on disk. The single construction
    /// point every entry goes through — the extended prefix itself, a
    /// listing child, a remembered history path — so an entry's shape is
    /// decided by what the path *is*, not by where it came from. The
    /// directory/file difference is only a branch per diverging field:
    /// - A directory suggests its path *plus a separator* so `Tab` descends
    ///   into it, and `Enter` opens a terminal in it: the
    ///   [`cwd`](EntryBuilder::cwd) is what places the spawn *in* the
    ///   directory; the argv only names a program to run there — the user's
    ///   shell, started bare.
    /// - A file suggests its path as is, and `Enter` hands it to the
    ///   configured [`open_command`](FilesConfig::open_command), spawned in
    ///   the file's directory.
    fn path_entry(&self, path: &Path) -> Option<Entry> {
        let metadata = std::fs::metadata(path).ok()?;
        let is_dir = metadata.is_dir();
        let name = title(path);
        let display = path.display().to_string();
        let relative = relative_to(&self.config.root, path);

        let builder = entry(entry_id(path), name.clone())
            .subtitle(display.clone())
            .icon_name(if is_dir { "folder" } else { file_icon(&name) })
            .set_query_keeping_prefix(if is_dir {
                format!("{relative}/")
            } else {
                relative
            })
            .history_key(format!("{HISTORY_PREFIX}-{}", display));

        let builder = if is_dir {
            builder.terminal_exec(vec![]).cwd(path.to_path_buf())
        } else {
            builder
                .exec(vec![self.config.open_command.clone(), display])
                .cwd(path.parent().unwrap_or(path).to_path_buf())
        };
        Some(builder.match_fields(self.match_fields(&name, path)))
    }

    /// The `..` entry: a directory entry for `parent`, titled `..` and
    /// suggesting the parent directory.
    fn parent_entry(&self, parent: &Path, suggestion: String) -> Entry {
        entry(entry_id(parent), "..")
            .subtitle(parent.display().to_string())
            .icon_name("folder")
            .set_query_keeping_prefix(suggestion)
            .terminal_exec(vec![])
            .cwd(parent.to_path_buf())
            .history_key(format!("{HISTORY_PREFIX}-{}", parent.display()))
            .match_field("..")
    }

    /// The match fields of an entry, the full path weighted above the name
    /// by default: for a path the whole path is the more distinguishing
    /// text.
    fn match_fields(&self, name: &str, path: &Path) -> Vec<MatchField> {
        vec![
            MatchField {
                text: path.display().to_string(),
                weight: self.config.weight_path,
            },
            MatchField {
                text: name.to_string(),
                weight: self.config.weight_name,
            },
        ]
    }
}

/// `path` relative to `root`, or `path` itself when it is not under `root`.
fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|rel| rel.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

/// The entry id for `path`: one shape for both directories and files — an
/// id names a path, and a path is one thing on disk.
fn entry_id(path: &Path) -> String {
    format!("{HISTORY_PREFIX}:{}", path.display())
}

/// The title of an entry for `path`: its basename, or the whole path
/// displayed when `path` has no final component (e.g. `/` or the
/// configured root).
fn title(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Merge path sources into one ordered list with duplicates removed: only
/// the first occurrence of a path is kept, so with the sources passed
/// current-first the live listing wins over remembered history and no path
/// is ever offered twice.
///
/// Deduplicating *paths* before building any entry keeps the decision cheap
/// and the construction single-pass: an already-covered path is never
/// stat'ed, and every surviving path is turned into exactly one entry.
fn dedup_paths(sources: &[&[PathBuf]]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for source in sources {
        for path in *source {
            if seen.insert(path.clone()) {
                paths.push(path.clone());
            }
        }
    }
    paths
}

/// A themed icon name guessed from a file's name. Generic — no MIME lookup,
/// so any file gets a sensible glyph.
fn file_icon(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" => "image-x-generic",
        "mp3" | "flac" | "ogg" | "wav" | "m4a" => "audio-x-generic",
        "mp4" | "mkv" | "webm" | "avi" | "mov" => "video-x-generic",
        "zip" | "tar" | "gz" | "xz" | "bz2" | "zst" | "7z" | "rar" => "package-x-generic",
        "pdf" => "application-pdf",
        "rs" | "go" | "c" | "h" | "cpp" | "hpp" | "py" | "js" | "ts" | "sh" => "text-x-script",
        _ => "text-x-generic",
    }
}

impl Provider for FilesProvider {
    fn meta(&self) -> ProviderMeta {
        // Trigger prefix: `/etc` lists `/etc`.
        ProviderMeta::builder("files")
            .prefix(PREFIX)
            .prefix_only(true)
            .build()
    }

    fn init(&mut self, ctx: InitContext) -> ProviderResult {
        match parse_extra_config::<FilesConfig>(&ctx.extra) {
            Err(result) => return result,
            Ok(Some(config)) => self.config = config,
            Ok(None) => {}
        }
        if !self.config.root.is_dir() {
            return ProviderResult::Other(format!(
                "root {} is not a directory",
                self.config.root.display()
            ));
        }
        ProviderResult::Ok
    }

    fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
        // The extended prefix: everything before the last `/`, read as a
        // path local to this provider. The last component (when present) is
        // only a filter.
        let dir = self.extended_prefix(ctx.query);

        // Current entries: the extended prefix itself and its direct
        // children, read from disk (or the cache).
        let children = self.children(&dir);
        let current = self.current_paths(&dir, &children);

        // History entries: remembered paths from the `files-` namespace,
        // complementing whatever the current listing just produced.
        let history = self.history_paths(&ctx);

        // One dedup pass over the paths — current first, so the listing
        // wins — then a single construction pass: each surviving path that
        // still exists becomes exactly one entry.
        let mut entries: Vec<Entry> = dedup_paths(&[&current, &history])
            .iter()
            .filter_map(|path| self.path_entry(path))
            .collect();

        if self.config.show_parent
            && let Some(parent) = dir.parent()
        {
            entries.push(self.parent_entry(
                parent,
                format!("{}/", relative_to(&self.config.root, parent)),
            ));
        }

        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::{Action, ExecMode, Generation};
    use crate::engine::scoring::Rank;
    use crate::engine::scoring::history::{HistoryStore, QueryHistory};

    fn ctx<'a>(query: &'a str, original: &'a str, history: &'a QueryHistory) -> QueryContext<'a> {
        QueryContext {
            prefix: Some(PREFIX),
            query,
            original,
            history,
            generation: Generation::default(),
        }
    }

    /// A real [`HistoryStore`] with `keys` recorded under the empty query, so
    /// [`history_for`](HistoryStore::history_for) hands back exactly those
    /// keys — the same view a provider sees. This exercises the store's own
    /// key format rather than a hand-built `QueryHistory`.
    fn store_with(keys: impl IntoIterator<Item = String>) -> HistoryStore {
        let mut store = HistoryStore::default();
        for key in keys {
            store.record_launch("", &key);
        }
        store
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("huffi-files-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn provider_at(root: &Path) -> FilesProvider {
        let mut p = FilesProvider::with_root(root);
        assert!(matches!(
            p.init(InitContext {
                data_dir: Path::new("/tmp"),
                extra: None,
            }),
            ProviderResult::Ok
        ));
        p
    }

    fn ids(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(|r| r.entry.id.clone()).collect()
    }

    fn id(path: &Path) -> String {
        format!("files:{}", path.display())
    }

    #[test]
    fn meta_triggers_on_slash_prefix_only() {
        let meta = FilesProvider::new().meta();
        assert_eq!(meta.id, "files");
        assert_eq!(meta.prefixes, vec!["/".to_string()]);
        assert!(meta.prefix_only);
        assert!(meta.enabled);
    }

    /// The extended-prefix split of a query: everything before the last `/`
    /// is a path, the last component is a filter, and a trailing `/` carries
    /// no filter.
    #[test]
    fn query_splits_into_extended_prefix_and_filter() {
        let p = FilesProvider::with_root("/");
        assert_eq!(p.extended_prefix(""), PathBuf::from("/"));
        assert_eq!(p.extended_prefix("et"), PathBuf::from("/"));
        assert_eq!(p.extended_prefix("etc"), PathBuf::from("/"));
        assert_eq!(p.extended_prefix("etc/"), PathBuf::from("/etc"));
        assert_eq!(p.extended_prefix("etc/ho"), PathBuf::from("/etc"));
    }

    #[test]
    fn a_leading_slash_is_ignored_in_the_relative_text() {
        let p = FilesProvider::with_root("/");
        assert_eq!(p.extended_prefix("/etc"), PathBuf::from("/"));
        assert_eq!(p.extended_prefix("/etc/hosts"), PathBuf::from("/etc"));
    }

    /// The current entries are the extended prefix itself and its direct
    /// children: typing `/note.txt` still offers the root to open it plus
    /// the file, and `..` suffixed queries offer the listed directory.
    #[test]
    fn the_extended_prefix_itself_is_a_current_entry() {
        let root = temp_dir("prefix-self");
        std::fs::write(root.join("note.txt"), "hi").unwrap();

        let mut p = provider_at(&root);
        let entries = p.query(ctx("note.txt", "/note.txt", &QueryHistory::EMPTY));
        assert!(
            ids(&entries).contains(&id(&root)),
            "the root itself is an entry of /note.txt"
        );

        std::fs::create_dir(root.join("sub")).unwrap();
        let entries = p.query(ctx("sub/", "/sub/", &QueryHistory::EMPTY));
        assert!(
            ids(&entries).contains(&id(&root.join("sub"))),
            "the listed directory itself is an entry"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The path weighting: an entry's full path outweighs its name by default.
    #[test]
    fn match_fields_weight_the_path_above_the_name() {
        let config = FilesConfig::default();
        assert!(config.weight_path > config.weight_name);

        let root = temp_dir("weighting");
        std::fs::write(root.join("note.txt"), "hi").unwrap();
        let path = root.join("note.txt").display().to_string();
        let mut p = provider_at(&root);
        let entries = p.query(ctx("note.txt", "/note.txt", &QueryHistory::EMPTY));
        let file = entries
            .iter()
            .find(|r| r.entry.title == "note.txt")
            .expect("the file entry");
        let fields = match &file.rank {
            Rank::MatchFields(fields) => fields,
            other => panic!("expected match fields, got {other:?}"),
        };
        assert_eq!(
            fields.iter().find(|f| f.text == path).map(|f| f.weight),
            Some(config.weight_path),
            "the path field keeps its weight"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn directories_and_files_become_entries() {
        let root = temp_dir("listing");
        std::fs::create_dir(root.join("subdir")).unwrap();
        std::fs::write(root.join("note.txt"), "hi").unwrap();

        let mut p = provider_at(&root);
        let entries = p.query(ctx("", "/", &QueryHistory::EMPTY));
        let ids = ids(&entries);
        assert!(ids.contains(&id(&root.join("subdir"))));
        assert!(ids.contains(&id(&root.join("note.txt"))));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn hidden_entries_are_shown_by_default_and_can_be_hidden() {
        let root = temp_dir("hidden");
        std::fs::write(root.join(".dotfile"), "x").unwrap();

        let mut p = provider_at(&root);
        let entries = p.query(ctx("", "/", &QueryHistory::EMPTY));
        assert!(ids(&entries).contains(&id(&root.join(".dotfile"))));

        p.config.show_hidden = false;
        let entries = p.query(ctx("", "/", &QueryHistory::EMPTY));
        assert!(!ids(&entries).contains(&id(&root.join(".dotfile"))));

        std::fs::remove_dir_all(&root).ok();
    }

    /// The path typed in full is an entry of its own, even though the
    /// listing is its parent directory.
    #[test]
    fn the_typed_path_is_an_entry_when_it_exists() {
        let root = temp_dir("typed-path");
        std::fs::create_dir(root.join("sub")).unwrap();
        std::fs::write(root.join("note.txt"), "hi").unwrap();
        std::fs::write(root.join("sub").join("deep.txt"), "hi").unwrap();

        let mut p = provider_at(&root);

        // `note.txt` is both a child of the listing and the typed path: offered
        // exactly once.
        let entries = p.query(ctx("note.txt", "/note.txt", &QueryHistory::EMPTY));
        assert_eq!(
            ids(&entries)
                .iter()
                .filter(|eid| **eid == id(&root.join("note.txt")))
                .count(),
            1,
            "the typed path is not duplicated with the listing entry"
        );

        // `sub/deep.txt` names a file in a subdirectory, so the listing of
        // `sub` would not otherwise offer it as the typed path does.
        let entries = p.query(ctx("sub/deep.txt", "/sub/deep.txt", &QueryHistory::EMPTY));
        let entry = entries
            .iter()
            .find(|r| r.entry.id == id(&root.join("sub").join("deep.txt")))
            .expect("the typed file path is an entry");
        match &entry.entry.action {
            Action::Exec { mode, cwd, .. } => {
                assert_eq!(*mode, ExecMode::Direct);
                assert_eq!(cwd.as_deref(), Some(root.join("sub").as_path()));
            }
            other => panic!("expected direct Exec, got {other:?}"),
        }
        let suggestion = entry.entry.set_query.as_ref().expect("a suggestion");
        assert_eq!(suggestion.resolve(Some(PREFIX)), "/sub/deep.txt");

        // A directory typed in full is an entry too, opening a terminal in it.
        let entries = p.query(ctx("sub", "/sub", &QueryHistory::EMPTY));
        let dir_entry = entries
            .iter()
            .find(|r| r.entry.id == id(&root.join("sub")))
            .expect("the typed directory path is an entry");
        match &dir_entry.entry.action {
            Action::Exec { mode, cwd, .. } => {
                assert_eq!(*mode, ExecMode::Terminal);
                assert_eq!(cwd.as_deref(), Some(root.join("sub").as_path()));
            }
            other => panic!("expected terminal Exec, got {other:?}"),
        }

        // A path that does not exist is not an entry.
        let entries = p.query(ctx("nope.txt", "/nope.txt", &QueryHistory::EMPTY));
        assert!(!ids(&entries).contains(&id(&root.join("nope.txt"))));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_parent_entry_walks_up_unless_disabled() {
        let root = temp_dir("parent");
        std::fs::create_dir(root.join("sub")).unwrap();

        let mut p = provider_at(&root);
        let entries = p.query(ctx("sub/", "/sub/", &QueryHistory::EMPTY));
        let parent = entries
            .iter()
            .find(|r| r.entry.title == "..")
            .expect("a parent entry");
        assert_eq!(parent.entry.set_query.as_ref().unwrap().query, "/");
        match &parent.entry.action {
            Action::Exec { cwd, mode, .. } => {
                assert_eq!(cwd.as_deref(), Some(root.as_path()));
                assert_eq!(*mode, ExecMode::Terminal);
            }
            other => panic!("expected terminal exec, got {other:?}"),
        }

        p.config.show_parent = false;
        let entries = p.query(ctx("sub/", "/sub/", &QueryHistory::EMPTY));
        assert!(!entries.iter().any(|r| r.entry.title == ".."));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn file_entries_open_the_file_and_directories_open_a_terminal() {
        let root = temp_dir("actions");
        std::fs::create_dir(root.join("subdir")).unwrap();
        std::fs::write(root.join("note.txt"), "hi").unwrap();

        let mut p = provider_at(&root);
        let entries = p.query(ctx("", "/", &QueryHistory::EMPTY));

        let file = entries
            .iter()
            .find(|r| r.entry.title == "note.txt")
            .expect("the file entry");
        match &file.entry.action {
            Action::Exec { args, mode, cwd } => {
                assert_eq!(*mode, ExecMode::Direct);
                assert_eq!(
                    args.last().unwrap(),
                    &root.join("note.txt").display().to_string()
                );
                assert_eq!(cwd.as_deref(), Some(root.as_path()));
            }
            other => panic!("expected exec, got {other:?}"),
        }

        let dir = entries
            .iter()
            .find(|r| r.entry.title == "subdir")
            .expect("the directory entry");
        match &dir.entry.action {
            Action::Exec { mode, cwd, .. } => {
                assert_eq!(*mode, ExecMode::Terminal);
                assert_eq!(cwd.as_deref(), Some(root.join("subdir").as_path()));
            }
            other => panic!("expected terminal exec, got {other:?}"),
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn every_entry_keys_history_under_the_files_namespace() {
        let root = temp_dir("history-key");
        std::fs::write(root.join("note.txt"), "hi").unwrap();

        let mut p = provider_at(&root);
        for entry in p.query(ctx("", "/", &QueryHistory::EMPTY)) {
            let key = entry.history_key.expect("a history key");
            assert!(key.starts_with("files-"), "{key} must be namespaced");
            assert!(matches!(entry.rank, Rank::MatchFields(_)));
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn entries_offer_keeping_prefix_suggestions() {
        let root = temp_dir("suggest");
        std::fs::create_dir(root.join("subdir")).unwrap();
        std::fs::write(root.join("note.txt"), "hi").unwrap();

        let mut p = provider_at(&root);
        let entries = p.query(ctx("", "/", &QueryHistory::EMPTY));

        let dir = entries.iter().find(|r| r.entry.title == "subdir").unwrap();
        let s = dir.entry.set_query.clone().expect("a suggestion");
        assert!(s.keep_prefix);
        assert_eq!(s.resolve(Some(PREFIX)), "/subdir/");

        let file = entries
            .iter()
            .find(|r| r.entry.title == "note.txt")
            .unwrap();
        let s = file.entry.set_query.clone().expect("a suggestion");
        assert!(s.keep_prefix);
        assert_eq!(s.resolve(Some(PREFIX)), "/note.txt");

        std::fs::remove_dir_all(&root).ok();
    }

    /// A remembered path outside the current listing comes back as an entry; a
    /// path already in the listing is deduped; a vanished path is dropped.
    #[test]
    fn history_keys_reconstruct_entries() {
        let root = temp_dir("reconstruct");
        let remembered = root.join("deep");
        std::fs::create_dir_all(&remembered).unwrap();
        std::fs::write(remembered.join("old.txt"), "x").unwrap();
        std::fs::write(root.join("listed.txt"), "x").unwrap();

        let store = store_with([
            format!("{HISTORY_PREFIX}-{}", remembered.join("old.txt").display()),
            format!("{HISTORY_PREFIX}-{}", root.join("listed.txt").display()),
            format!("{HISTORY_PREFIX}-{}", root.join("gone.txt").display()),
        ]);
        let history = store.history_for("");

        let mut p = provider_at(&root);
        let entries = p.query(ctx("", "/", &history));
        let entry_ids = ids(&entries);
        assert!(entry_ids.contains(&id(&remembered.join("old.txt"))));
        assert!(
            !entry_ids.contains(&id(&root.join("gone.txt"))),
            "a vanished path is not an entry"
        );
        assert_eq!(
            entry_ids
                .iter()
                .filter(|recur| **recur == id(&root.join("listed.txt")))
                .count(),
            1,
            "a path in the listing is not duplicated by history"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Keys from other providers share the same query history and must be
    /// ignored: only the `files-` namespace is reconstructed.
    #[test]
    fn keys_from_other_providers_are_ignored() {
        let root = temp_dir("foreign-keys");

        let store = store_with([
            "desktop-firefox.desktop".to_string(),
            "runner-git status".to_string(),
        ]);
        let history = store.history_for("");

        let mut p = provider_at(&root);
        let entries = p.query(ctx("", "/", &history));
        assert!(entries.iter().all(|r| r.entry.title != "firefox.desktop"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn history_entries_can_be_turned_off() {
        let root = temp_dir("no-history");
        let remembered = root.join("deep");
        std::fs::create_dir_all(&remembered).unwrap();
        std::fs::write(remembered.join("old.txt"), "x").unwrap();

        let store = store_with([format!(
            "{HISTORY_PREFIX}-{}",
            remembered.join("old.txt").display()
        )]);
        let history = store.history_for("");

        let mut p = provider_at(&root);
        p.config.history_entries = false;
        let entries = p.query(ctx("", "/", &history));
        assert!(!ids(&entries).contains(&id(&remembered.join("old.txt"))));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_invalid_root_is_rejected_at_init() {
        let mut p = FilesProvider::with_root("/definitely/not/a/dir");
        assert!(matches!(
            p.init(InitContext {
                data_dir: Path::new("/tmp"),
                extra: None,
            }),
            ProviderResult::Other(_)
        ));
    }

    #[test]
    fn extra_config_applies_on_init() {
        let root = temp_dir("extra");
        let mut p = FilesProvider::new();
        assert!(matches!(
            p.init(InitContext {
                data_dir: Path::new("/tmp"),
                extra: Some(serde_json::json!({
                    "root": root,
                    "show_hidden": false,
                    "show_parent": false,
                    "weight_path": 0.5,
                })),
            }),
            ProviderResult::Ok
        ));
        assert_eq!(p.config.root, root);
        assert!(!p.config.show_hidden);
        assert!(!p.config.show_parent);
        assert_eq!(p.config.weight_path, 0.5);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn invalid_extra_config_is_non_critical() {
        let mut p = FilesProvider::new();
        assert!(matches!(
            p.init(InitContext {
                data_dir: Path::new("/tmp"),
                extra: Some(serde_json::json!({ "show_hidden": "yes please" })),
            }),
            ProviderResult::Config {
                critical: false,
                ..
            }
        ));
    }
}
