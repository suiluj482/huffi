use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
    parse_extra_config,
};
use crate::engine::scoring::MatchField;

/// Per-provider tuning knobs. When provided via
/// `[engine.provider.builtin.nix.extra]`, the fields are parsed from the
/// arbitrary extra config, mirroring [`super::DesktopConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default)]
pub struct NixConfig {
    /// Fuzzy-match weight for the package attrpath.
    pub weight_attr: f32,
    /// Fuzzy-match weight for the package pname.
    pub weight_pname: f32,
    /// Fuzzy-match weight for the package description. `0.0` (default) skips
    /// description matching entirely — it only costs scoring time — while the
    /// description still shows as the row subtitle.
    pub weight_desc: f32,
    /// Regenerate the cached nixpkgs index once it is this old. nixpkgs-unstable
    /// moves daily; the search itself takes ~15s, so it only runs when stale.
    pub cache_max_age_secs: u64,
}

impl Default for NixConfig {
    fn default() -> Self {
        Self {
            weight_attr: 1.0,
            weight_pname: 0.9,
            weight_desc: 0.0,
            cache_max_age_secs: 7 * 24 * 3600,
        }
    }
}

/// Serializes regeneration so parallel daemons or tests don't launch many
/// concurrent nix evaluations at once.
static REFRESH_LOCK: Mutex<()> = Mutex::new(());

/// One entry from `nix search nixpkgs --json ''`.
#[derive(Deserialize)]
struct NixSearchInfo {
    #[serde(default)]
    pname: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// The subset of a nixpkgs package that the provider needs, cached on disk so
/// the full `nix search` only runs when the cache is stale.
#[derive(Serialize, Deserialize)]
struct PackageInfo {
    attr: String,
    pname: String,
    description: Option<String>,
}

/// Provides nixpkgs packages to run via `nix run nixpkgs#<name>`.
///
/// Triggered by the `!` prefix. The index is built asynchronously: `init()`
/// returns [`ProviderResult::Unsupported`] when the `nix` binary is missing,
/// otherwise it spawns a background thread that loads the on-disk cache or
/// (if stale) re-runs `nix search`. Until it is ready, `query()` returns no
/// entries — the same graceful degradation as
/// [`super::CalculatorProvider`] without a rink context.
pub struct NixRunProvider {
    config: NixConfig,
    entries: Arc<RwLock<Arc<[Entry]>>>,
}

impl Default for NixRunProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl NixRunProvider {
    pub fn new() -> Self {
        Self {
            config: NixConfig::default(),
            entries: Arc::new(RwLock::new(Arc::from([]))),
        }
    }

    #[cfg(test)]
    fn with_entries(entries: Vec<Entry>) -> Self {
        Self {
            config: NixConfig::default(),
            entries: Arc::new(RwLock::new(Arc::from(entries))),
        }
    }
}

impl Provider for NixRunProvider {
    fn meta(&self) -> ProviderMeta {
        // Trigger prefix: `!firefox` runs `nix run nixpkgs#firefox`.
        ProviderMeta::builder("nix")
            .prefix("!")
            .prefix_only(true)
            .build()
    }

    fn init(&mut self, ctx: InitContext) -> ProviderResult {
        match parse_extra_config::<NixConfig>(&ctx.extra) {
            Err(result) => return result,
            Ok(Some(config)) => self.config = config,
            Ok(None) => {}
        }

        if !nix_available() {
            return ProviderResult::Unsupported("nix binary not found in PATH".into());
        }

        let entries = Arc::clone(&self.entries);
        let data_dir = ctx.data_dir.to_path_buf();
        let config = self.config;
        std::thread::spawn(move || {
            let built = load_or_build(&data_dir, config);
            if let Ok(mut guard) = entries.write() {
                *guard = built;
            }
        });

        ProviderResult::Ok
    }

    fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
        self.entries
            .read()
            .map(|guard| guard.to_vec())
            .unwrap_or_default()
    }
}

