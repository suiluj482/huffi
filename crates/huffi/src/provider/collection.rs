use std::path::Path;

use crate::scoring::history::KeyedHistoryRecord;
use crate::scoring::{QueryGroup, Scored, Scorer};

use super::{CalculatorProvider, DesktopEntryProvider, Entry, EntryMeta, Provider};

pub struct ProviderCollection {
    providers: Vec<Box<dyn Provider>>,
    scorer: Scorer,
    dry_run: bool,
}

/// A registered provider and its trigger prefixes.
pub struct ProviderInfo {
    pub id: String,
    pub prefixes: Vec<String>,
}

/// The result of resolving the global prefix for a query.
///
/// The longest declared provider prefix that the query starts with wins;
/// there is at most one active prefix per query.
pub struct PreprocessedQuery {
    pub original_query: String,
    pub prefix: Option<String>,
    pub query: String,
}

impl ProviderCollection {
    pub fn new(data_path: impl AsRef<Path>, dry_run: bool) -> anyhow::Result<Self> {
        let scorer = Scorer::open(data_path, dry_run)?;
        let mut providers: Vec<Box<dyn Provider>> = vec![
            Box::new(DesktopEntryProvider::default()),
            Box::new(CalculatorProvider::new()),
        ];
        for p in providers.iter_mut() {
            p.init();
        }
        Ok(Self { providers, scorer, dry_run })
    }

    pub fn add_provider(&mut self, mut provider: Box<dyn Provider>) {
        provider.init();
        self.providers.push(provider);
    }

    /// Query providers and score results.
    ///
    /// Each provider is grouped with the query its entries should be
    /// fuzzy-matched against: the prefix-stripped query for the provider whose
    /// prefix matched, the original query for everyone else. All groups are
    /// normalized together. History is always looked up with the original
    /// query.
    ///
    /// Returns the resolved global prefix (if any) alongside the scored entries.
    pub fn query(&mut self, query: &str) -> (Option<String>, Vec<Scored<EntryMeta>>) {
        let pre = self.preprocess_query(query);
        let groups = self.grouped_entries(&pre);
        let scored = self.scorer.score(groups, query);
        (pre.prefix, scored)
    }

    /// Resolve the global prefix for a query.
    ///
    /// Matches the longest declared provider prefix that the query starts
    /// with. If several prefixes are tied in length, the first declared wins.
    pub fn preprocess_query(&self, query: &str) -> PreprocessedQuery {
        let mut longest: Option<&str> = None;
        for provider in &self.providers {
            for prefix in provider.prefixes() {
                if !prefix.is_empty()
                    && query.starts_with(prefix)
                    && longest.is_none_or(|current| prefix.len() > current.len())
                {
                    longest = Some(prefix);
                }
            }
        }

        match longest {
            Some(prefix) => PreprocessedQuery {
                original_query: query.to_string(),
                prefix: Some(prefix.to_string()),
                query: query[prefix.len()..].to_string(),
            },
            None => PreprocessedQuery {
                original_query: query.to_string(),
                prefix: None,
                query: query.to_string(),
            },
        }
    }

    /// Query providers without scoring (raw entries).
    pub fn entries(&mut self, pre: &PreprocessedQuery) -> Vec<Entry> {
        self.grouped_entries(pre)
            .into_iter()
            .flat_map(|g| g.entries)
            .collect()
    }

    /// Query each provider and group its entries with the query they should
    /// be fuzzy-scored against. Entries are annotated with their provider id.
    fn grouped_entries(&mut self, pre: &PreprocessedQuery) -> Vec<QueryGroup<EntryMeta>> {
        self.providers
            .iter_mut()
            .map(|p| {
                let matched = pre
                    .prefix
                    .as_deref()
                    .is_some_and(|pfx| p.prefixes().contains(&pfx));
                let (prefix, query) = if matched {
                    (pre.prefix.as_deref(), &pre.query)
                } else {
                    (None, &pre.original_query)
                };
                let mut entries = p.query(prefix, query);
                for e in entries.iter_mut() {
                    e.entry.provider_id = Some(p.id().to_string());
                }
                QueryGroup {
                    query: query.to_string(),
                    entries,
                }
            })
            .collect()
    }

    /// Find an entry by ID and perform a select: record the launch in history,
    /// then execute the entry's action (unless in dry-run mode).
    pub fn select(&mut self, query: &str, entry_id: &str) {
        let pre = self.preprocess_query(query);
        let entries = self.entries(&pre);

        if let Some(key) = entries
            .iter()
            .find(|s| s.entry.id == entry_id)
            .and_then(|s| s.history_key.clone())
        {
            self.scorer.record_launch(query, &key);
        }

        if !self.dry_run
            && let Some(s) = entries.iter().find(|s| s.entry.id == entry_id)
        {
            s.entry.action.perform();
        }
    }

    pub fn record_launch(&mut self, query: &str, history_key: &str) {
        self.scorer.record_launch(query, history_key);
    }

