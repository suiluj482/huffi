pub mod base_scorer;
pub mod config;
pub mod history;

pub use config::ScoringConfig;

use base_scorer::BaseScorer;
use history::{HistoryStore, KeyedHistoryRecord, QueryHistory};

#[derive(Debug, Clone)]
pub struct MatchField {
    pub text: String,
    pub weight: f32,
}

#[derive(Debug, Clone)]
pub enum Rank {
    Score(f32),
    MatchFields(Vec<MatchField>),
}

#[derive(Debug, Clone)]
pub struct Scoreable<T> {
    pub entry: T,
    pub rank: Rank,
    pub history_key: Option<String>,
}

/// A batch of entries to be fuzzy-scored against a single query.
///
/// Each group supplies its own needle; all groups are normalized together
/// against the same global maximum.
#[derive(Debug, Clone)]
pub struct QueryGroup<T> {
    pub query: String,
    pub entries: Vec<Scoreable<T>>,
}

#[derive(Debug, Clone)]
pub struct BaseScored<T> {
    pub entry: T,
    pub rank: Rank,
    pub history_key: Option<String>,
    pub base_score: f64,
}

#[derive(Debug, Clone)]
pub struct Scored<T> {
    pub entry: T,
    pub rank: Rank,
    pub history_key: Option<String>,
    pub base_score: f64,
    pub history_score: Option<f64>,
    pub combined: f64,
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
    pub fn score<T>(
        &mut self,
        groups: Vec<QueryGroup<T>>,
        history: &QueryHistory,
    ) -> Vec<Scored<T>> {
        let base_scored = self.base_scorer.base_scoring(groups);
        let mut scored = self.history.history_scoring(history, base_scored);
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
}
