pub mod base_scorer;
pub mod config;
pub mod history;

pub use config::ScoringConfig;

use base_scorer::BaseScorer;
use history::{HistoryStore, KeyedHistoryRecord, KnownHistory, QueryHistory};

use crate::engine::provider::{Entry, QueryGroup};

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

/// One scored entry: where it sits in its [`QueryGroup`], plus the scores the
/// ranking produced for it.
///
/// A result is deliberately just a position and three numbers. The entry
/// itself is addressed through [`QueryGroup::entries`] rather than stored,
/// so ranking a large corpus moves ~32 bytes per entry instead of copying
/// every field.
///
/// This is an internal ranking record, not a public result handle. Its
/// positions are private and only [`QueryReply`](crate::engine::QueryReply)
/// can resolve them, returning a
/// [`QueryHit`](crate::engine::QueryHit) that pairs the entry with its
/// provider and scores.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryAnnotation {
    group: u32,
    index: u32,
    pub(crate) base_score: f64,
    pub(crate) history_score: Option<f64>,
    pub(crate) combined: f64,
}

impl EntryAnnotation {
    /// A freshly base-scored entry.
    ///
    /// `combined` starts equal to `base_score`; history scoring later
    /// replaces it with the base/history blend, so a result with no history
    /// already ranks by its base score. Positions are checked rather than
    /// truncated, so a corpus that outgrows `u32` fails loudly instead of
    /// silently pointing at the wrong entry.
    pub(crate) fn new(group: usize, index: usize, base_score: f64) -> Self {
        Self {
            group: u32::try_from(group).expect("query group index exceeds u32::MAX"),
            index: u32::try_from(index).expect("entry index exceeds u32::MAX"),
            base_score,
            history_score: None,
            combined: base_score,
        }
    }

    /// The entry this annotation points at.
    pub(crate) fn resolve<'a>(&self, groups: &'a [QueryGroup]) -> &'a Entry {
        &groups[self.group as usize].entries[self.index as usize]
    }

    /// The id of the provider that produced this entry.
    pub(crate) fn provider_id<'a>(&self, groups: &'a [QueryGroup]) -> &'a str {
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