/// Whether the `nix` executable is on PATH. Checked at init so a machine
/// without nix disables the provider via [`ProviderResult::Unsupported`]
/// instead of failing queries later.
fn nix_available() -> bool {
    Command::new("nix")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Load the cached index if fresh, otherwise regenerate it (serialized across
/// threads) and write the cache. Returns an empty index on any failure — the
/// provider stays out of the way rather than crashing the daemon.
fn load_or_build(data_dir: &Path, config: NixConfig) -> Arc<[Entry]> {
    let path = cache_path(data_dir);

    if fresh(&path, config.cache_max_age_secs) && let Some(packages) = load_cache(&path) {
        return build_entries(&packages, config);
    }

    let _guard = REFRESH_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Another thread may have regenerated the cache while we waited for the lock.
    if fresh(&path, config.cache_max_age_secs) && let Some(packages) = load_cache(&path) {
        return build_entries(&packages, config);
    }

    match run_nix_search() {
        Ok(packages) => {
            save_cache(&path, &packages);
            build_entries(&packages, config)
        }
        Err(e) => {
            eprintln!("[nix] failed to index nixpkgs: {e}");
            Arc::from([])
        }
    }
}

/// Run `nix search nixpkgs --json ''`, which lists every package in nixpkgs.
fn run_nix_search() -> anyhow::Result<Vec<PackageInfo>> {
    let out = Command::new("nix")
        .args(["search", "nixpkgs", "--json", ""])
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("nix search failed: {}", err.trim());
    }
    parse_nix_search_json(&out.stdout)
}

/// Parse the JSON emitted by `nix search --json` into package info.
///
/// Keys look like `legacyPackages.x86_64-linux.hello` (nested attrpaths such
/// as `legacyPackages.x86_64-linux.CuboCore.coreaction` also occur). The
/// `legacyPackages.<system>.` prefix is stripped, leaving the attrpath that
/// `nix run nixpkgs#<attrpath>` understands.
fn parse_nix_search_json(json: &[u8]) -> anyhow::Result<Vec<PackageInfo>> {
    let map: HashMap<String, NixSearchInfo> = serde_json::from_slice(json)?;
    let mut packages = Vec::with_capacity(map.len());
    for (key, info) in map {
        let Some(attr) = attr_path(&key) else {
            continue;
        };
        let pname = info
            .pname
            .unwrap_or_else(|| attr.rsplit('.').next().unwrap().to_string());
        packages.push(PackageInfo {
            attr,
            pname,
            description: info.description,
        });
    }
    Ok(packages)
}

/// Strip the `legacyPackages.<system>.` prefix from a full search-result key,
/// keeping any nested attrpath (`CuboCore.coreaction` stays intact).
fn attr_path(key: &str) -> Option<String> {
    let mut parts = key.splitn(3, '.');
    let _root = parts.next()?;
    let _system = parts.next()?;
    parts.next().map(str::to_string)
}

fn build_entries(packages: &[PackageInfo], config: NixConfig) -> Arc<[Entry]> {
    packages.iter().map(|p| build_entry(p, config)).collect()
}

fn build_entry(p: &PackageInfo, config: NixConfig) -> Entry {
    let mut fields = vec![MatchField {
        text: p.attr.clone(),
        weight: config.weight_attr,
    }];
    if p.pname != p.attr {
        fields.push(MatchField {
            text: p.pname.clone(),
            weight: config.weight_pname,
        });
    }

    let id = format!("nix-{}", p.attr);
    let mut e = entry(&id, &p.pname)
        .terminal_exec(vec![
            "nix".into(),
            "run".into(),
            format!("nixpkgs#{}", p.attr),
        ])
        .history_key(&id);

    if let Some(desc) = &p.description {
        e = e.subtitle(desc.clone());
        if config.weight_desc > 0.0 {
            fields.push(MatchField {
                text: desc.clone(),
                weight: config.weight_desc,
            });
        }
    }

    e.match_fields(fields)
}

/// The index lives in the provider's own data folder (from
/// [`InitContext`]), which the engine creates before `init()` runs.
fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("nixpkgs.json")
}

fn fresh(path: &Path, max_age_secs: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(age) = modified.elapsed() else {
        return false;
    };
    age.as_secs() < max_age_secs
}