    pub fn record_boost(&mut self, query: &str, history_key: &str, weight: f64) {
        self.scorer.record_boost(query, history_key, weight);
    }

    pub fn delete(&mut self, query: &str, history_key: &str) {
        self.scorer.delete(query, history_key);
    }

    pub fn list_entries(&mut self, prefix: &str) -> Vec<KeyedHistoryRecord> {
        self.scorer.list_entries(prefix)
    }

    /// List registered providers and their trigger prefixes.
    pub fn providers(&self) -> Vec<ProviderInfo> {
        self.providers
            .iter()
            .map(|p| ProviderInfo {
                id: p.id().into(),
                prefixes: p.prefixes().iter().map(|s| s.to_string()).collect(),
            })
            .collect()
    }

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
    use crate::provider::{entry, Provider, TestProvider};
    use crate::scoring::MatchField;

    type CallLog = Arc<Mutex<Vec<(String, Option<String>, String)>>>;

    fn collection() -> ProviderCollection {
        ProviderCollection::new("/tmp/huffi-test-preprocess.json", true).unwrap()
    }

    struct TrackingProvider {
        id: String,
        prefixes: Vec<&'static str>,
        calls: CallLog,
    }

    impl TrackingProvider {
        fn new(id: &str, prefixes: Vec<&'static str>, calls: CallLog) -> Self {
            Self { id: id.into(), prefixes, calls }
        }
    }

    impl Provider for TrackingProvider {
        fn id(&self) -> &str {
            &self.id
        }

        fn prefixes(&self) -> &[&str] {
            &self.prefixes
        }

        fn init(&mut self) {}

        fn query(&mut self, prefix: Option<&str>, query: &str) -> Vec<Entry> {
            self.calls
                .lock()
                .unwrap()
                .push((self.id.clone(), prefix.map(String::from), query.to_string()));
            vec![entry(&self.id, &self.id).history_key(&self.id).score(1.0)]
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
        c.add_provider(Box::new(TrackingProvider::new("long", vec!["=="], Arc::new(Mutex::new(Vec::new())))));
        let pre = c.preprocess_query("== 2 + 2");
        assert_eq!(pre.prefix.as_deref(), Some("=="));
        assert_eq!(pre.query, " 2 + 2");
    }

    #[test]
    fn preprocess_multiple_prefixes_on_one_provider() {
        let mut c = collection();
        c.add_provider(Box::new(TrackingProvider::new("calc", vec!["=", "=="], Arc::new(Mutex::new(Vec::new())))));
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
        )));
        c.add_provider(Box::new(TrackingProvider::new(
            "long",
            vec!["=="],
            Arc::clone(&calls),
        )));

        let pre = c.preprocess_query("== 2");
        let _ = c.entries(&pre);

        let log = calls.lock().unwrap();
        let short = log.iter().find(|(id, _, _)| id == "short").unwrap();
        let long = log.iter().find(|(id, _, _)| id == "long").unwrap();
        assert_eq!(short, &("short".to_string(), None, "== 2".to_string()));
        assert_eq!(long, &("long".to_string(), Some("==".to_string()), " 2".to_string()));
    }

    fn match_fields_entry(id: &str, text: &str) -> Entry {
        entry(id, id)
            .history_key(id)
            .match_fields(vec![MatchField {
                text: text.into(),
                weight: 1.0,
            }])
    }

    #[test]
    fn prefixed_entries_scored_against_stripped_query() {
        let mut c = collection();
        c.add_provider(Box::new(TestProvider::with_prefixes(
            "pfx",
            vec!["=="],
            vec![match_fields_entry("pfx", "Firefox")],
        )));
        c.add_provider(Box::new(TestProvider::new(
            "other",
            vec![match_fields_entry("other", "Firefox")],
        )));

        let (prefix, scored) = c.query("==fi");

        assert_eq!(prefix.as_deref(), Some("=="));
        assert!(
            scored.iter().any(|s| s.entry.id == "pfx"),
            "prefixed provider entry should fuzzy-match the stripped query"
        );
        assert!(
            !scored.iter().any(|s| s.entry.id == "other"),
            "unprefixed provider entry should not fuzzy-match the original query"
        );
        assert_eq!(scored[0].entry.provider_id.as_deref(), Some("pfx"));
    }

    #[test]
    fn entries_are_stamped_with_provider_id() {
        let mut c = collection();
        c.add_provider(Box::new(TestProvider::new(
            "desktop",
            vec![match_fields_entry("desktop", "Firefox")],
        )));
        let pre = c.preprocess_query("firefox");
        let entries = c.entries(&pre);
        for e in &entries {
            assert!(e.entry.provider_id.is_some());
        }
        assert!(entries.iter().any(|e| e.entry.provider_id.as_deref() == Some("desktop")));
    }

    #[test]
    fn providers_lists_ids_and_prefixes() {        let c = collection();
        let providers = c.providers();
        assert!(providers.iter().any(|p| p.id == "desktop" && p.prefixes.is_empty()));
        assert!(providers.iter().any(|p| p.id == "calculator" && p.prefixes == vec!["="]));
    }
}
