use std::path::Path;

use crate::scoring::history::KeyedHistoryRecord;
use crate::scoring::{Scored, Scorer};

use super::{CalculatorProvider, DesktopEntryProvider, Entry, EntryMeta, Provider};

pub struct ProviderCollection {
    providers: Vec<Box<dyn Provider>>,
    scorer: Scorer,
    dry_run: bool,
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
    pub fn query(&self, query: &str) -> Vec<Scored<EntryMeta>> {
        let entries = self.entries(query);
        self.scorer.score(&entries, query)
    }

    /// Query providers without scoring (raw entries).
    pub fn entries(&self, query: &str) -> Vec<Entry> {
        self.providers
            .iter()
            .flat_map(|p| {
                let matched_prefix = p
                    .prefixes()
                    .iter()
                    .find(|pfx| query.starts_with(*pfx));
                let (prefix, stripped) = match matched_prefix {
                    Some(pfx) => (Some(*pfx), &query[pfx.len()..]),
                    None => (None, query),
                };
                p.query(prefix, stripped)
            })
            .collect()
    }

    /// Find an entry by ID and perform a select: record the launch in history,
    /// then execute the entry's action (unless in dry-run mode).
    pub fn select(&self, query: &str, entry_id: &str) {
        let entries = self.entries(query);

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

    pub fn record_launch(&self, query: &str, history_key: &str) {
        self.scorer.record_launch(query, history_key);
    }

    pub fn record_boost(&self, query: &str, history_key: &str, weight: f64) {
        self.scorer.record_boost(query, history_key, weight);
    }

    pub fn delete(&self, query: &str, history_key: &str) {
        self.scorer.delete(query, history_key);
    }

    pub fn list_entries(&self, prefix: &str) -> Vec<KeyedHistoryRecord> {
        self.scorer.list_entries(prefix)
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}
