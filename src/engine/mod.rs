//! The huffi engine: providers plus scoring.
//!
//! [`Engine`] owns a [`ProviderCollection`](provider::ProviderCollection) and
//! the [`Scorer`](scoring::Scorer), exposing the querying, selection, and
//! history API the UI drives. The collection is responsible for provider
//! management and entry lookup; the scorer is responsible for fuzzy matching
//! and usage-history ranking. [`Engine`] wires the two together.

pub mod config;
pub mod provider;
pub mod scoring;

use std::fs;
use std::path::Path;

use crate::engine::config::{EngineConfig, ExternalConfig};
use crate::engine::provider::{
    EntryMeta, PreprocessedQuery, Provider, ProviderCollection, ProviderMeta,
};
use crate::engine::scoring::history::KeyedHistoryRecord;
use crate::engine::scoring::{Scored, Scorer};

/// The result of a query: the resolved [`PreprocessedQuery`] and the full
/// ranked result set.
///
/// [`Engine::query`] returns a read-only reference into the engine's
/// single-slot cache, so querying copies nothing; the UI clones only the
/// window of rows it renders.
#[derive(Debug, Clone)]
pub struct QueryReply {
    pub pre: PreprocessedQuery,
    pub scored: Vec<Scored<EntryMeta>>,
}

/// Owns the providers and the scorer, exposing querying, selection, and
/// history. Holds a single-slot cache of the most recent [`QueryReply`],
/// keyed by the query as typed and dropped on any history mutation so
/// scores always reflect live history.
pub struct Engine {
    providers: ProviderCollection,
    scorer: Scorer,
    dry_run: bool,
    external: ExternalConfig,
    cache: Option<QueryReply>,
}

impl Engine {
    pub fn new(data_dir: impl AsRef<Path>, dry_run: bool) -> anyhow::Result<Self> {
        Self::new_with_config(data_dir, dry_run, &EngineConfig::default())
    }

    /// Construct an engine from a resolved [`EngineConfig`]: the config drives
    /// history/scoring constants, provider settings, and the external
    /// binaries used when launching entries.
    ///
    /// `data_dir` is huffi's data folder: the history file lives inside it
    /// (see [`scoring::history::HISTORY_FILE`]) and each provider gets its
    /// own `<data_dir>/providers/<provider id>/` folder via
    /// [`Provider::init`]. Both are created unless running in dry-run mode.
    pub fn new_with_config(
        data_dir: impl AsRef<Path>,
        dry_run: bool,
        config: &EngineConfig,
    ) -> anyhow::Result<Self> {
        let data_dir = data_dir.as_ref();
        if !dry_run {
            fs::create_dir_all(data_dir)?;
        }
        Ok(Self {
            providers: ProviderCollection::new_with_config(data_dir, dry_run, &config.provider)?,
            scorer: Scorer::new_with_config(data_dir, dry_run, &config.scoring)?,
            dry_run,
            external: config.external.clone(),
            cache: None,
        })
    }

    pub fn add_provider(&mut self, provider: Box<dyn Provider>) -> anyhow::Result<()> {
        self.providers.add_provider(provider)
    }

    /// Query providers and score results, returning a read-only reference to
    /// the [`QueryReply`] in the engine's cache.
    ///
    /// Each provider is grouped with the query its entries should be
    /// fuzzy-matched against: the prefix-stripped query for the provider whose
    /// prefix matched, the original query for everyone else. All groups are
    /// normalized together. History is always looked up with the original
    /// query.
    ///
    /// Repeating the same query resolves instantly without re-running
    /// providers or scoring. The cache holds one reply, keyed by the query as
    /// typed; any history mutation drops it so scores always reflect live
    /// history.
    pub fn query(&mut self, query: &str) -> &QueryReply {
        if !self
            .cache
            .as_ref()
            .is_some_and(|c| c.pre.original_query == query)
        {
            let pre = self.providers.preprocess_query(query);
            let groups = self.providers.grouped_entries(&pre);
            let scored = self.scorer.score(groups, query);
            self.cache = Some(QueryReply { pre, scored });
        }
        self.cache.as_ref().expect("query always fills the cache")
    }

    /// Find an entry by ID within the current query's results, notify its
    /// provider via [`Provider::handle`], record its launch in history (if it
    /// carries a history key), then execute its action — unless in dry-run
    /// mode.
    ///
    /// Selection reuses the cached reply for the query, so it does not
    /// re-run providers or scoring; only the selected entry is copied out.
    pub fn select(&mut self, query: &str, entry_id: &str) {
        let (pre, hit) = {
            let reply = self.query(query);
            let hit = reply.scored.iter().find(|s| s.entry.id == entry_id);
            (reply.pre.clone(), hit.cloned())
        };
        let Some(hit) = hit else {
            eprintln!("[engine] select: no entry '{entry_id}' for query '{query}'");
            return;
        };
        self.cache = None;
        if !self.dry_run {
            hit.entry.action.perform(&self.external);
        }
        if let Some(provider_id) = &hit.entry.provider_id {
            self.providers.handle(provider_id, entry_id, &pre);
        }
        if let Some(key) = &hit.history_key {
            self.scorer.record_launch(query, key);
        }
    }

