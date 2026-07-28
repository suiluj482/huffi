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

    pub fn score<T: Clone>(&self, entries: &[Scoreable<T>], query: &str) -> Vec<Scored<T>> {
        let history = self.history.lock().unwrap();
        let mut base_scorer = self.base_scorer.lock().unwrap();
        let base_scored = base_scorer.base_scoring(entries.to_vec(), query);
        let mut scored = history.history_scoring(query, base_scored);
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
