use std::path::Path;

use anyhow::Context;

use crate::engine::scoring::QueryGroup;

use super::config::ProviderConfig;
use super::{
    CalculatorProvider, DesktopEntryProvider, EntryMeta, HandleContext, InitContext, Provider,
    ProviderMeta, ProviderResult, QueryContext,
};

pub struct ProviderCollection {
    /// Registered providers in insertion order, with the metadata used for
    /// them (the `enabled` flag may be flipped by a failed init).
    providers: Vec<(Box<dyn Provider>, ProviderMeta)>,
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
    /// not in dry-run mode) and calls its [`init`](Provider::init).
    pub fn new_with_config(
        data_dir: impl AsRef<Path>,
        dry_run: bool,
        config: &ProviderConfig,
    ) -> anyhow::Result<Self> {
        let mut collection = Self {
            providers: Vec::new(),
            data_dir: data_dir.as_ref().to_path_buf(),
            dry_run,
        };
        collection.add_provider(Box::new(DesktopEntryProvider::new(
            freedesktop_desktop_entry::default_paths().collect(),
            config.desktop,
        )))?;
        collection.add_provider(Box::new(CalculatorProvider::new()))?;
        Ok(collection)
    }
}

impl ProviderCollection {
    pub fn add_provider(&mut self, mut provider: Box<dyn Provider>) -> anyhow::Result<()> {
        let mut meta = provider.meta();
        let dir = self.data_dir.join("providers").join(&meta.id);
        if !self.dry_run {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create data dir {}", dir.display()))?;
        }
        match provider.init(InitContext { data_dir: &dir }) {
            ProviderResult::Ok => {}
            ProviderResult::Unsupported(msg) => {
                eprintln!("[provider] {} unsupported: {msg}", meta.id);
                meta.enabled = false;
            }
            ProviderResult::Other(msg) => {
                eprintln!("[provider] {} disabled: {msg}", meta.id);
                meta.enabled = false;
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
            ProviderMeta {
                id: self.id.clone(),
                prefixes: self.prefixes.iter().map(|s| (*s).to_string()).collect(),
                enabled: true,
            }
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
                .any(|p| p.id == "desktop" && p.prefixes.is_empty())
        );
        assert!(
            providers
                .iter()
                .any(|p| p.id == "calculator" && p.prefixes == vec!["="])
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
}
