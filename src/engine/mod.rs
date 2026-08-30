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
use crate::engine::provider::{EntryMeta, Provider, ProviderCollection, ProviderInfo};
use crate::engine::scoring::history::KeyedHistoryRecord;
use crate::engine::scoring::{Scored, Scorer};

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
    external: ExternalConfig,
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
        })
    }

    pub fn add_provider(&mut self, provider: Box<dyn Provider>) -> anyhow::Result<()> {
        self.providers.add_provider(provider)
    }

    /// Query providers and score results.
    ///
    /// Each provider is grouped with the query its entries should be
    /// fuzzy-matched against: the prefix-stripped query for the provider whose
    /// prefix matched, the original query for everyone else. All groups are
    /// normalized together. History is always looked up with the original
    /// query.
    ///
    /// Returns the resolved global prefix (if any) alongside the fully ranked
    /// result set; the UI slices its own visible window out of it.
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
            entry.entry.action.perform(&self.external);
        }
    }

    /// Boost a history key's ranking using the scorer's configured boost weight.
    pub fn boost(&mut self, query: &str, history_key: &str) {
        self.scorer.record_boost(query, history_key);
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
        Engine::new("/tmp/huffi-engine-test", true).unwrap()
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
        )))
        .unwrap();
        e.add_provider(Box::new(TestProvider::new(
            "other",
            vec![match_fields_entry("other", "Firefox")],
        )))
        .unwrap();

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
        )))
        .unwrap();

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

        let (_, scored) = e.query("fi");
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
        let (_, scored) = e.query("~~");
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