    /// Boost a history key's ranking using the scorer's configured boost weight.
    pub fn boost(&mut self, query: &str, history_key: &str) {
        self.cache = None;
        self.scorer.record_boost(query, history_key);
    }

    pub fn delete(&mut self, query: &str, history_key: &str) {
        self.cache = None;
        self.scorer.delete(query, history_key);
    }

    pub fn list_entries(&mut self, prefix: &str) -> Vec<KeyedHistoryRecord> {
        self.scorer.list_entries(prefix)
    }

    /// List registered providers and their trigger prefixes, for the UI.
    pub fn providers(&self) -> Vec<ProviderMeta> {
        self.providers.providers()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::engine::provider::{
        Entry, HandleContext, InitContext, ProviderMeta, ProviderResult, QueryContext,
        TestProvider, entry,
    };
    use crate::engine::scoring::MatchField;

    fn engine() -> Engine {
        Engine::new("/tmp/huffi-engine-test", true).unwrap()
    }

    fn match_fields_entry(id: &str, text: &str) -> Entry {
        entry(id, id).history_key(id).match_fields(vec![MatchField {
            text: text.into(),
            weight: 1.0,
        }])
    }

    struct CountingProvider {
        id: String,
        entries: Vec<Entry>,
        calls: Arc<Mutex<usize>>,
        handled: Arc<Mutex<Vec<String>>>,
    }

    impl Provider for CountingProvider {
        fn meta(&self) -> ProviderMeta {
            ProviderMeta {
                id: self.id.clone(),
                ..Default::default()
            }
        }
        fn init(&mut self, _ctx: InitContext) -> ProviderResult {
            ProviderResult::Ok
        }
        fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
            *self.calls.lock().unwrap() += 1;
            self.entries.clone()
        }
        fn handle(&mut self, ctx: HandleContext) {
            self.handled.lock().unwrap().push(ctx.entry_id.to_string());
        }
    }

    fn counting_provider(
        id: &str,
        calls: &Arc<Mutex<usize>>,
    ) -> (Box<dyn Provider>, Arc<Mutex<Vec<String>>>) {
        let handled = Arc::new(Mutex::new(Vec::new()));
        (
            Box::new(CountingProvider {
                id: id.into(),
                entries: vec![match_fields_entry("alpha", "alpha")],
                calls: Arc::clone(calls),
                handled: Arc::clone(&handled),
            }),
            handled,
        )
    }

    #[test]
    fn prefixed_entries_scored_against_stripped_query() {
        let mut e = engine();
        e.add_provider(Box::new(TestProvider::with_prefixes(
            "pfx",
            vec!["=="],
            vec![match_fields_entry("pfx", "Firefox")],
        )))
        .unwrap();
        e.add_provider(Box::new(TestProvider::new(
            "other",
            vec![match_fields_entry("other", "Firefox")],
        )))
        .unwrap();

        let reply = e.query("==fi");

        assert_eq!(reply.pre.prefix.as_deref(), Some("=="));
        assert!(
            reply.scored.iter().any(|s| s.entry.id == "pfx"),
            "prefixed provider entry should fuzzy-match the stripped query"
        );
        assert!(
            !reply.scored.iter().any(|s| s.entry.id == "other"),
            "unprefixed provider entry should not fuzzy-match the original query"
        );
        assert_eq!(reply.scored[0].entry.provider_id.as_deref(), Some("pfx"));
    }

