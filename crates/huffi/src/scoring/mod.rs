pub mod base_scorer;
pub mod history;

use std::sync::Mutex;

use base_scorer::BaseScorer;
use history::{HistoryStore, KeyedHistoryRecord};

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
    history: Mutex<HistoryStore>,
    base_scorer: Mutex<BaseScorer>,
}

impl Scorer {
    pub fn open(path: impl AsRef<std::path::Path>, dry_run: bool) -> anyhow::Result<Self> {
        let history = HistoryStore::open(path, dry_run)?;
        Ok(Self {
            history: Mutex::new(history),
            base_scorer: Mutex::new(BaseScorer::new()),
        })
    }

    /// Score entries, each group fuzzy-matched against its own query.
    ///
    /// `groups` carries the base query per group (e.g. the prefix-stripped
    /// query for the provider whose prefix matched, the original query for
    /// everyone else). `history_query` is used for history lookup and stays
    /// the user's original input.
    pub fn score<T>(&self, groups: Vec<QueryGroup<T>>, history_query: &str) -> Vec<Scored<T>> {
        let history = self.history.lock().unwrap();
        let mut base_scorer = self.base_scorer.lock().unwrap();
        let base_scored = base_scorer.base_scoring(groups);
        let mut scored = history.history_scoring(history_query, base_scored);
        scored.sort_by(|a, b| {
            b.combined
                .partial_cmp(&a.combined)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored
    }

    pub fn record_launch(&self, query: &str, history_key: &str) {
        self.history.lock().unwrap().record_launch(query, history_key);
    }

    pub fn record_boost(&self, query: &str, history_key: &str, weight: f64) {
        self.history.lock().unwrap().record_boost(query, history_key, weight);
    }

    pub fn delete(&self, query: &str, history_key: &str) {
        self.history.lock().unwrap().delete(query, history_key);
    }

    pub fn list_entries(&self, prefix: &str) -> Vec<KeyedHistoryRecord> {
        self.history.lock().unwrap().list_entries(prefix)
    }
}
