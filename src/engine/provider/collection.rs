use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;

use crate::engine::scoring::QueryGroup;

use super::config::{ProviderConfig, ProviderOverride, default_target_prefix};
use super::{
    ActionsProvider, CalculatorProvider, DesktopEntryProvider, EntryMeta, HandleContext,
    InitContext, NixRunProvider, Provider, ProviderMeta, ProviderResult, QueryContext,
    RunnerProvider, UnicodeProvider, CliphistProvider,
};

pub struct ProviderCollection {
    /// Registered providers in insertion order, with the metadata used for
    /// them (the `enabled` flag may be flipped by a failed init).
    providers: Vec<(Box<dyn Provider>, ProviderMeta)>,
    /// User-config overrides keyed by provider id.
    overrides: HashMap<String, ProviderOverride>,
    /// Prefixes that own a query outright: when the resolved global prefix
    /// is one of these, only providers declaring that prefix are queried.
    exclusive_prefixes: Vec<String>,
    /// The delimiter introducing a `\<id> ` provider target, from config.
    target_prefix: String,
    /// Huffi's data folder; each provider gets `data_dir/providers/<id>/`.
    data_dir: std::path::PathBuf,
    /// Skip creating on-disk state when true.
    dry_run: bool,
}

/// The result of resolving the global prefix for a query.
///
/// The longest declared provider prefix that the query starts with wins;
/// there is at most one active prefix per query. A `\<id> ` query resolves
/// instead to a *target*: only the provider with id `<id>` is queried, and
/// [`prefix`](Self::prefix) holds the whole `\<id> ` token, separator
/// included.
#[derive(Debug, Clone)]
pub struct PreprocessedQuery {
    pub original_query: String,
    /// The prefix token that scopes the query, if any. For a `\<id> ` target
    /// this includes the trailing separator (`"\calc "`); a declared prefix is
    /// stored verbatim.
    pub prefix: Option<String>,
    /// Whether only providers owning [`prefix`](Self::prefix) answer this
    /// query: the target namespace, a target, or a configured exclusive
    /// prefix.
    pub exclusive: bool,
    pub query: String,
}

/// Whether `meta` owns `prefix`: it declared it, or `prefix` is the
/// `<target_prefix><id> ` target token naming it.
pub fn matches_prefix(meta: &ProviderMeta, target_prefix: &str, prefix: &str) -> bool {
    meta.prefixes.iter().any(|p| p == prefix) || prefix == format!("{target_prefix}{} ", meta.id)
}

/// Whether `meta` should answer `pre`: it owns the active prefix, or the
/// query is unscoped or non-exclusive and the provider is not prefix-only.
///
/// This is the single gate shared by dispatch and by the UI (e.g. a footer
/// that marks every provider currently active).
pub fn matches_query(meta: &ProviderMeta, target_prefix: &str, pre: &PreprocessedQuery) -> bool {
    meta.enabled
        && (pre
            .prefix
            .as_deref()
            .is_some_and(|prefix| matches_prefix(meta, target_prefix, prefix))
            || (!pre.exclusive && !meta.prefix_only))
}

impl ProviderCollection {
    /// Construct the collection with the built-in providers configured from
    /// `config`. Each provider is registered with `add_provider`, which
    /// provisions its `<data_dir>/providers/<provider id>/` folder (when
    /// not in dry-run mode) and calls its [`init`](Provider::init) — unless
    /// the provider is disabled by its own default or by user config.
    /// `config.exclusive_prefixes` drives the exclusivity filter applied when
    /// entries are grouped, and `config.target_prefix` the delimiter that
    /// introduces a provider target (empty falls back to the default).
    pub fn new_with_config(
        data_dir: impl AsRef<Path>,
        dry_run: bool,
        config: &ProviderConfig,
    ) -> anyhow::Result<Self> {
        let target_prefix = if config.target_prefix.is_empty() {
            default_target_prefix()
        } else {
            config.target_prefix.clone()
        };
        let mut collection = Self {
            providers: Vec::new(),
            overrides: config.builtin.clone(),
            exclusive_prefixes: config.exclusive_prefixes.clone(),
            target_prefix,
            data_dir: data_dir.as_ref().to_path_buf(),
            dry_run,
        };
        collection.add_provider(Box::new(DesktopEntryProvider::new(
            freedesktop_desktop_entry::default_paths().collect(),
        )))?;
        collection.add_provider(Box::new(ActionsProvider::new()))?;
        collection.add_provider(Box::new(CalculatorProvider::new()))?;
        collection.add_provider(Box::new(NixRunProvider::new()))?;
        collection.add_provider(Box::new(UnicodeProvider::new()))?;
        collection.add_provider(Box::new(RunnerProvider::new()))?;
        collection.add_provider(Box::new(CliphistProvider::new()))?;
        Ok(collection)
    }
}