    #[test]
    fn select_records_launch_and_boosts_ranking() {
        let mut e = engine();
        e.add_provider(Box::new(TestProvider::with_prefixes(
            "pfx",
            vec!["::"],
            vec![match_fields_entry("firefox", "Firefox")],
        )))
        .unwrap();

        for _ in 0..5 {
            e.select("::fire", "firefox");
        }

        {
            let reply = e.query("::fire");
            assert_eq!(reply.pre.prefix.as_deref(), Some("::"));
            assert_eq!(reply.scored[0].entry.id, "firefox");
            assert!(
                reply.scored[0].history_score.is_some_and(|h| h > 0.0),
                "select should feed usage history into ranking"
            );
        }

        let history = e.list_entries("::fire");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].key, "firefox");
        assert_eq!(history[0].record.n, 5);
    }

    #[test]
    fn boost_moves_entry_to_top() {
        let mut e = engine();
        e.add_provider(Box::new(TestProvider::new(
            "desktop",
            vec![
                match_fields_entry("firefox", "Firefox"),
                match_fields_entry("files", "Files"),
            ],
        )))
        .unwrap();

        for _ in 0..10 {
            e.boost("fi", "files");
        }

        let scored = &e.query("fi").scored;
        assert_eq!(scored[0].history_key.as_deref(), Some("files"));
    }

    #[test]
    fn configured_scoring_constants_are_used() {
        let mut config = EngineConfig::default();
        config.scoring.boost_weight = 2.0;
        config.scoring.boost_samples = 2;
        config.scoring.empty_query_score = 0.25;

        let mut e = Engine::new_with_config("/tmp/huffi-engine-config", true, &config).unwrap();
        e.add_provider(Box::new(TestProvider::with_prefixes(
            "test",
            vec!["~~"],
            vec![
                match_fields_entry("a", "A"),
                match_fields_entry("b", "B"),
                match_fields_entry("c", "C"),
            ],
        )))
        .unwrap();

        // empty_query_score shows up in the base score for an empty query.
        let scored = &e.query("~~").scored;
        assert!(
            scored.iter().all(|h| (h.base_score - 0.25).abs() < 1e-9),
            "empty-query base score should use the configured value"
        );

        // boost_weight and boost_samples are honored: 4 boosts at weight 2.0
        // add 8.0 to the effective score, and 4 boosts at 2 samples each bump
        // the sample count `n` by 8.
        for _ in 0..4 {
            e.boost("b", "b");
        }
        let entries = e.list_entries("b");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].record.n, 8);

        let _ = std::fs::remove_dir_all("/tmp/huffi-engine-config");
    }

    #[test]
    fn select_invokes_provider_handle() {
        use crate::engine::provider::{
            HandleContext, InitContext, ProviderMeta, ProviderResult, QueryContext,
        };
        use std::sync::{Arc, Mutex};

        struct HandleTrackingProvider {
            id: String,
            entries: Vec<Entry>,
            handled: Arc<Mutex<Vec<String>>>,
        }

        impl Provider for HandleTrackingProvider {
            fn meta(&self) -> ProviderMeta {
                ProviderMeta {
                    id: self.id.clone(),
                    ..Default::default()
                }
            }
            fn init(&mut self, _ctx: InitContext) -> ProviderResult {
                ProviderResult::Ok
            }
            fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
                self.entries.clone()
            }
            fn handle(&mut self, ctx: HandleContext) {
                self.handled.lock().unwrap().push(ctx.entry_id.to_string());
            }
        }

        let mut e = engine();
        let handled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        e.add_provider(Box::new(HandleTrackingProvider {
            id: "h".into(),
            entries: vec![match_fields_entry("h-entry", "hello")],
            handled: Arc::clone(&handled),
        }))
        .unwrap();

        e.select("hello", "h-entry");

        assert_eq!(*handled.lock().unwrap(), vec!["h-entry".to_string()]);
    }

    #[test]
    fn query_serves_repeated_inputs_from_cache() {
        let mut e = engine();
        let calls = Arc::new(Mutex::new(0_usize));
        let (provider, _) = counting_provider("c", &calls);
        e.add_provider(provider).unwrap();

        let first_len = e.query("al").scored.len();
        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "first query should hit providers"
        );

        let second = e.query("al");
        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "repeating the same query should be served from the cache"
        );
        assert_eq!(second.pre.original_query, "al");
        assert_eq!(second.scored.len(), first_len);

        e.query("alx");
        assert_eq!(
            *calls.lock().unwrap(),
            2,
            "a different query should recompute"
        );
    }

    #[test]
    fn cache_invalidated_by_history_mutations() {
        let mut e = engine();
        let calls = Arc::new(Mutex::new(0_usize));
        let (provider, _) = counting_provider("c", &calls);
        e.add_provider(provider).unwrap();

        e.query("al");
        e.boost("al", "alpha");
        e.query("al");
        assert_eq!(
            *calls.lock().unwrap(),
            2,
            "boost should invalidate the cache"
        );

        e.delete("al", "alpha");
        e.query("al");
        assert_eq!(
            *calls.lock().unwrap(),
            3,
            "delete should invalidate the cache"
        );
    }

    #[test]
    fn select_uses_cached_query() {
        let mut e = engine();
        let calls = Arc::new(Mutex::new(0_usize));
        let (provider, handled) = counting_provider("c", &calls);
        e.add_provider(provider).unwrap();

        assert!(
            e.query("al").scored.iter().any(|s| s.entry.id == "alpha"),
            "precondition: entry is in the result set"
        );

        e.select("al", "alpha");

        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "select should reuse the cached query instead of re-running providers"
        );
        assert_eq!(*handled.lock().unwrap(), vec!["alpha".to_string()]);
        let entries = e.list_entries("al");
        assert!(
            entries.iter().any(|r| r.key == "alpha"),
            "select should record a launch in history"
        );
    }

    #[test]
    fn select_unknown_entry_is_noop() {
        let mut e = engine();
        let calls = Arc::new(Mutex::new(0_usize));
        let (provider, handled) = counting_provider("c", &calls);
        e.add_provider(provider).unwrap();

        e.select("al", "missing");

        assert!(handled.lock().unwrap().is_empty());
        assert!(e.list_entries("al").is_empty());
        assert_eq!(
            *calls.lock().unwrap(),
            1,
            "a cold select resolves the query once but does not double-query"
        );
    }

    #[test]
    fn providers_lists_entries() {
        let e = engine();
        let providers = e.providers();
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
}
