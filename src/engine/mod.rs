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
    EntryMeta, Generation, PreprocessedQuery, Provider, ProviderCollection, ProviderMeta,
};
use crate::engine::scoring::history::{KeyedHistoryRecord, QueryHistory};
use crate::engine::scoring::{Scored, Scorer};

/// The result of a query: the resolved [`PreprocessedQuery`], the
/// precomputed [`QueryHistory`] for it, and the full ranked result set.
///
/// Like [`pre`](Self::pre), the history is built once before providers run
/// and reused everywhere the query is revisited — by providers in their
/// `QueryContext`, by the scorer's final blend, and by [`Engine::select`]
/// when it notifies a provider of a selection.
///
/// [`Engine::query`] returns a read-only reference into the engine's
/// single-slot cache, so querying copies nothing; the UI clones only the
/// window of rows it renders.
#[derive(Debug, Clone)]
pub struct QueryReply {
    pub pre: PreprocessedQuery,
    pub history: QueryHistory,
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
    generation: Generation,
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
            generation: Generation::default(),
        })
    }

    /// Start a new [`Generation::session`]: the launcher has been revealed
    /// for a round of typing, so providers caching against
    /// `generation.session` reload on the next query.
    ///
    /// Drops the query cache so the session's first query always re-runs
    /// providers — a prefilled query that repeats the previous session's
    /// last string must still let providers observe the new session.
    ///
    /// The UI calls this each time it shows the window; the engine itself
    /// never bumps the session counter.
    pub fn begin_session(&mut self) {
        self.generation.session += 1;
        self.cache = None;
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
    /// query, precomputed once, and the same [`QueryHistory`] flows into
    /// every provider's `QueryContext` and into the final blend.
    ///
    /// Repeating the same query resolves instantly without re-running
    /// providers or scoring. The cache holds one reply, keyed by the query as
    /// typed; any history mutation drops it so scores always reflect live
    /// history. Each run that misses the cache bumps
    /// [`Generation::query`](crate::engine::provider::Generation::query).
    pub fn query(&mut self, query: &str) -> &QueryReply {
        if !self
            .cache
            .as_ref()
            .is_some_and(|c| c.pre.original_query == query)
        {
            self.generation.query += 1;
            let pre = self.providers.preprocess_query(query);
            let history = self.scorer.history_for(query);
            let groups = self
                .providers
                .grouped_entries(&pre, &history, self.generation);
            let scored = self.scorer.score(groups, &history);
            self.cache = Some(QueryReply {
                pre,
                history,
                scored,
            });
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
        let (pre, history, hit) = {
            let reply = self.query(query);
            let hit = reply.scored.iter().find(|s| s.entry.id == entry_id);
            (reply.pre.clone(), reply.history.clone(), hit.cloned())
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
            self.providers
                .handle(provider_id, entry_id, &pre, &history, self.generation);
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

    /// The delimiter introducing a provider target, for the UI.
    pub fn target_prefix(&self) -> &str {
        self.providers.target_prefix()
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
            ProviderMeta::builder(&self.id).build()
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

    /// What a provider saw in `QueryContext`.
    #[derive(Debug, Clone, PartialEq)]
    struct HistoryView {
        keys: Vec<String>,
        confidence: f64,
        alpha_score: f64,
        generation: Generation,
    }

    fn history_view(ctx: &QueryContext) -> HistoryView {
        HistoryView {
            keys: ctx.history.keys().cloned().collect(),
            confidence: ctx.history.confidence(),
            alpha_score: ctx
                .history
                .get("alpha")
                .map(|entry| entry.score)
                .unwrap_or(0.0),
            generation: ctx.generation,
        }
    }

    /// The view on the engine's first provider run: nothing recorded yet,
    /// and `generation.query` already bumped for the run in progress.
    fn cold_first_run() -> HistoryView {
        HistoryView {
            keys: Vec::new(),
            confidence: 0.0,
            alpha_score: 0.0,
            generation: Generation {
                session: 0,
                query: 1,
            },
        }
    }

    /// Captures the history view seen in the `query` and `handle` contexts.
    struct HistoryCapturingProvider {
        prefixes: Vec<&'static str>,
        entries: Vec<Entry>,
        queried: HistoryLog,
        handled: HistoryLog,
    }

    impl Provider for HistoryCapturingProvider {
        fn meta(&self) -> ProviderMeta {
            ProviderMeta::builder("history-capture")
                .prefixes(self.prefixes.iter().copied())
                .build()
        }
        fn init(&mut self, _ctx: InitContext) -> ProviderResult {
            ProviderResult::Ok
        }
        fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
            self.queried.lock().unwrap().push(history_view(&ctx));
            self.entries.clone()
        }
        fn handle(&mut self, ctx: HandleContext) {
            self.handled.lock().unwrap().push(history_view(&ctx.query));
        }
    }

    type HistoryLog = Arc<Mutex<Vec<HistoryView>>>;

    fn history_capturing_provider(
        prefixes: Vec<&'static str>,
    ) -> (Box<dyn Provider>, HistoryLog, HistoryLog) {
        let queried = Arc::new(Mutex::new(Vec::new()));
        let handled = Arc::new(Mutex::new(Vec::new()));
        (
            Box::new(HistoryCapturingProvider {
                prefixes,
                entries: vec![match_fields_entry("alpha", "alpha")],
                queried: Arc::clone(&queried),
                handled: Arc::clone(&handled),
            }),
            queried,
            handled,
        )
    }

    #[test]
    fn providers_receive_history_for_the_original_query() {
        let mut e = engine();
        let (provider, queried, handled) = history_capturing_provider(vec![]);
        e.add_provider(provider).unwrap();

        e.query("al");
        assert_eq!(
            *queried.lock().unwrap(),
            vec![cold_first_run()],
            "a cold query has no recorded history"
        );

        e.select("al", "alpha");
        assert_eq!(
            *handled.lock().unwrap(),
            vec![cold_first_run()],
            "handle runs before the selection is recorded in history"
        );

        e.query("al");
        {
            let log = queried.lock().unwrap();
            let view = log.last().unwrap();
            assert_eq!(
                view.keys,
                vec!["alpha".to_string()],
                "the key recorded by the selection is delivered to query"
            );
            assert!(
                view.confidence > 0.0,
                "the recorded launch raises confidence"
            );
            assert_eq!(view.alpha_score, 1.0, "the only recorded key is the peak");
        }

        let after_query = queried.lock().unwrap().last().unwrap().clone();
        e.select("al", "alpha");
        assert_eq!(
            handled.lock().unwrap().last().unwrap(),
            &after_query,
            "handle receives the same history the query call saw"
        );
    }

    #[test]
    fn history_is_keyed_on_the_original_query_not_the_stripped_one() {
        let mut e = engine();
        let (provider, queried, _) = history_capturing_provider(vec!["=="]);
        e.add_provider(provider).unwrap();

        e.select("==al", "alpha");

        assert!(
            e.scorer.history_for("al").is_empty(),
            "precondition: fan-out records only prefixes of `==al`, not the stripped text"
        );

        e.query("==al");
        let log = queried.lock().unwrap();
        let view = log.last().unwrap();
        assert_eq!(
            view.keys,
            vec!["alpha".to_string()],
            "the provider sees history looked up under the original query, even though its own query text is stripped"
        );
        assert_eq!(view.alpha_score, 1.0);
    }

    #[test]
    fn generation_tracks_sessions_and_provider_runs() {
        let mut e = engine();
        let (provider, queried, _) = history_capturing_provider(vec![]);
        e.add_provider(provider).unwrap();

        assert_eq!(e.generation, Generation::default(), "starts at 0/0");

        e.query("al");
        assert_eq!(
            queried.lock().unwrap().last().unwrap().generation,
            Generation {
                session: 0,
                query: 1
            },
            "each provider run sees the bumped query generation"
        );

        e.query("al");
        assert_eq!(
            e.generation.query, 1,
            "a repeated query is a cache hit, not a provider run"
        );

        e.query("b");
        assert_eq!(
            queried.lock().unwrap().last().unwrap().generation,
            Generation {
                session: 0,
                query: 2
            },
            "a new keystroke runs providers under the next generation"
        );

        e.begin_session();
        assert_eq!(
            e.generation,
            Generation {
                session: 1,
                query: 2
            },
            "begin_session bumps only the session counter"
        );

        e.query("b");
        assert_eq!(
            queried.lock().unwrap().last().unwrap().generation,
            Generation {
                session: 1,
                query: 3
            },
            "the next run carries the new session"
        );
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
                ProviderMeta::builder(&self.id).build()
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
    fn target_prefix_scopes_to_one_provider_and_lists_providers() {
        use crate::engine::provider::ProvidersProvider;

        let mut e = engine();
        e.add_provider(Box::new(TestProvider::new(
            "desktop",
            vec![match_fields_entry("firefox", "Firefox")],
        )))
        .unwrap();
        e.add_provider(Box::new(TestProvider::with_prefixes(
            "calc",
            vec!["="],
            vec![match_fields_entry("sum", "2 + 2 = 4")],
        )))
        .unwrap();
        let target_prefix = e.target_prefix().to_string();
        let snapshot = e.providers();
        e.add_provider(Box::new(ProvidersProvider::new(snapshot, target_prefix)))
            .unwrap();

        let reply = e.query("\\calc 2 + 2");
        assert_eq!(reply.pre.prefix.as_deref(), Some("\\calc "));
        assert!(
            reply
                .scored
                .iter()
                .all(|s| s.entry.provider_id.as_deref() == Some("calc")),
            "a targeted query only returns the targeted provider's entries"
        );

        let reply = e.query("\\");
        assert_eq!(reply.pre.prefix.as_deref(), Some("\\"));
        assert!(
            reply.scored.iter().any(|s| s.entry.id == "provider-calc"),
            "the bare prefix lists providers"
        );
        assert!(
            reply
                .scored
                .iter()
                .all(|s| s.entry.provider_id.as_deref() == Some("providers")),
            "the listing prefix is exclusive"
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
        assert!(providers.iter().any(|p| {
            p.id == "calculator" && p.name == "calculator" && p.prefixes == vec!["="]
        }));
    }

    /// Profiling harness (run: `cargo test --release profile_engine -- --ignored
    /// --nocapture`) against the real on-disk nix index. Times the full
    /// Engine::query path per keystroke, split into provider grouping
    /// (clones + stamping) vs. scoring (fuzzy + history + sort).
    #[test]
    #[ignore]
    fn profile_engine_query() {
        use std::time::{Duration, Instant};

        let data_dir = std::env::var("HUFFI_PROFILE_DATA")
            .unwrap_or_else(|_| format!("{}/.local/share/huffi", std::env::var("HOME").unwrap()));
        let mut engine = Engine::new(&data_dir, false).unwrap();

        let mut ready = false;
        for i in 0..200 {
            let q = format!("!{}", (b'a' + (i % 26) as u8) as char);
            if !engine.query(&q).scored.is_empty() {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(ready, "nix index never populated");
        eprintln!("[prof] nix index ready");

        for q in ["!f", "!fi", "!fire", "!firefox", "!gimp"] {
            let t = Instant::now();
            let n = engine.query(q).scored.len();
            let total = t.elapsed();

            let pre = engine.providers.preprocess_query(q);
            let history = engine.scorer.history_for(q);
            let t = Instant::now();
            let groups = engine
                .providers
                .grouped_entries(&pre, &history, engine.generation);
            let group_dur = t.elapsed();
            let t = Instant::now();
            let scored = engine.scorer.score(groups, &history);
            let score_dur = t.elapsed();

            assert_eq!(scored.len(), n);
            eprintln!(
                "[prof] query '{q}': total {:?} = group {:?} + score {:?} ({} results)",
                total, group_dur, score_dur, n
            );
        }
    }
}
