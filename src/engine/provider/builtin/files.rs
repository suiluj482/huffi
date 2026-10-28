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
//! `cd`), or hands a file to `xdg-open`.
//!
//! # Open-with variants
//!
//! The configured [`suffix`](FilesConfig::suffix) (default `@`) extends the
//! listing rather than replacing it: `/etc/ho @code` lists `/etc` filtered to
//! `ho` and gives every listed path — and, per
//! [`targets`](Opener::targets), those of its openers that apply — an extra
//! variant that opens it with that [`Opener`]. Variants carry the
//! opener id in their id (`files:/etc/hosts@code`), history key, suggestion,
//! and both match fields, so the engine's fuzzy matching filters them by the
//! suffix on its own: plain entries no longer match once `@` is typed.
//!
//! Variants for current paths are only generated while the suffix is typed,
//! because their suffixed fields would otherwise also match plain queries
//! and flood the listing. History variants always show: launching
//! `/etc/hosts@code` records `files-/etc/hosts@code` under every prefix of
//! the query, so `/etc/ho` later offers the same opener one keystroke away.
//!
//! # Entries from history keys
//!
//! **History entries** are rebuilt from the usage history as well as from the
//! current listing: the store records a launch under *every* prefix of the
//! query, so a path launched at `/etc/ho` is reachable from `ctx.history`
//! for `/etc/` and `/` alike, with no engine-side lookup. On each query the
//! provider takes the keys under the original query and keeps the `files-`
//! namespace — keys from other providers share the same history and are
//! ignored. A key's suffix is resolved against the configured openers; an
//! unknown one stays path text, so paths that contain the suffix symbol keep
//! working.
//!
//! Current and history targets — a path, optionally paired with an opener —
//! are merged into one deduplicated list (the current listing wins) before
//! any entry is built, so nothing is offered twice. Each surviving target
//! that still exists on disk becomes an entry; a deleted file in history is
//! stale, not an entry.

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

/// [`Opener::targets`] token selecting directories.
const DIRECTORY_TARGET: &str = "inode/directory";

/// [`Opener::targets`] token selecting every file.
const FILE_TARGET: &str = "*";

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
    /// Symbol that separates a path from an opener suffix: `/etc/ho @code`
    /// lists `/etc` filtered to `ho` and offers each entry's `code` variant.
    /// Defaults to `@`; an empty string turns opener variants off.
    pub suffix: String,
    /// Extra programs every listed path can be opened with. Each yields a
    /// suffixed variant of the path's entry, usable while the suffix is
    /// typed.
    pub openers: Vec<Opener>,
    /// Fuzzy-match weight of an entry's full path. Defaults above the name
    /// weight: for paths, the whole path is the more distinguishing text.
    pub weight_path: f32,
    /// Fuzzy-match weight of an entry's name.
    pub weight_name: f32,
    /// Rebuild entries from the paths recorded in the usage history.
    pub history_entries: bool,
}

/// A configured "open with" program for [`FilesConfig::openers`].
///
/// The opener's [`id`](Self::id) is what appears after the suffix in the
/// query, entry id, and history key, so it should be stable and short.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct Opener {
    /// Suffix text and identity of this opener. Defaults to the basename of
    /// the first command item (`/usr/bin/code` → `code`).
    pub name: String,
    /// Program and arguments; the target path is appended as the last
    /// argument.
    pub command: Vec<String>,
    /// What this opener applies to: `inode/directory` matches directories,
    /// `*` every file, and any other token without a `/` is a file extension
    /// (case-insensitive, leading dot optional). Empty means every file.
    /// Tokens containing a `/` other than `inode/directory` are reserved for
    /// MIME matching and never match yet.
    pub targets: Vec<String>,
}

impl Opener {
    /// The opener's identity: [`name`](Self::name), or the basename of the
    /// program when no name is configured.
    fn id(&self) -> &str {
        if !self.name.is_empty() {
            return &self.name;
        }
        self.command
            .first()
            .map(|program| program.rsplit('/').next().unwrap_or(program))
            .unwrap_or_default()
    }

