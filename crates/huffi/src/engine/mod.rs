//! The huffi engine: providers plus scoring.
//!
//! [`Engine`] owns a [`ProviderCollection`](provider::ProviderCollection) and
//! the [`Scorer`](scoring::Scorer), exposing the querying, selection, and
//! history API the daemon drives. The collection is responsible for provider
//! management and entry lookup; the scorer is responsible for fuzzy matching
//! and usage-history ranking. [`Engine`] wires the two together.

pub mod provider;
pub mod scoring;

use std::path::Path;

use crate::engine::provider::{
    EntryMeta, Provider, ProviderCollection, ProviderInfo,
};
use crate::engine::scoring::history::KeyedHistoryRecord;
use crate::engine::scoring::{Scored, Scorer};

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
        self.providers.providers()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::{entry, Entry, TestProvider};
    use crate::engine::scoring::MatchField;

    fn engine() -> Engine {
        Engine::new("/tmp/huffi-engine-test.json", true).unwrap()
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
}