fn load_cache(path: &Path) -> Option<Vec<PackageInfo>> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_cache(path: &Path, packages: &[PackageInfo]) {
    let json = match serde_json::to_string(packages) {
        Ok(json) => json,
        Err(e) => {
            eprintln!("[nix] failed to serialize cache: {e}");
            return;
        }
    };
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    if let Err(e) = std::fs::write(path, json) {
        eprintln!("[nix] failed to write cache {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> &'static str {
        r#"{
            "legacyPackages.x86_64-linux.hello": {
                "pname": "hello",
                "description": "A program that produces a familiar, friendly greeting"
            },
            "legacyPackages.x86_64-linux.CuboCore.coreaction": {
                "pname": "coreaction",
                "description": "Side bar for showing widgets from the C Suite"
            },
            "legacyPackages.x86_64-linux.nopname": {
                "description": "Package without a pname"
            }
        }"#
    }

    fn parse() -> Vec<PackageInfo> {
        parse_nix_search_json(sample_json().as_bytes()).unwrap()
    }

    #[test]
    fn parses_and_strips_prefix() {
        let packages = parse();
        assert_eq!(packages.len(), 3);
        assert!(
            packages
                .iter()
                .any(|p| p.attr == "hello" && p.pname == "hello")
        );
        assert!(
            packages
                .iter()
                .any(|p| p.attr == "nopname" && p.pname == "nopname")
        );
    }

    #[test]
    fn preserves_nested_attrpath() {
        let packages = parse();
        let nested = packages
            .iter()
            .find(|p| p.attr == "CuboCore.coreaction")
            .expect("nested attrpath");
        assert_eq!(nested.pname, "coreaction");
        assert_eq!(
            nested.description.as_deref(),
            Some("Side bar for showing widgets from the C Suite")
        );
    }

    #[test]
    fn pname_missing_falls_back_to_attr() {
        let packages = parse();
        let no_pname = packages
            .iter()
            .find(|p| p.attr == "nopname")
            .expect("package without pname");
        assert_eq!(no_pname.pname, "nopname");
    }

    #[test]
    fn attr_path_strips_legacy_prefix() {
        assert_eq!(
            attr_path("legacyPackages.x86_64-linux.hello").as_deref(),
            Some("hello")
        );
        assert_eq!(
            attr_path("legacyPackages.x86_64-linux.CuboCore.coreaction").as_deref(),
            Some("CuboCore.coreaction")
        );
        assert_eq!(attr_path("only-once"), None);
    }

    #[test]
    fn build_entry_execs_nix_run() {
        let package = PackageInfo {
            attr: "hello".into(),
            pname: "hello".into(),
            description: Some("A program that produces a familiar, friendly greeting".into()),
        };
        let e = build_entry(&package, NixConfig::default());
        match &e.entry.action {
            crate::engine::provider::Action::Exec { args, terminal } => {
                assert!(terminal);
                assert_eq!(
                    args,
                    &vec![
                        "nix".to_string(),
                        "run".to_string(),
                        "nixpkgs#hello".to_string()
                    ]
                );
            }
            _ => panic!("expected Exec action"),
        }
        assert_eq!(e.history_key.as_deref(), Some("nix-hello"));
        assert_eq!(e.entry.title, "hello");
        assert_eq!(
            e.entry.subtitle.as_deref(),
            Some("A program that produces a familiar, friendly greeting")
        );
    }

    #[test]
    fn build_entry_match_fields_weighted() {
        let package = PackageInfo {
            attr: "CuboCore.coreaction".into(),
            pname: "coreaction".into(),
            description: Some("Side bar".into()),
        };
        let config = NixConfig::default();
        let e = build_entry(&package, config);
        let fields = match &e.rank {
            crate::engine::scoring::Rank::MatchFields(fields) => fields,
            other => panic!("expected MatchFields, got {other:?}"),
        };
        assert_eq!(fields.len(), 2);
        assert!(
            fields
                .iter()
                .any(|f| f.text == "CuboCore.coreaction" && f.weight == config.weight_attr)
        );
        assert!(
            fields
                .iter()
                .any(|f| f.text == "coreaction" && f.weight == config.weight_pname)
        );
    }

    #[test]
    fn extra_config_overrides_weights() {
        let package = PackageInfo {
            attr: "hello".into(),
            pname: "hello".into(),
            description: Some("A program that produces a familiar, friendly greeting".into()),
        };
        let config: NixConfig =
            serde_json::from_value(serde_json::json!({ "weight_attr": 2.0 })).unwrap();
        let e = build_entry(&package, config);
        let fields = match &e.rank {
            crate::engine::scoring::Rank::MatchFields(fields) => fields,
            other => panic!("expected MatchFields, got {other:?}"),
        };
        assert!(
            fields
                .iter()
                .all(|f| f.text != "A program that produces a familiar, friendly greeting"),
            "description is not a match field by default"
        );
    }

    #[test]
    fn description_matched_only_when_weighted() {
        let package = PackageInfo {
            attr: "hello".into(),
            pname: "hello".into(),
            description: Some("A program that produces a familiar, friendly greeting".into()),
        };
        let default = NixConfig::default();
        assert!(!description_is_match_field(&package, default));

        let enabled = NixConfig {
            weight_desc: 0.5,
            ..NixConfig::default()
        };
        assert!(description_is_match_field(&package, enabled));
    }

    fn description_is_match_field(package: &PackageInfo, config: NixConfig) -> bool {
        let e = build_entry(package, config);
        let fields = match &e.rank {
            crate::engine::scoring::Rank::MatchFields(fields) => fields,
            other => panic!("expected MatchFields, got {other:?}"),
        };
        fields
            .iter()
            .any(|f| f.text == package.description.as_deref().unwrap())
    }

    #[test]
    fn query_returns_entries_without_prefix() {
        let mut p = NixRunProvider::with_entries(vec![build_entry(
            &PackageInfo {
                attr: "hello".into(),
                pname: "hello".into(),
                description: None,
            },
            NixConfig::default(),
        )]);
        assert_eq!(
            p.query(QueryContext {
                prefix: None,
                query: "",
                original: "",
            })
            .len(),
            1
        );
        assert_eq!(
            p.query(QueryContext {
                prefix: None,
                query: "hello",
                original: "hello",
            })
            .len(),
            1
        );
    }

    #[test]
    fn query_returns_entries_with_prefix() {
        let mut p = NixRunProvider::with_entries(vec![build_entry(
            &PackageInfo {
                attr: "hello".into(),
                pname: "hello".into(),
                description: None,
            },
            NixConfig::default(),
        )]);
        let entries = p.query(QueryContext {
            prefix: Some("!"),
            query: "hell",
            original: "!hell",
        });
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entry.id, "nix-hello");
    }

    #[test]
    fn cache_roundtrip() {
        let packages = parse();
        let json = serde_json::to_string(&packages).unwrap();
        let back: Vec<PackageInfo> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), packages.len());
        assert_eq!(back[0].attr, packages[0].attr);
    }

    /// Profiling harness (run: `cargo test --release profile_nix -- --ignored
    /// --nocapture`). Times index build, per-keystroke clone, and fuzzy
    /// scoring against the real on-disk cache.
    #[test]
    #[ignore]
    fn profile_nix_keystroke_cost() {
        use crate::engine::scoring::base_scorer::BaseScorer;
        use crate::engine::scoring::QueryGroup;
        use std::time::Instant;

        let home = std::env::var("HOME").expect("HOME set");
        let data_dir = std::env::var("HUFFI_PROFILE_DATA")
            .unwrap_or_else(|_| format!("{home}/.local/share/huffi/providers/nix"));
        let config = NixConfig::default();

        let t = Instant::now();
        let entries = load_or_build(Path::new(&data_dir), config);
        eprintln!(
            "[prof] index build: {} entries in {:?}",
            entries.len(),
            t.elapsed()
        );

        for needle in ["f", "fi", "fire", "firefox"] {
            let t = Instant::now();
            let cloned: Vec<Entry> = entries.to_vec();
            let clone_dur = t.elapsed();

            let mut scorer = BaseScorer::default();
            let groups = vec![QueryGroup {
                query: needle.to_string(),
                entries: cloned,
            }];
            let t = Instant::now();
            let scored = scorer.base_scoring(groups);
            let score_dur = t.elapsed();

            eprintln!(
                "[prof] query '{needle}': clone {:?} + base_scoring {:?} = {:?} ({} matches)",
                clone_dur,
                score_dur,
                clone_dur + score_dur,
                scored.len()
            );
        }
    }
}