impl ProviderCollection {
    pub fn add_provider(&mut self, mut provider: Box<dyn Provider>) -> anyhow::Result<()> {
        let mut meta = provider.meta();
        let id = meta.id.clone();

        // Apply user-config overrides; the id is never overridden.
        if let Some(ov) = self.overrides.get(&id) {
            meta = ov.apply(meta);
        }

        // Reject a missing id loudly and resolve an empty name to the id.
        let mut meta = meta.resolved()?;

        // A provider disabled by its own default or by user config is
        // registered but never initialized: no data folder is provisioned and
        // `init` is not called.
        if meta.enabled {
            let dir = self.data_dir.join("providers").join(&id);
            if !self.dry_run {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("failed to create data dir {}", dir.display()))?;
            }
            let extra = self.overrides.get(&id).and_then(|ov| ov.extra.clone());
            match provider.init(InitContext {
                data_dir: &dir,
                extra,
            }) {
                ProviderResult::Ok => {}
                ProviderResult::Unsupported(msg) => {
                    eprintln!("[provider] {id} unsupported: {msg}");
                    meta.enabled = false;
                }
                ProviderResult::Other(msg) => {
                    eprintln!("[provider] {id} disabled: {msg}");
                    meta.enabled = false;
                }
                ProviderResult::Config { msg, critical } => {
                    if critical {
                        eprintln!("[provider] {id} config error, disabled: {msg}");
                        meta.enabled = false;
                    } else {
                        eprintln!("[provider] {id} config warning, using defaults: {msg}");
                    }
                }
            }
        }
        self.providers.push((provider, meta));
        Ok(())
    }

    /// Resolve the global prefix for a query.
    ///
    /// A `<target_prefix><id> <rest>` query (see
    /// [`target_prefix`](ProviderConfig::target_prefix)) that names an enabled
    /// provider resolves to a target for that provider: [`prefix`] is the
    /// whole `<target_prefix><id> ` token (separator included), [`exclusive`]
    /// is `true`, and [`query`] is `<rest>` verbatim. The id must be followed
    /// by whitespace, so typing `\desktop` still filters the provider list and
    /// only `\desktop ` starts targeting it.
    ///
    /// Otherwise the longest declared provider prefix that the query starts
    /// with wins. If several prefixes are tied in length, the first declared
    /// wins. A prefix in the target namespace, or one listed in
    /// [`ProviderConfig::exclusive_prefixes`](super::config::ProviderConfig::exclusive_prefixes),
    /// is marked [`exclusive`].
    ///
    /// [`prefix`]: PreprocessedQuery::prefix
    /// [`exclusive`]: PreprocessedQuery::exclusive
    /// [`query`]: PreprocessedQuery::query
    pub fn preprocess_query(&self, query: &str) -> PreprocessedQuery {
        if let Some(target) = self.target_query(query) {
            return target;
        }

        let mut longest: Option<String> = None;
        for (_, meta) in &self.providers {
            if !meta.enabled {
                continue;
            }
            for prefix in &meta.prefixes {
                if !prefix.is_empty()
                    && query.starts_with(prefix)
                    && longest
                        .as_deref()
                        .is_none_or(|current| prefix.len() > current.len())
                {
                    longest = Some(prefix.clone());
                }
            }
        }

        match longest {
            Some(prefix) => {
                let exclusive = prefix.starts_with(self.target_prefix.as_str())
                    || self.exclusive_prefixes.iter().any(|e| e == &prefix);
                PreprocessedQuery {
                    original_query: query.to_string(),
                    prefix: Some(prefix.clone()),
                    exclusive,
                    query: query[prefix.len()..].to_string(),
                }
            }
            None => PreprocessedQuery {
                original_query: query.to_string(),
                prefix: None,
                exclusive: false,
                query: query.to_string(),
            },
        }
    }

    /// Resolve a provider-targeting query, or `None` when `query` is not one.
    ///
    /// The shape is `\<id>` followed by whitespace: the first token after the
    /// delimiter must be an enabled provider's id. A partial id (`\desk`), a
    /// bare delimiter (`\`), or a delimiter not followed by whitespace falls
    /// through to ordinary prefix matching, where the providers provider
    /// handles the listing.
    fn target_query(&self, query: &str) -> Option<PreprocessedQuery> {
        let rest = query.strip_prefix(self.target_prefix.as_str())?;
        let ws = rest.find(char::is_whitespace)?;
        let id = &rest[..ws];
        if !self
            .providers
            .iter()
            .any(|(_, meta)| meta.enabled && meta.id == id)
        {
            return None;
        }

        Some(PreprocessedQuery {
            original_query: query.to_string(),
            prefix: Some(format!("{}{id} ", self.target_prefix)),
            exclusive: true,
            query: rest[ws..].trim_start().to_string(),
        })
    }

    /// The provider-relative [`QueryContext`] for `meta`: prefix kept and text
    /// stripped when this provider owns the query's global prefix (declared it
    /// or is the query's target), otherwise prefix dropped and the full query
    /// kept.
    fn provider_query_context<'a>(
        meta: &ProviderMeta,
        pre: &'a PreprocessedQuery,
        target_prefix: &str,
    ) -> QueryContext<'a> {
        let owns = pre
            .prefix
            .as_deref()
            .is_some_and(|prefix| matches_prefix(meta, target_prefix, prefix));
        if owns {
            QueryContext {
                prefix: pre.prefix.as_deref(),
                query: &pre.query,
                original: &pre.original_query,
            }
        } else {
            QueryContext {
                prefix: None,
                query: &pre.original_query,
                original: &pre.original_query,
            }
        }
    }

    /// Query each provider and group its entries with the query they should
    /// be fuzzy-scored against. Entries are annotated with their provider id.
    ///
    /// Only providers for which [`matches_query`] holds are queried: those
    /// owning the active prefix, plus — unless the prefix is exclusive or the
    /// provider is prefix-only — every other provider. A `\<id> ` target and
    /// the bare listing prefix `\` are exclusive, so only the targeted or
    /// listing provider answers.
    ///
    /// Scoring itself is owned by [`crate::engine::Engine`]; export the raw
    /// groups here so the engine can hand them to the
    /// [`Scorer`](crate::engine::scoring::Scorer).
    pub(crate) fn grouped_entries(
        &mut self,
        pre: &PreprocessedQuery,
    ) -> Vec<QueryGroup<EntryMeta>> {
        let target_prefix = self.target_prefix.clone();
        self.providers
            .iter_mut()
            .filter_map(|(p, meta)| {
                if !matches_query(meta, &target_prefix, pre) {
                    return None;
                }
                let ctx = Self::provider_query_context(meta, pre, &target_prefix);
                let mut entries = p.query(ctx);
                for e in entries.iter_mut() {
                    e.entry.provider_id = Some(meta.id.clone());
                }
                Some(QueryGroup {
                    query: ctx.query.to_string(),
                    entries,
                })
            })
            .collect()
    }

    /// Notify the provider that produced `entry_id` that the entry was
    /// selected. The [`QueryContext`] built for the provider is
    /// provider-relative: `prefix` is `Some` only when the provider owns the
    /// query's global prefix (declared it or is the query's target). No-op
    /// when the provider is no longer registered.
    pub fn handle(&mut self, provider_id: &str, entry_id: &str, pre: &PreprocessedQuery) {
        let target_prefix = self.target_prefix.clone();
        for (provider, meta) in &mut self.providers {
            if meta.id != provider_id {
                continue;
            }
            provider.handle(HandleContext {
                entry_id,
                query: Self::provider_query_context(meta, pre, &target_prefix),
            });
            break;
        }
    }

    /// List registered providers and their trigger prefixes.
    pub fn providers(&self) -> Vec<ProviderMeta> {
        self.providers
            .iter()
            .map(|(_, meta)| meta.clone())
            .collect()
    }

    /// The delimiter introducing a provider target, from config (never empty).
    pub fn target_prefix(&self) -> &str {
        &self.target_prefix
    }

    /// The number of registered providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::engine::provider::{
        Entry, HandleContext, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext,
        TestProvider, entry,
    };
    use crate::engine::scoring::MatchField;

    type CallLog = Arc<Mutex<Vec<(String, Option<String>, String)>>>;
    type HandleLog = Arc<Mutex<Vec<(String, Option<String>, String)>>>;

    /// Tests only: a dry-run collection against an ephemeral folder, so no
    /// providers can touch the real data dir.
    fn collection() -> ProviderCollection {
        let dir = std::env::temp_dir().join(format!("huffi-providers-{}", std::process::id()));
        ProviderCollection::new_with_config(dir, true, &ProviderConfig::default()).unwrap()
    }

    /// Tests only: like [`collection()`], but with `exclusive` as the
    /// configured set of prefixes that own a query outright.
    fn collection_with_exclusive(exclusive: &[&str]) -> ProviderCollection {
        let dir = std::env::temp_dir().join(format!("huffi-providers-excl-{}", std::process::id()));
        let config = ProviderConfig {
            builtin: HashMap::new(),
            exclusive_prefixes: exclusive.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        };
        ProviderCollection::new_with_config(dir, true, &config).unwrap()
    }

    struct TrackingProvider {
        id: String,
        prefixes: Vec<&'static str>,
        calls: CallLog,
        handles: HandleLog,
    }

    impl TrackingProvider {
        fn new(id: &str, prefixes: Vec<&'static str>, calls: CallLog) -> Self {
            Self::with_handle_log(id, prefixes, calls, Arc::new(Mutex::new(Vec::new())))
        }

        fn with_handle_log(
            id: &str,
            prefixes: Vec<&'static str>,
            calls: CallLog,
            handles: HandleLog,
        ) -> Self {
            Self {
                id: id.into(),
                prefixes,
                calls,
                handles,
            }
        }
    }

    impl Provider for TrackingProvider {
        fn meta(&self) -> ProviderMeta {
            ProviderMeta::builder(&self.id)
                .prefixes(self.prefixes.iter().copied())
                .build()
        }

        fn init(&mut self, _ctx: InitContext) -> ProviderResult {
            ProviderResult::Ok
        }

        fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
            self.calls.lock().unwrap().push((
                self.id.clone(),
                ctx.prefix.map(String::from),
                ctx.query.to_string(),
            ));
            vec![entry(&self.id, &self.id).history_key(&self.id).score(1.0)]
        }

        fn handle(&mut self, ctx: HandleContext) {
            self.handles.lock().unwrap().push((
                ctx.entry_id.to_string(),
                ctx.query.prefix.map(String::from),
                ctx.query.query.to_string(),
            ));
        }
    }

    #[test]
    fn preprocess_no_prefix_matches() {
        let c = collection();
        let pre = c.preprocess_query("firefox");
        assert_eq!(pre.prefix, None);
        assert_eq!(pre.original_query, "firefox");
        assert_eq!(pre.query, "firefox");
    }

    #[test]
    fn preprocess_single_prefix_matches() {
        let c = collection();
        let pre = c.preprocess_query("= 2 + 2");
        assert_eq!(pre.prefix.as_deref(), Some("="));
        assert_eq!(pre.original_query, "= 2 + 2");
        assert_eq!(pre.query, " 2 + 2");
    }

    #[test]
    fn preprocess_longest_prefix_wins() {
        let mut c = collection();
        c.add_provider(Box::new(TrackingProvider::new(
            "long",
            vec!["=="],
            Arc::new(Mutex::new(Vec::new())),
        )))
        .unwrap();
        let pre = c.preprocess_query("== 2 + 2");
        assert_eq!(pre.prefix.as_deref(), Some("=="));
        assert_eq!(pre.query, " 2 + 2");
    }

    #[test]
    fn preprocess_multiple_prefixes_on_one_provider() {
        let mut c = collection();
        c.add_provider(Box::new(TrackingProvider::new(
            "calc",
            vec!["=", "=="],
            Arc::new(Mutex::new(Vec::new())),
        )))
        .unwrap();
        let pre = c.preprocess_query("== 2");
        assert_eq!(pre.prefix.as_deref(), Some("=="));
        assert_eq!(pre.query, " 2");
    }

    #[test]
    fn target_requires_trailing_whitespace() {
        let c = collection();
        // No trailing space: not a target, and nothing declares `\`, so no
        // prefix resolves either.
        let pre = c.preprocess_query("\\desktop");
        assert_eq!(pre.prefix, None);

        let pre = c.preprocess_query("\\desktop fire");
        assert_eq!(pre.prefix.as_deref(), Some("\\desktop "));
        assert!(pre.exclusive);
        assert_eq!(pre.query, "fire");
        assert_eq!(pre.original_query, "\\desktop fire");
    }

    #[test]
    fn configured_target_prefix_replaces_the_default() {
        let dir = std::env::temp_dir().join(format!(
            "huffi-providers-target-prefix-{}",
            std::process::id()
        ));
        let config = ProviderConfig {
            target_prefix: "@".to_string(),
            ..Default::default()
        };
        let mut c = ProviderCollection::new_with_config(&dir, true, &config).unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "calc",
            vec![],
            Arc::new(Mutex::new(Vec::new())),
        )))
        .unwrap();

        assert_eq!(c.target_prefix(), "@");
        let pre = c.preprocess_query("@calc 2");
        assert_eq!(pre.prefix.as_deref(), Some("@calc "));
        assert!(pre.exclusive);
        assert_eq!(pre.query, "2");
        assert_eq!(
            c.preprocess_query("\\calc 2").prefix,
            None,
            "the default delimiter no longer targets once overridden"
        );

        let empty = ProviderConfig {
            target_prefix: String::new(),
            ..Default::default()
        };
        let c = ProviderCollection::new_with_config(&dir, true, &empty).unwrap();
        assert_eq!(c.target_prefix(), "\\", "an empty prefix falls back");
    }

    #[test]
    fn target_queries_only_the_named_provider() {
        let mut c = collection();
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        c.add_provider(Box::new(TrackingProvider::new(
            "desk",
            vec![],
            Arc::clone(&calls),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "other",
            vec![],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("\\desk fi");
        let _ = c.grouped_entries(&pre);

        assert_eq!(
            *calls.lock().unwrap(),
            vec![(
                "desk".to_string(),
                Some("\\desk ".to_string()),
                "fi".to_string()
            )],
            "only the targeted provider is queried, with the target token as its prefix"
        );
    }

    #[test]
    fn target_passes_body_verbatim() {
        let mut c = collection();
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        c.add_provider(Box::new(TrackingProvider::new(
            "calc",
            vec!["="],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("\\calc = 2 + 2");
        assert_eq!(pre.prefix.as_deref(), Some("\\calc "));
        assert!(pre.exclusive);
        assert_eq!(pre.query, "= 2 + 2", "the target's own `=` is left alone");

        let _ = c.grouped_entries(&pre);
        assert_eq!(
            *calls.lock().unwrap(),
            vec![(
                "calc".to_string(),
                Some("\\calc ".to_string()),
                "= 2 + 2".to_string()
            )]
        );
    }

    #[test]
    fn disabled_provider_is_not_targetable() {
        let dir = std::env::temp_dir().join(format!(
            "huffi-providers-target-disabled-{}",
            std::process::id()
        ));
        let config = ProviderConfig {
            builtin: HashMap::from([(
                "calc".to_string(),
                ProviderOverride {
                    name: None,
                    enabled: Some(false),
                    prefixes: None,
                    prefix_only: None,
                    extra: None,
                },
            )]),
            exclusive_prefixes: Vec::new(),
            ..Default::default()
        };
        let mut c = ProviderCollection::new_with_config(dir, true, &config).unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "calc",
            vec!["="],
            Arc::new(Mutex::new(Vec::new())),
        )))
        .unwrap();

        let pre = c.preprocess_query("\\calc 2");
        assert_eq!(pre.prefix, None, "a disabled provider cannot be targeted");
    }

    #[test]
    fn handle_passes_target_relative_context() {
        let mut c = collection();
        let handles: HandleLog = Arc::new(Mutex::new(Vec::new()));
        c.add_provider(Box::new(TrackingProvider::with_handle_log(
            "calc",
            vec!["="],
            Arc::new(Mutex::new(Vec::new())),
            Arc::clone(&handles),
        )))
        .unwrap();

        let pre = c.preprocess_query("\\calc 2 + 2");
        c.handle("calc", "calc-entry", &pre);

        assert_eq!(
            *handles.lock().unwrap(),
            vec![(
                "calc-entry".to_string(),
                Some("\\calc ".into()),
                "2 + 2".into()
            )]
        );
    }

    #[test]
    fn target_listing_prefix_is_exclusive() {
        let mut c = collection();
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        // Stand in for the providers provider declaring the target prefix.
        c.add_provider(Box::new(TrackingProvider::new(
            "providers",
            vec!["\\"],
            Arc::clone(&calls),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "desk",
            vec![],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("\\desk");
        assert_eq!(pre.prefix.as_deref(), Some("\\"));
        assert!(pre.exclusive);
        let _ = c.grouped_entries(&pre);

        let log = calls.lock().unwrap();
        assert_eq!(
            log.iter().map(|(id, _, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["providers"],
            "the bare target prefix is exclusive"
        );
    }

    #[test]
    fn entries_dispatch_original_query_to_provider_without_global_prefix() {
        let mut c = collection();
        let calls = Arc::new(Mutex::new(Vec::new()));
        c.add_provider(Box::new(TrackingProvider::new(
            "short",
            vec!["="],
            Arc::clone(&calls),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "long",
            vec!["=="],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("== 2");
        let _ = c.grouped_entries(&pre);

        let log = calls.lock().unwrap();
        let short = log.iter().find(|(id, _, _)| id == "short").unwrap();
        let long = log.iter().find(|(id, _, _)| id == "long").unwrap();
        assert_eq!(short, &("short".to_string(), None, "== 2".to_string()));
        assert_eq!(
            long,
            &("long".to_string(), Some("==".to_string()), " 2".to_string())
        );
    }

    fn match_fields_entry(id: &str, text: &str) -> Entry {
        entry(id, id).history_key(id).match_fields(vec![MatchField {
            text: text.into(),
            weight: 1.0,
        }])
    }

    #[test]
    fn entries_are_stamped_with_provider_id() {
        let mut c = collection();
        c.add_provider(Box::new(TestProvider::new(
            "desktop",
            vec![match_fields_entry("desktop", "Firefox")],
        )))
        .unwrap();
        let pre = c.preprocess_query("firefox");
        let entries: Vec<_> = c
            .grouped_entries(&pre)
            .into_iter()
            .flat_map(|g| g.entries)
            .map(|s| s.entry)
            .collect();
        for e in &entries {
            assert!(e.provider_id.is_some());
        }
        assert!(
            entries
                .iter()
                .any(|e| e.provider_id.as_deref() == Some("desktop"))
        );
    }

    #[test]
    fn providers_lists_ids_and_prefixes() {
        let c = collection();
        let providers = c.providers();
        assert!(
            providers
                .iter()
                .any(|p| p.id == "desktop" && p.name == "desktop" && p.prefixes.is_empty())
        );
        assert!(providers.iter().any(|p| {
            p.id == "calculator" && p.name == "calculator" && p.prefixes == vec!["="]
        }));
    }

    /// The actions provider ships always-on and inert: no prefix to type,
    /// nothing to show until the user configures entries.
    #[test]
    fn actions_provider_is_registered_always_active() {
        let c = collection();
        let actions = c
            .providers()
            .into_iter()
            .find(|p| p.id == "actions")
            .expect("the actions provider is registered");
        assert!(actions.enabled);
        assert!(actions.prefixes.is_empty());
        assert!(!actions.prefix_only);
    }

    #[test]
    fn handle_dispatches_to_matching_provider_only() {
        let mut c = collection();
        let a_handles: HandleLog = Arc::new(Mutex::new(Vec::new()));
        let b_handles: HandleLog = Arc::new(Mutex::new(Vec::new()));
        c.add_provider(Box::new(TrackingProvider::with_handle_log(
            "a",
            vec![],
            Arc::new(Mutex::new(Vec::new())),
            Arc::clone(&a_handles),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::with_handle_log(
            "b",
            vec![],
            Arc::new(Mutex::new(Vec::new())),
            Arc::clone(&b_handles),
        )))
        .unwrap();

        let pre = c.preprocess_query("");
        c.handle("b", "b-entry", &pre);
        c.handle("missing", "ignored", &pre);

        assert!(a_handles.lock().unwrap().is_empty());
        assert_eq!(
            *b_handles.lock().unwrap(),
            vec![("b-entry".to_string(), None, "".to_string())]
        );
    }

    #[test]
    fn handle_passes_provider_relative_context() {
        let mut c = collection();
        let prefixed: HandleLog = Arc::new(Mutex::new(Vec::new()));
        let unprefixed: HandleLog = Arc::new(Mutex::new(Vec::new()));
        c.add_provider(Box::new(TrackingProvider::with_handle_log(
            "calc",
            vec!["="],
            Arc::new(Mutex::new(Vec::new())),
            Arc::clone(&prefixed),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::with_handle_log(
            "desk",
            vec![],
            Arc::new(Mutex::new(Vec::new())),
            Arc::clone(&unprefixed),
        )))
        .unwrap();

        let pre = c.preprocess_query("= fi");
        assert_eq!(pre.prefix.as_deref(), Some("="));
        c.handle("calc", "calc-entry", &pre);
        c.handle("desk", "desk-entry", &pre);

        assert_eq!(
            *prefixed.lock().unwrap(),
            vec![("calc-entry".to_string(), Some("=".into()), " fi".into())],
            "a provider whose prefix matched sees Some(prefix) and the stripped query"
        );
        assert_eq!(
            *unprefixed.lock().unwrap(),
            vec![("desk-entry".to_string(), None, "= fi".into())],
            "a provider whose prefixes don't contain the global prefix sees None and the full query"
        );
    }

    #[test]
    fn meta_overrides_applied_from_config() {
        let dir =
            std::env::temp_dir().join(format!("huffi-providers-override-{}", std::process::id()));
        let mut overrides = HashMap::new();
        overrides.insert(
            "calculator".to_string(),
            ProviderOverride {
                name: Some("Calc".to_string()),
                enabled: Some(false),
                prefixes: Some(vec!["::".to_string()]),
                prefix_only: None,
                extra: None,
            },
        );
        let config = ProviderConfig {
            builtin: overrides,
            exclusive_prefixes: Vec::new(),
            ..Default::default()
        };
        let c = ProviderCollection::new_with_config(dir, true, &config).unwrap();
        let providers = c.providers();
        let calc = providers.iter().find(|p| p.id == "calculator").unwrap();
        assert_eq!(calc.name, "Calc");
        assert!(!calc.enabled);
        assert_eq!(calc.prefixes, vec!["::"]);
    }

    #[test]
    fn provider_extra_config_is_passed_to_init() {
        let received: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));

        struct ExtraCapturingProvider {
            received: Arc<Mutex<Option<serde_json::Value>>>,
        }
        impl Provider for ExtraCapturingProvider {
            fn meta(&self) -> ProviderMeta {
                ProviderMeta::builder("extra-capture").build()
            }
            fn init(&mut self, ctx: InitContext) -> ProviderResult {
                *self.received.lock().unwrap() = ctx.extra.clone();
                ProviderResult::Ok
            }
            fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
                vec![]
            }
        }

        let dir =
            std::env::temp_dir().join(format!("huffi-providers-extra-{}", std::process::id()));
        let override_cfg = ProviderConfig {
            builtin: HashMap::from([(
                "extra-capture".to_string(),
                ProviderOverride {
                    name: None,
                    enabled: None,
                    prefixes: None,
                    prefix_only: None,
                    extra: Some(serde_json::json!({ "precision": 2 })),
                },
            )]),
            exclusive_prefixes: Vec::new(),
            ..Default::default()
        };
        let mut c = ProviderCollection::new_with_config(dir, true, &override_cfg).unwrap();
        c.add_provider(Box::new(ExtraCapturingProvider {
            received: Arc::clone(&received),
        }))
        .unwrap();
        assert_eq!(
            *received.lock().unwrap(),
            Some(serde_json::json!({ "precision": 2 }))
        );
    }

    #[test]
    fn critical_config_error_disables_provider() {
        struct FailingInit;
        impl Provider for FailingInit {
            fn meta(&self) -> ProviderMeta {
                ProviderMeta::builder("failing").build()
            }
            fn init(&mut self, _ctx: InitContext) -> ProviderResult {
                ProviderResult::Config {
                    msg: "bad config".into(),
                    critical: true,
                }
            }
            fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
                vec![]
            }
        }

        let dir =
            std::env::temp_dir().join(format!("huffi-providers-{}-critical", std::process::id()));
        let mut c =
            ProviderCollection::new_with_config(dir, true, &ProviderConfig::default()).unwrap();
        c.add_provider(Box::new(FailingInit)).unwrap();
        let providers = c.providers();
        assert!(!providers.iter().any(|p| p.name == "failing" && p.enabled));
    }

    #[test]
    fn init_skipped_for_provider_disabled_by_default() {
        let inits: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

        struct DisabledByDefault {
            inits: Arc<Mutex<usize>>,
        }
        impl Provider for DisabledByDefault {
            fn meta(&self) -> ProviderMeta {
                ProviderMeta::builder("disabled-by-default")
                    .enabled(false)
                    .build()
            }
            fn init(&mut self, _ctx: InitContext) -> ProviderResult {
                *self.inits.lock().unwrap() += 1;
                ProviderResult::Ok
            }
            fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
                vec![]
            }
        }

        let mut c = collection();
        c.add_provider(Box::new(DisabledByDefault {
            inits: Arc::clone(&inits),
        }))
        .unwrap();
        assert_eq!(
            *inits.lock().unwrap(),
            0,
            "init must not run for a provider disabled by its own meta"
        );
        assert!(
            !c.providers()
                .iter()
                .any(|p| p.id == "disabled-by-default" && p.enabled)
        );
    }

    #[test]
    fn init_skipped_for_provider_disabled_by_config() {
        let inits: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

        struct ConfigDisabled {
            inits: Arc<Mutex<usize>>,
        }
        impl Provider for ConfigDisabled {
            fn meta(&self) -> ProviderMeta {
                ProviderMeta::builder("config-disabled").build()
            }
            fn init(&mut self, _ctx: InitContext) -> ProviderResult {
                *self.inits.lock().unwrap() += 1;
                ProviderResult::Ok
            }
            fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
                vec![]
            }
        }

        let dir =
            std::env::temp_dir().join(format!("huffi-providers-disabled-{}", std::process::id()));
        let override_cfg = ProviderConfig {
            builtin: HashMap::from([(
                "config-disabled".to_string(),
                ProviderOverride {
                    name: None,
                    enabled: Some(false),
                    prefixes: None,
                    prefix_only: None,
                    extra: None,
                },
            )]),
            exclusive_prefixes: Vec::new(),
            ..Default::default()
        };
        let mut c = ProviderCollection::new_with_config(dir, true, &override_cfg).unwrap();
        c.add_provider(Box::new(ConfigDisabled {
            inits: Arc::clone(&inits),
        }))
        .unwrap();
        assert_eq!(
            *inits.lock().unwrap(),
            0,
            "init must not run for a provider disabled by user config"
        );
        assert!(
            !c.providers()
                .iter()
                .any(|p| p.id == "config-disabled" && p.enabled)
        );
    }

    #[test]
    fn prefix_only_provider_skipped_without_prefix() {
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));

        struct PrefixOnlyProvider {
            calls: CallLog,
        }
        impl Provider for PrefixOnlyProvider {
            fn meta(&self) -> ProviderMeta {
                ProviderMeta::builder("prefixed")
                    .prefix("::")
                    .prefix_only(true)
                    .build()
            }
            fn init(&mut self, _ctx: InitContext) -> ProviderResult {
                ProviderResult::Ok
            }
            fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
                self.calls.lock().unwrap().push((
                    "prefixed".to_string(),
                    ctx.prefix.map(String::from),
                    ctx.query.to_string(),
                ));
                vec![
                    entry("prefixed", "prefixed")
                        .history_key("prefixed")
                        .score(1.0),
                ]
            }
        }

        let mut c = collection();
        c.add_provider(Box::new(PrefixOnlyProvider {
            calls: Arc::clone(&calls),
        }))
        .unwrap();

        let _ = c.grouped_entries(&c.preprocess_query("firefox"));
        assert!(
            calls.lock().unwrap().is_empty(),
            "prefix_only provider must not be queried when its prefix is absent"
        );

        let _ = c.grouped_entries(&c.preprocess_query(":: 2"));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![("prefixed".to_string(), Some("::".into()), " 2".into())],
            "prefix_only provider is queried when its prefix matches"
        );
    }

    #[test]
    fn exclusive_prefix_skips_providers_not_declaring_it() {
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        let mut c = collection_with_exclusive(&["="]);
        c.add_provider(Box::new(TrackingProvider::new(
            "calc",
            vec!["="],
            Arc::clone(&calls),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "desk",
            vec![],
            Arc::clone(&calls),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "other",
            vec!["!"],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("= fi");
        assert_eq!(pre.prefix.as_deref(), Some("="));
        let _ = c.grouped_entries(&pre);

        let log = calls.lock().unwrap();
        assert_eq!(
            *log,
            vec![("calc".to_string(), Some("=".to_string()), " fi".to_string())],
            "only providers declaring the exclusive prefix are queried"
        );
    }

    #[test]
    fn exclusive_prefix_leaves_unprefixed_queries_shared() {
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        let mut c = collection_with_exclusive(&["="]);
        c.add_provider(Box::new(TrackingProvider::new(
            "calc",
            vec!["="],
            Arc::clone(&calls),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "desk",
            vec![],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("firefox");
        assert_eq!(pre.prefix, None);
        let _ = c.grouped_entries(&pre);

        let log = calls.lock().unwrap();
        assert_eq!(
            log.iter().map(|(id, _, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["calc", "desk"],
            "with no prefix active every provider shares the query"
        );
        assert!(
            log.iter()
                .all(|(_, prefix, query)| prefix.is_none() && query == "firefox")
        );
    }

    #[test]
    fn empty_exclusive_list_keeps_queries_shared() {
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        let mut c = collection();
        c.add_provider(Box::new(TrackingProvider::new(
            "calc",
            vec!["="],
            Arc::clone(&calls),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "desk",
            vec![],
            Arc::clone(&calls),
        )))
        .unwrap();

        let _ = c.grouped_entries(&c.preprocess_query("= fi"));

        let log = calls.lock().unwrap();
        assert_eq!(
            log.iter().map(|(id, _, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["calc", "desk"],
            "the default empty exclusive list changes nothing"
        );
    }

    #[test]
    fn exclusive_prefix_no_provider_declares_is_inert() {
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        let mut c = collection_with_exclusive(&["~"]);
        c.add_provider(Box::new(TrackingProvider::new(
            "desk",
            vec![],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("~fi");
        assert_eq!(
            pre.prefix, None,
            "no provider declares ~, so it never resolves as a global prefix"
        );
        let _ = c.grouped_entries(&pre);

        let log = calls.lock().unwrap();
        assert_eq!(
            *log,
            vec![("desk".to_string(), None, "~fi".to_string())],
            "an inert exclusive prefix leaves the query shared"
        );
    }

    #[test]
    fn exclusive_prefix_applies_to_the_resolved_prefix_not_a_shorter_one() {
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        let mut c = collection_with_exclusive(&["="]);
        c.add_provider(Box::new(TrackingProvider::new(
            "short",
            vec!["="],
            Arc::clone(&calls),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "long",
            vec!["=="],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("== 2");
        assert_eq!(pre.prefix.as_deref(), Some("=="), "longest prefix wins");
        let _ = c.grouped_entries(&pre);

        let log = calls.lock().unwrap();
        assert_eq!(
            log.iter().map(|(id, _, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["short", "long"],
            "the resolved prefix == is not exclusive, so everyone is queried"
        );
    }

    #[test]
    fn every_provider_sharing_the_exclusive_prefix_is_queried() {
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        let mut c = collection_with_exclusive(&["="]);
        for id in ["a", "b"] {
            c.add_provider(Box::new(TrackingProvider::new(
                id,
                vec!["="],
                Arc::clone(&calls),
            )))
            .unwrap();
        }

        let _ = c.grouped_entries(&c.preprocess_query("= fi"));

        let log = calls.lock().unwrap();
        assert_eq!(
            log.iter().map(|(id, _, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"],
            "exclusivity is per prefix, not per provider"
        );
    }

    #[test]
    fn disabled_providers_do_not_keep_the_query_for_themselves() {
        let calls: CallLog = Arc::new(Mutex::new(Vec::new()));
        // "!" is declared by a provider disabled by user config, so it never
        // resolves as a global prefix and the query stays shared.
        let dir =
            std::env::temp_dir().join(format!("huffi-providers-excl-dis-{}", std::process::id()));
        let config = ProviderConfig {
            builtin: HashMap::from([(
                "nix".to_string(),
                ProviderOverride {
                    name: None,
                    enabled: Some(false),
                    prefixes: None,
                    prefix_only: None,
                    extra: None,
                },
            )]),
            exclusive_prefixes: vec!["!".to_string()],
            ..Default::default()
        };
        let mut c = ProviderCollection::new_with_config(dir, true, &config).unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "desk",
            vec![],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("!firefox");
        assert_eq!(pre.prefix, None, "the only ! provider is disabled");
        let _ = c.grouped_entries(&pre);
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "no prefix resolved, so providers are not filtered"
        );
    }

    #[test]
    fn empty_meta_name_resolves_to_id() {
        struct NameLessProvider;
        impl Provider for NameLessProvider {
            fn meta(&self) -> ProviderMeta {
                ProviderMeta::builder("nameless").build()
            }
            fn init(&mut self, _ctx: InitContext) -> ProviderResult {
                ProviderResult::Ok
            }
            fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
                vec![]
            }
        }

        let mut c = collection();
        c.add_provider(Box::new(NameLessProvider)).unwrap();
        let providers = c.providers();
        assert!(
            providers
                .iter()
                .any(|p| p.id == "nameless" && p.name == "nameless"),
            "an empty meta name should fall back to the provider id"
        );
    }

    #[test]
    fn provider_with_empty_id_is_rejected() {
        struct IdLessProvider;
        impl Provider for IdLessProvider {
            fn meta(&self) -> ProviderMeta {
                ProviderMeta {
                    id: String::new(),
                    name: String::new(),
                    prefixes: Vec::new(),
                    enabled: true,
                    prefix_only: false,
                }
            }
            fn init(&mut self, _ctx: InitContext) -> ProviderResult {
                ProviderResult::Ok
            }
            fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
                vec![]
            }
        }

        let mut c = collection();
        assert!(
            c.add_provider(Box::new(IdLessProvider)).is_err(),
            "a provider returning an empty id should be refused at registration"
        );
    }

    #[test]
    fn config_overrides_prefix_only() {
        let dir = std::env::temp_dir().join(format!(
            "huffi-providers-prefix-only-{}",
            std::process::id()
        ));
        let override_cfg = ProviderConfig {
            builtin: HashMap::from([(
                "desktop".to_string(),
                ProviderOverride {
                    name: None,
                    enabled: None,
                    prefixes: None,
                    prefix_only: Some(true),
                    extra: None,
                },
            )]),
            exclusive_prefixes: Vec::new(),
            ..Default::default()
        };
        let c = ProviderCollection::new_with_config(dir, true, &override_cfg).unwrap();
        let providers = c.providers();
        let desktop = providers.iter().find(|p| p.id == "desktop").unwrap();
        assert!(desktop.prefix_only);
    }
}
