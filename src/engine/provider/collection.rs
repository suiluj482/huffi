use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;

use crate::engine::scoring::QueryGroup;

use super::config::{ProviderConfig, ProviderOverride};
use super::{
    CalculatorProvider, DesktopEntryProvider, EntryMeta, HandleContext, InitContext,
    NixRunProvider, Provider, ProviderMeta, ProviderResult, QueryContext,
};

pub struct ProviderCollection {
    /// Registered providers in insertion order, with the metadata used for
    /// them (the `enabled` flag may be flipped by a failed init).
    providers: Vec<(Box<dyn Provider>, ProviderMeta)>,
    /// User-config overrides keyed by provider id.
    overrides: HashMap<String, ProviderOverride>,
    /// Huffi's data folder; each provider gets `data_dir/providers/<id>/`.
    data_dir: std::path::PathBuf,
    /// Skip creating on-disk state when true.
    dry_run: bool,
}

/// The result of resolving the global prefix for a query.
///
/// The longest declared provider prefix that the query starts with wins;
/// there is at most one active prefix per query.
#[derive(Debug, Clone)]
pub struct PreprocessedQuery {
    pub original_query: String,
    pub prefix: Option<String>,
    pub query: String,
}

impl ProviderCollection {
    /// Construct the collection with the built-in providers configured from
    /// `config`. Each provider is registered with `add_provider`, which
    /// provisions its `<data_dir>/providers/<provider id>/` folder (when
    /// not in dry-run mode) and calls its [`init`](Provider::init) — unless
    /// the provider is disabled by its own default or by user config.
    pub fn new_with_config(
        data_dir: impl AsRef<Path>,
        dry_run: bool,
        config: &ProviderConfig,
    ) -> anyhow::Result<Self> {
        let mut collection = Self {
            providers: Vec::new(),
            overrides: config.builtin.clone(),
            data_dir: data_dir.as_ref().to_path_buf(),
            dry_run,
        };
        collection.add_provider(Box::new(DesktopEntryProvider::new(
            freedesktop_desktop_entry::default_paths().collect(),
        )))?;
        collection.add_provider(Box::new(CalculatorProvider::new()))?;
        collection.add_provider(Box::new(NixRunProvider::new()))?;
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
    /// Matches the longest declared provider prefix that the query starts
    /// with. If several prefixes are tied in length, the first declared wins.
    pub fn preprocess_query(&self, query: &str) -> PreprocessedQuery {
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
            Some(prefix) => PreprocessedQuery {
                original_query: query.to_string(),
                prefix: Some(prefix.clone()),
                query: query[prefix.len()..].to_string(),
            },
            None => PreprocessedQuery {
                original_query: query.to_string(),
                prefix: None,
                query: query.to_string(),
            },
        }
    }

    /// The provider-relative [`QueryContext`] for `meta`: prefix kept and
    /// text stripped when this provider declared the query's global prefix,
    /// otherwise prefix dropped and the full query kept.
    fn provider_query_context<'a>(
        meta: &ProviderMeta,
        pre: &'a PreprocessedQuery,
    ) -> QueryContext<'a> {
        if pre
            .prefix
            .as_deref()
            .is_some_and(|pfx| meta.prefixes.iter().any(|m| m == pfx))
        {
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
    /// Scoring itself is owned by [`crate::engine::Engine`]; export the raw
    /// groups here so the engine can hand them to the
    /// [`Scorer`](crate::engine::scoring::Scorer).
    pub(crate) fn grouped_entries(
        &mut self,
        pre: &PreprocessedQuery,
    ) -> Vec<QueryGroup<EntryMeta>> {
        self.providers
            .iter_mut()
            .filter_map(|(p, meta)| {
                if !meta.enabled {
                    return None;
                }
                let ctx = Self::provider_query_context(meta, pre);
                if meta.prefix_only && ctx.prefix.is_none() {
                    return None;
                }
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
    /// provider-relative: `prefix` is `Some` only when the global prefix is
    /// in the provider's own prefix list. No-op when the provider is no
    /// longer registered.
    pub fn handle(&mut self, provider_id: &str, entry_id: &str, pre: &PreprocessedQuery) {
        for (provider, meta) in &mut self.providers {
            if meta.id != provider_id {
                continue;
            }
            provider.handle(HandleContext {
                entry_id,
                query: Self::provider_query_context(meta, pre),
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
        assert!(
            providers.iter().any(|p| {
                p.id == "calculator" && p.name == "calculator" && p.prefixes == vec!["="]
            })
        );
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
        let dir = std::env::temp_dir().join(format!(
            "huffi-providers-override-{}",
            std::process::id()
        ));
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
        let config = ProviderConfig { builtin: overrides };
        let c = ProviderCollection::new_with_config(dir, true, &config).unwrap();
        let providers = c.providers();
        let calc = providers.iter().find(|p| p.id == "calculator").unwrap();
        assert_eq!(calc.name, "Calc");
        assert!(!calc.enabled);
        assert_eq!(calc.prefixes, vec!["::"]);
    }

    #[test]
    fn provider_extra_config_is_passed_to_init() {
        let received: Arc<Mutex<Option<serde_json::Value>>> =
            Arc::new(Mutex::new(None));

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

        let dir = std::env::temp_dir().join(format!("huffi-providers-extra-{}", std::process::id()));
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

        let dir = std::env::temp_dir().join(format!("huffi-providers-{}-critical", std::process::id()));
        let mut c = ProviderCollection::new_with_config(dir, true, &ProviderConfig::default()).unwrap();
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

        let dir = std::env::temp_dir().join(format!(
            "huffi-providers-disabled-{}",
            std::process::id()
        ));
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
                vec![entry("prefixed", "prefixed").history_key("prefixed").score(1.0)]
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
        };
        let c = ProviderCollection::new_with_config(dir, true, &override_cfg).unwrap();
        let providers = c.providers();
        let desktop = providers.iter().find(|p| p.id == "desktop").unwrap();
        assert!(desktop.prefix_only);
    }
}
