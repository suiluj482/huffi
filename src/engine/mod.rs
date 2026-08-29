//! The huffi engine: providers plus scoring.
//!
//! [`Engine`] owns a [`ProviderCollection`](provider::ProviderCollection) and
//! the [`Scorer`](scoring::Scorer), exposing the querying, selection, and
//! history API the UI drives. The collection is responsible for provider
//! management and entry lookup; the scorer is responsible for fuzzy matching
//! and usage-history ranking. [`Engine`] wires the two together.

pub mod provider;
pub mod scoring;

use std::path::Path;

use crate::engine::provider::{EntryMeta, Provider, ProviderCollection, ProviderInfo};
use crate::engine::scoring::history::KeyedHistoryRecord;
use crate::engine::scoring::{Scored, Scorer};

/// How much a manual boost contributes to a history key's ranking.
pub const BOOST_WEIGHT: f64 = 10.0;

/// Upper bound on the number of scored results handed to a paged query.
pub const MAX_RESULTS: usize = 20;

/// A scored result page entry, shaped for rendering in the UI.
#[derive(Debug, Clone)]
pub struct QueryHit {
    pub entry_id: String,
    pub history_key: Option<String>,
    pub base_score: f64,
    pub history_score: Option<f64>,
    pub score: f64,
    pub title: String,
    pub subtitle: Option<String>,
    pub comment: Option<String>,
    pub icon: Option<String>,
    pub extra: Option<serde_json::Value>,
    pub set_query: Option<String>,
}

/// A registered provider and its trigger prefixes, as shown in the UI footer.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    pub id: String,
    pub prefixes: Vec<String>,
}

pub struct Engine {
    providers: ProviderCollection,
    scorer: Scorer,
    dry_run: bool,
}

impl Engine {
    pub fn new(data_path: impl AsRef<Path>, dry_run: bool) -> anyhow::Result<Self> {
        Ok(Self {
            providers: ProviderCollection::new(),
            scorer: Scorer::open(data_path, dry_run)?,
            dry_run,
        })
    }

    pub fn add_provider(&mut self, provider: Box<dyn Provider>) {
        self.providers.add_provider(provider);
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
        let pre = self.providers.preprocess_query(query);
        let groups = self.providers.grouped_entries(&pre);
        let scored = self.scorer.score(groups, query);
        (pre.prefix, scored)
    }

    /// Query, page through the ranked results, and project them onto
    /// [`QueryHit`]s for the UI. `length` is clamped to [`MAX_RESULTS`].
    ///
    /// Returns the resolved global prefix (if any), the page of hits, and the
    /// total number of scored results.
    pub fn query_hits(
        &mut self,
        query: &str,
        offset: usize,
        length: usize,
    ) -> (Option<String>, Vec<QueryHit>, usize) {
        let (prefix, scored) = self.query(query);
        let total = scored.len();
        let length = length.min(MAX_RESULTS);

        let results: Vec<QueryHit> = scored
            .into_iter()
            .skip(offset)
            .take(length)
            .map(|s| {
                let entry = s.entry;
                QueryHit {
                    entry_id: entry.id,
                    history_key: s.history_key,
                    base_score: s.base_score,
                    history_score: s.history_score,
                    score: s.combined,
                    title: entry.title,
                    subtitle: entry.subtitle,
                    comment: entry.comment,
                    icon: entry.icon,
                    extra: entry.extra,
                    set_query: entry.set_query,
                }
            })
            .collect();

        (prefix, results, total)
    }

    /// Find an entry by ID, record its launch in history (if it carries a
    /// history key), then execute its action — unless in dry-run mode.
    pub fn select(&mut self, query: &str, entry_id: &str) {
        let Some(entry) = self.providers.find(query, entry_id) else {
            return;
        };
        if let Some(key) = entry.history_key.clone() {
            self.scorer.record_launch(query, &key);
        }
        if !self.dry_run {
            entry.entry.action.perform();
        }
    }

    /// Boost a history key's ranking using [`BOOST_WEIGHT`].
    pub fn boost(&mut self, query: &str, history_key: &str) {
        self.record_boost(query, history_key, BOOST_WEIGHT);
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

    /// List registered providers and their trigger prefixes, for the UI.
    pub fn providers(&self) -> Vec<ProviderEntry> {
        self.providers
            .providers()
            .into_iter()
            .map(|provider: ProviderInfo| ProviderEntry {
                id: provider.id,
                prefixes: provider.prefixes,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::{Entry, TestProvider, entry};
    use crate::engine::scoring::MatchField;

    fn engine() -> Engine {
        Engine::new("/tmp/huffi-engine-test.json", true).unwrap()
    }

    fn match_fields_entry(id: &str, text: &str) -> Entry {
        entry(id, id).history_key(id).match_fields(vec![MatchField {
            text: text.into(),
            weight: 1.0,
        }])
    }

    #[test]
    fn prefixed_entries_scored_against_stripped_query() {
        let mut e = engine();
        e.add_provider(Box::new(TestProvider::with_prefixes(
            "pfx",
            vec!["=="],
            vec![match_fields_entry("pfx", "Firefox")],
        )));
        e.add_provider(Box::new(TestProvider::new(
            "other",
            vec![match_fields_entry("other", "Firefox")],
        )));

        let (prefix, scored) = e.query("==fi");

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
    fn select_records_launch_and_boosts_ranking() {
        let mut e = engine();
        e.add_provider(Box::new(TestProvider::with_prefixes(
            "pfx",
            vec!["::"],
            vec![match_fields_entry("firefox", "Firefox")],
        )));

        for _ in 0..5 {
            e.select("::fire", "firefox");
        }

        let (prefix, scored) = e.query("::fire");
        assert_eq!(prefix.as_deref(), Some("::"));
        assert_eq!(scored[0].entry.id, "firefox");
        assert!(
            scored[0].history_score.is_some_and(|h| h > 0.0),
            "select should feed usage history into ranking"
        );

        let history = e.list_entries("::fire");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].key, "firefox");
        assert_eq!(history[0].record.n, 5);
    }

    #[test]
    fn query_hits_pages_results_and_clamps_length() {
        let mut e = engine();
        e.add_provider(Box::new(TestProvider::with_prefixes("pfx", vec!["~~~"], {
            let mut entries = Vec::new();
            for i in 0..25 {
                entries.push(match_fields_entry(
                    &format!("t-app-{i}"),
                    &format!("T App {i}"),
                ));
            }
            entries
        })));

        let (prefix, hits, total) = e.query_hits("~~~app", 0, 20);
        assert_eq!(prefix.as_deref(), Some("~~~"));
        assert_eq!(hits.len(), 20, "length clamped to MAX_RESULTS");
        assert_eq!(total, 25);

        let (_, second_page, _) = e.query_hits("~~~app", 20, MAX_RESULTS);
        assert_eq!(second_page.len(), 5);
        assert_eq!(second_page[0].entry_id, "t-app-20");
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
        )));

        for _ in 0..10 {
            e.boost("fi", "files");
        }

        let (_, hits, _) = e.query_hits("fi", 0, MAX_RESULTS);
        assert_eq!(hits[0].history_key.as_deref(), Some("files"));
    }

    #[test]
    fn providers_lists_entries() {
        let e = engine();
        let providers = e.providers();
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
}