    /// Whether this opener applies to `path`, given whether it is a
    /// directory. An empty target list means every file; the directory token
    /// only ever matches directories, `*` only files, and extension tokens
    /// only files carrying that extension.
    fn applies_to(&self, path: &Path, is_dir: bool) -> bool {
        if self.targets.is_empty() {
            return !is_dir;
        }
        self.targets.iter().any(|target| match target.as_str() {
            DIRECTORY_TARGET => is_dir,
            FILE_TARGET => !is_dir,
            other if other.contains('/') => false,
            extension => {
                !is_dir
                    && path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|actual| {
                            extension
                                .trim_start_matches('.')
                                .eq_ignore_ascii_case(actual)
                        })
            }
        })
    }
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/"),
            show_parent: true,
            show_hidden: true,
            open_command: "xdg-open".into(),
            suffix: "@".into(),
            openers: Vec::new(),
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

    /// The configured opener suffix symbol; empty when variants are off.
    fn suffix_symbol(&self) -> &str {
        &self.config.suffix
    }

    /// The suffix appended for `opener`: the configured symbol plus its id,
    /// e.g. `@code`.
    fn suffix(&self, opener: &str) -> String {
        format!("{}{opener}", self.suffix_symbol())
    }

    /// `text` with `opener`'s suffix appended when an opener is given,
    /// unchanged otherwise. The conditional suffix pattern lives here so
    /// entry ids, history keys, and `Tab` suggestions read as one expression.
    fn suffixed(&self, text: impl Into<String>, opener: Option<&str>) -> String {
        let mut text = text.into();
        if let Some(opener) = opener {
            text.push_str(&self.suffix(opener));
        }
        text
    }

    /// Split `query` into the listing query and the opener suffix at the
    /// last configured suffix symbol. `/etc/ho @code` lists from `/etc/ho `
    /// and suffixes with `code`. With the symbol disabled or absent there is
    /// no suffix and `query` is returned unchanged.
    fn split_suffix<'q>(&self, query: &'q str) -> (&'q str, Option<&'q str>) {
        let symbol = self.suffix_symbol();
        if symbol.is_empty() {
            return (query, None);
        }
        match query.rsplit_once(symbol) {
            Some((query, suffix)) => (query, Some(suffix)),
            None => (query, None),
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
    /// is off. Paths — not entries — because dedup runs on targets before
    /// any entry is built.
    fn current_paths(&mut self, dir: &Path) -> Vec<PathBuf> {
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(
            self.children(dir)
                .iter()
                .filter(|child| self.config.show_hidden || !title(child).starts_with('.'))
                .cloned(),
        );
        paths
    }

    /// The current targets behind a query: every current path as itself,
    /// plus — when the query carries a suffix — one variant per applicable
    /// opener for that path. Variants are only generated once the suffix is
    /// typed because their suffixed match fields would otherwise also match
    /// plain queries and flood the listing.
    fn current_targets(&mut self, query: &str, with_openers: bool) -> Vec<Target> {
        let dir = self.extended_prefix(query);
        let mut targets = Vec::new();
        for path in self.current_paths(&dir) {
            let is_dir = std::fs::metadata(&path).ok().map(|meta| meta.is_dir());
            targets.push(Target {
                path: path.clone(),
                opener: None,
            });
            let Some(is_dir) = is_dir else { continue };
            if !with_openers {
                continue;
            }
            for (index, opener) in self.config.openers.iter().enumerate() {
                if opener.applies_to(&path, is_dir) {
                    targets.push(Target {
                        path: path.clone(),
                        opener: Some(index),
                    });
                }
            }
        }
        targets
    }

    /// The remembered targets for `ctx`: the entries of the history under
    /// the original query that live in the `files-` namespace, decoded back
    /// to path + opener pairs in history order. A suffix that does not name
    /// a configured opener is left as path text, so paths containing the
    /// symbol keep working. History from other providers shares the same
    /// `ctx.history` and is skipped here.
    fn history_targets(&self, ctx: &QueryContext) -> Vec<Target> {
        if !self.config.history_entries {
            return Vec::new();
        }
        let namespace = format!("{HISTORY_PREFIX}-");
        let symbol = self.suffix_symbol();
        ctx.history
            .keys()
            .filter_map(|key| {
                let rest = key.strip_prefix(&namespace)?;
                if !symbol.is_empty()
                    && let Some((path, id)) = rest.rsplit_once(symbol)
                    && let Some(index) = self
                        .config
                        .openers
                        .iter()
                        .position(|opener| opener.id() == id)
                {
                    return Some(Target {
                        path: PathBuf::from(path),
                        opener: Some(index),
                    });
                }
                Some(Target {
                    path: PathBuf::from(rest),
                    opener: None,
                })
            })
            .collect()
    }

    /// The entry for a target that exists on disk. The single construction
    /// point every entry goes through — the extended prefix itself, a
    /// listing child, a remembered history path — so an entry's shape is
    /// decided by what the path *is* (and, with an opener, how it is opened),
    /// not by where it came from.
    ///
    /// Without an opener the directory/file difference is only a branch per
    /// diverging field: a directory suggests its path *plus a separator* so
    /// `Tab` descends into it and `Enter` opens a terminal in it (the
    /// [`cwd`](EntryBuilder::cwd) places the spawn *in* the directory); a
    /// file suggests its path as is and `Enter` hands it to the configured
    /// [`open_command`](FilesConfig::open_command).
    ///
    /// With an opener the entry is a suffixed variant: id, history key,
    /// suggestion, and both match fields carry the opener id, and `Enter`
    /// appends the path to the opener's command.
    fn path_entry(&self, target: &Target) -> Option<Entry> {
        let path = target.path.as_path();
        let opener = target.opener.map(|index| &self.config.openers[index]);
        let metadata = std::fs::metadata(path).ok()?;
        let is_dir = metadata.is_dir();
        let name = title(path);
        let display = path.display().to_string();
        let relative = relative_to(&self.config.root, path);

        let opener_id = opener.map(Opener::id);
        let id = self.suffixed(entry_id(path), opener_id);
        let entry_title = match opener {
            None => name.clone(),
            Some(opener) => format!("{name} ({})", opener.id()),
        };
        let icon = match opener {
            None if is_dir => "folder",
            None => file_icon(&name),
            Some(_) => "application-x-executable",
        };
        let suggestion = if is_dir && opener.is_none() {
            format!("{relative}/")
        } else {
            self.suffixed(relative, opener_id)
        };
        let history_key = self.suffixed(format!("{HISTORY_PREFIX}-{display}"), opener_id);

        let builder = entry(id, entry_title)
            .subtitle(display.clone())
            .icon_name(icon)
            .set_query_keeping_prefix(suggestion)
            .history_key(history_key);

        let builder = match opener {
            Some(opener) => {
                let mut command = opener.command.clone();
                command.push(display);
                let cwd = if is_dir {
                    path.to_path_buf()
                } else {
                    path.parent().unwrap_or(path).to_path_buf()
                };
                builder.exec(command).cwd(cwd)
            }
            None if is_dir => builder.terminal_exec(vec![]).cwd(path.to_path_buf()),
            None => builder
                .exec(vec![self.config.open_command.clone(), display])
                .cwd(path.parent().unwrap_or(path).to_path_buf()),
        };
        Some(builder.match_fields(self.match_fields(&name, path, opener_id)))
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
    /// text. A suffixed variant appends the configured symbol and opener id
    /// to both fields, so `"/etc/ho @code"` fuzzy-matches `hosts @code`
    /// while plain entries fall out of the results on their own. The space
    /// before the symbol is deliberate: the engine matches spaces literally,
    /// so it lets both `hosts @code` and `hosts@code` match the same field.
    fn match_fields(&self, name: &str, path: &Path, opener: Option<&str>) -> Vec<MatchField> {
        let field = |text: &str| match opener {
            Some(opener) => format!("{text} {}", self.suffix(opener)),
            None => text.to_string(),
        };
        vec![
            MatchField {
                text: field(&path.display().to_string()),
                weight: self.config.weight_path,
            },
            MatchField {
                text: field(name),
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

/// One entry candidate: a path, optionally opened with a configured opener.
/// Dedup runs on these before any entry is built, so a path and its opener
/// variants are distinct while a variant from the listing and the same
/// variant from history are not.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    path: PathBuf,
    /// Index into [`FilesConfig::openers`]; `None` is the plain entry.
    opener: Option<usize>,
}

/// Merge target sources into one ordered list with duplicates removed: only
/// the first occurrence of a `(path, opener)` pair is kept, so with the
/// sources passed current-first the live listing wins over remembered
/// history and no entry is ever offered twice.
///
/// Deduplicating targets before building any entry keeps the decision cheap
/// and the construction single-pass: an already-covered target is never
/// stat'ed, and every surviving target is turned into exactly one entry.
fn dedup_targets(sources: &[&[Target]]) -> Vec<Target> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for source in sources {
        for target in *source {
            if seen.insert((target.path.clone(), target.opener)) {
                targets.push(target.clone());
            }
        }
    }
    targets
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
        // An opener needs a program to run; anything else cannot produce a
        // working variant. Duplicate ids are dropped so `(path, opener)` and
        // the ids/history keys they produce stay unique.
        let mut seen = HashSet::new();
        self.config
            .openers
            .retain(|opener| !opener.command.is_empty() && seen.insert(opener.id().to_string()));
        ProviderResult::Ok
    }

    fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
        // The suffix marks opener variants, never path text: `/etc/ho @code`
        // lists from `/etc/ho ` and suffixes with `code`.
        let (query, suffix) = self.split_suffix(ctx.query);

        // Current targets: the extended prefix itself and its direct
        // children, plus their opener variants when a suffix is typed.
        let current = self.current_targets(query, suffix.is_some());

        // History targets: remembered path + opener pairs from the `files-`
        // namespace, complementing the current listing. Suffixed history
        // entries show without `@` too, so a used opener is one keystroke
        // away.
        let history = self.history_targets(&ctx);

        // One dedup pass over the targets — current first, so the listing
        // wins over history — then a single construction pass: each
        // surviving target that still exists becomes exactly one entry.
        let mut entries: Vec<Entry> = dedup_targets(&[&current, &history])
            .iter()
            .filter_map(|target| self.path_entry(target))
            .collect();

        let dir = self.extended_prefix(query);
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

    fn opener(name: &str, command: &[&str], targets: &[&str]) -> Opener {
        Opener {
            name: name.to_string(),
            command: command.iter().map(|item| item.to_string()).collect(),
            targets: targets.iter().map(|item| item.to_string()).collect(),
        }
    }

    /// Opener variants extend the listing but are only generated while the
    /// suffix is typed; a plain query still yields the path's normal entry.
    #[test]
    fn opener_variants_are_only_generated_with_a_suffix() {
        let root = temp_dir("variants");
        std::fs::write(root.join("hosts"), "x").unwrap();

        let mut p = provider_at(&root);
        p.config.openers = vec![opener("code", &["code"], &[])];
        let variant_id = format!("{}@code", id(&root.join("hosts")));

        let plain = p.query(ctx("hosts", "/hosts", &QueryHistory::EMPTY));
        assert!(ids(&plain).contains(&id(&root.join("hosts"))));
        assert!(!ids(&plain).contains(&variant_id));

        let suffixed = p.query(ctx("hosts@co", "/hosts@co", &QueryHistory::EMPTY));
        let variant = suffixed
            .iter()
            .find(|entry| entry.entry.id == variant_id)
            .expect("the code variant of hosts");
        assert_eq!(variant.entry.title, "hosts (code)");

        std::fs::remove_dir_all(&root).ok();
    }

    /// Extension targets select files only; `*` and the directory token opt
    /// an opener in for every file and for directory paths (including the
    /// listed directory itself).
    #[test]
    fn opener_variants_respect_targets() {
        let root = temp_dir("variants-filter");
        std::fs::write(root.join("note.txt"), "x").unwrap();
        std::fs::write(root.join("hosts"), "x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();

        let mut p = provider_at(&root);
        p.config.openers = vec![
            opener("code", &["code"], &[".txt"]),
            opener("fm", &["thunar"], &[FILE_TARGET, DIRECTORY_TARGET]),
        ];
        let entries = p.query(ctx("@", "/@", &QueryHistory::EMPTY));
        let entries = ids(&entries);
        let variant = |path: PathBuf, opener: &str| format!("{}@{opener}", id(&path));

        assert!(entries.contains(&variant(root.join("note.txt"), "code")));
        assert!(entries.contains(&variant(root.join("note.txt"), "fm")));
        assert!(entries.contains(&variant(root.join("hosts"), "fm")));
        assert!(entries.contains(&variant(root.join("sub"), "fm")));
        assert!(entries.contains(&variant(root.clone(), "fm")));
        assert!(!entries.contains(&variant(root.join("hosts"), "code")));
        assert!(!entries.contains(&variant(root.join("sub"), "code")));
        assert!(
            !entries.contains(&variant(root.clone(), "code")),
            "directories need the inode/directory target"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A directory-only opener never applies to files, including the files
    /// in the same listing.
    #[test]
    fn opener_variants_can_target_directories_only() {
        let root = temp_dir("variants-dirs-only");
        std::fs::write(root.join("note.txt"), "x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();

        let mut p = provider_at(&root);
        p.config.openers = vec![opener("fm", &["thunar"], &[DIRECTORY_TARGET])];
        let entries = ids(&p.query(ctx("@", "/@", &QueryHistory::EMPTY)));
        let variant = |path: PathBuf| format!("{}@fm", id(&path));

        assert!(entries.contains(&variant(root.clone())));
        assert!(entries.contains(&variant(root.join("sub"))));
        assert!(!entries.contains(&variant(root.join("note.txt"))));

        std::fs::remove_dir_all(&root).ok();
    }

    /// An empty target list means every file and no directory; MIME-shaped
    /// tokens are reserved and do not match yet.
    #[test]
    fn empty_targets_mean_every_file() {
        let root = temp_dir("variants-default");
        std::fs::write(root.join("note.txt"), "x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();

        let mut p = provider_at(&root);
        p.config.openers = vec![
            opener("default", &["code"], &[]),
            opener("mime", &["viewer"], &["text/*"]),
        ];
        let entries = ids(&p.query(ctx("@", "/@", &QueryHistory::EMPTY)));

        assert!(entries.contains(&format!("{}@default", id(&root.join("note.txt")))));
        assert!(!entries.contains(&format!("{}@default", id(&root.join("sub")))));
        assert!(!entries.contains(&format!("{}@default", id(&root))));
        assert!(
            entries.iter().all(|entry| !entry.ends_with("@mime")),
            "unimplemented MIME targets do not match"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A variant carries the opener id in id, history key, suggestion, and
    /// both match fields, and appends the target to the opener's command.
    #[test]
    fn opener_entries_carry_the_suffix_in_every_field() {
        let root = temp_dir("variant-shape");
        std::fs::write(root.join("note.txt"), "x").unwrap();
        let path = root.join("note.txt");
        let display = path.display().to_string();

        let mut p = provider_at(&root);
        p.config.openers = vec![opener("editor", &["code", "-n"], &[])];
        let entries = p.query(ctx("note.txt@edi", "/note.txt@edi", &QueryHistory::EMPTY));
        let entry = entries
            .iter()
            .find(|entry| entry.entry.id == format!("{}@editor", id(&path)))
            .expect("the editor variant");

        assert_eq!(entry.history_key, Some(format!("files-{display}@editor")));
        let suggestion = entry.entry.set_query.as_ref().expect("a suggestion");
        assert_eq!(
            suggestion.resolve(Some(PREFIX)),
            "/note.txt@editor",
            "tab re-types the suffixed path"
        );
        match &entry.entry.action {
            Action::Exec { args, mode, cwd } => {
                assert_eq!(*mode, ExecMode::Direct);
                assert_eq!(
                    args,
                    &vec!["code".to_string(), "-n".into(), display.clone()]
                );
                assert_eq!(cwd.as_deref(), Some(root.as_path()));
            }
            other => panic!("expected a direct exec, got {other:?}"),
        }
        match &entry.rank {
            Rank::MatchFields(fields) => {
                let texts: Vec<&str> = fields.iter().map(|field| field.text.as_str()).collect();
                assert_eq!(
                    texts,
                    vec![format!("{display} @editor"), "note.txt @editor".to_string()]
                );
            }
            other => panic!("expected match fields, got {other:?}"),
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// Suffixed history entries show even on a plain query, and dedup with
    /// the current variant once the suffix is typed.
    #[test]
    fn history_variants_show_without_the_suffix() {
        let root = temp_dir("history-variant");
        let path = root.join("note.txt");
        std::fs::write(&path, "x").unwrap();
        let key = format!("{HISTORY_PREFIX}-{}@code", path.display());
        let store = store_with([key]);
        let history = store.history_for("");

        let mut p = provider_at(&root);
        p.config.openers = vec![opener("code", &["code"], &[])];
        let variant_id = format!("{}@code", id(&path));

        let plain_query = p.query(ctx("", "/", &history));
        assert!(ids(&plain_query).contains(&variant_id));

        let suffixed = p.query(ctx("note.txt@code", "/note.txt@code", &history));
        assert_eq!(
            ids(&suffixed)
                .iter()
                .filter(|entry_id| **entry_id == variant_id)
                .count(),
            1,
            "history does not duplicate the current variant"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// An unrecognized history suffix is path text, so paths containing the
    /// symbol still reconstruct.
    #[test]
    fn unknown_history_suffix_stays_path_text() {
        let root = temp_dir("literal-suffix");
        let path = root.join("note.txt@nope");
        std::fs::write(&path, "x").unwrap();
        let store = store_with([format!("{HISTORY_PREFIX}-{}", path.display())]);
        let history = store.history_for("");

        let mut p = provider_at(&root);
        p.config.openers = vec![opener("code", &["code"], &[])];
        assert!(ids(&p.query(ctx("", "/", &history))).contains(&id(&path)));

        std::fs::remove_dir_all(&root).ok();
    }

    /// The suffix symbol is configurable; the default `@` stops splitting.
    #[test]
    fn the_suffix_symbol_is_configurable() {
        let root = temp_dir("suffix-symbol");
        std::fs::write(root.join("note.txt"), "x").unwrap();

        let mut p = provider_at(&root);
        p.config.suffix = ":".into();
        p.config.openers = vec![opener("code", &["code"], &[])];
        let variant_id = format!("{}@code", id(&root.join("note.txt")));
        assert!(
            !ids(&p.query(ctx("note.txt@code", "/note.txt@code", &QueryHistory::EMPTY)))
                .contains(&variant_id)
        );
        let variant_id = format!("{}:code", id(&root.join("note.txt")));
        assert!(
            ids(&p.query(ctx("note.txt:code", "/note.txt:code", &QueryHistory::EMPTY)))
                .contains(&variant_id)
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// A space before the suffix is just filter text: `/etc/ho @code` still
    /// yields the `code` variants.
    #[test]
    fn opener_variants_work_after_a_space() {
        let root = temp_dir("variants-space");
        std::fs::write(root.join("note.txt"), "x").unwrap();

        let mut p = provider_at(&root);
        p.config.openers = vec![opener("code", &["code"], &[])];
        let entries = p.query(ctx("note.txt @co", "/note.txt @co", &QueryHistory::EMPTY));
        assert!(ids(&entries).contains(&format!("{}@code", id(&root.join("note.txt")))));

        std::fs::remove_dir_all(&root).ok();
    }

    /// Openers without a command cannot run and duplicate ids would collide
    /// in ids and history keys, so both are dropped at init. An unnamed
    /// opener takes its program's basename.
    #[test]
    fn init_normalizes_openers() {
        let root = temp_dir("opener-init");
        let mut p = FilesProvider::new();
        assert!(matches!(
            p.init(InitContext {
                data_dir: Path::new("/tmp"),
                extra: Some(serde_json::json!({
                    "root": root,
                    "suffix": ":",
                    "openers": [
                        { "name": "code", "command": ["code"] },
                        { "name": "code", "command": ["other"] },
                        { "command": ["/usr/bin/firefox"], "targets": ["inode/directory"] },
                        { "name": "broken", "command": [] }
                    ],
                })),
            }),
            ProviderResult::Ok
        ));
        assert_eq!(p.config.suffix, ":");
        let ids: Vec<&str> = p.config.openers.iter().map(Opener::id).collect();
        assert_eq!(ids, vec!["code", "firefox"]);
        assert_eq!(p.config.openers[1].command, vec!["/usr/bin/firefox"]);
        assert_eq!(p.config.openers[1].targets, vec!["inode/directory"]);
        std::fs::remove_dir_all(&root).ok();
    }
}
