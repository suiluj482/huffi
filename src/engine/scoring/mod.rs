pub mod base_scorer;
pub mod config;
pub mod history;

pub use config::ScoringConfig;

use base_scorer::BaseScorer;
use history::{HistoryStore, KeyedHistoryRecord, KnownHistory, QueryHistory};

use crate::engine::provider::Entry;

#[derive(Debug, Clone, PartialEq)]
pub struct MatchField {
    pub text: String,
    pub weight: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Rank {
    Score(f32),
    MatchFields(Vec<MatchField>),
}

/// A batch of entries to be fuzzy-scored against a single query.
///
/// Each group supplies its own needle; all groups are normalized together
/// against the same global maximum.
///
/// The provider identity lives here rather than on [`Entry`]: every entry in
/// a group comes from the same provider, so one copy per group replaces one
/// per entry, and the annotation that replaces it can address an entry by
/// position rather than by owning it.
#[derive(Debug, Clone)]
pub struct QueryGroup {
    pub query: String,
    pub provider_id: String,
    pub entries: Vec<Entry>,
}

/// One scored entry: where it sits in its [`QueryGroup`], plus the scores the
/// ranking produced for it.
///
/// A result is deliberately just a position and three numbers. The entry
/// itself is addressed through [`QueryGroup::entries`] rather than stored,
/// so ranking a large corpus moves ~40 bytes per entry instead of copying
/// every field. [`resolve`](Self::resolve) turns an annotation back into the
/// entry it points at.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryAnnotation {
    pub group: u32,
    pub index: u32,
    pub base_score: f64,
    pub history_score: Option<f64>,
    pub combined: f64,
}

impl EntryAnnotation {
    /// The entry this annotation points at.
    pub fn resolve<'a>(&self, groups: &'a [QueryGroup]) -> &'a Entry {
        &groups[self.group as usize].entries[self.index as usize]
    }

    /// The id of the provider that produced this entry.
    pub fn provider_id<'a>(&self, groups: &'a [QueryGroup]) -> &'a str {
        &groups[self.group as usize].provider_id
    }
}

pub struct Scorer {
    history: HistoryStore,
    base_scorer: BaseScorer,
    boost_weight: f64,
    boost_samples: u32,
}

impl Scorer {
    /// Create the scorer backed by the history file inside `data_dir` (the
    /// filename is a [`history`] concern). In dry-run mode history stays
    /// in-memory.
    pub fn new_with_config(
        data_dir: impl AsRef<std::path::Path>,
        dry_run: bool,
        scoring: &ScoringConfig,
    ) -> anyhow::Result<Self> {
        let history = HistoryStore::new_with_config(data_dir, dry_run, scoring)?;
        Ok(Self {
            history,
            base_scorer: BaseScorer::new(scoring.empty_query_score),
            boost_weight: scoring.boost_weight,
            boost_samples: scoring.boost_samples,
        })
    }

    /// Score entries, each group fuzzy-matched against its own query.
    ///
    /// `groups` carries the base query per group (e.g. the prefix-stripped
    /// query for the provider whose prefix matched, the original query for
    /// everyone else). `history` is the precomputed [`QueryHistory`]
    /// for the original query — the same one providers received in their
    /// `QueryContext` — and drives the history half of the blend.
    ///
    /// The result addresses entries by position in `groups`, which the caller
    /// keeps alive; see [`EntryAnnotation`].
    pub fn score(&mut self, groups: &[QueryGroup], history: &QueryHistory) -> Vec<EntryAnnotation> {
        let mut scored = self.base_scorer.base_scoring(groups);
        self.history.history_scoring(history, groups, &mut scored);
        scored.sort_by(|a, b| {
            b.combined
                .partial_cmp(&a.combined)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored
    }

    pub fn record_launch(&mut self, query: &str, history_key: &str) {
        self.history.record_launch(query, history_key);
    }

    pub fn record_boost(&mut self, query: &str, history_key: &str) {
        self.history
            .record_boost(query, history_key, self.boost_weight, self.boost_samples);
    }

    pub fn delete(&mut self, query: &str, history_key: &str) {
        self.history.delete(query, history_key);
    }

    pub fn list_entries(&mut self, prefix: &str) -> Vec<KeyedHistoryRecord> {
        self.history.list_entries(prefix)
    }

    /// The precomputed [`QueryHistory`] for `query` — the store's
    /// decayed per-key scores plus query-level confidence and peak. See
    /// [`HistoryStore::history_for`].
    pub fn history_for(&self, query: &str) -> QueryHistory {
        self.history.history_for(query)
    }

    /// The unscoped view of every key ever used, cached by the store.
    /// Providers use it to reconstruct entries; ranking uses
    /// [`history_for`](Self::history_for) instead.
    pub fn known_history(&self) -> &KnownHistory {
        self.history.known_history()
    }
}
