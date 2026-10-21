use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::config::ScoringConfig;

use super::{BaseScored, Scored};

pub type HistoryKey = String;

pub type PrefixHistory = HashMap<HistoryKey, HistoryRecord>;
pub type HistoryData = HashMap<String, PrefixHistory>;

/// Name of the history file inside the huffi data folder.
pub const HISTORY_FILE: &str = "history.json";

/// Decay constant for a half-life in days: `λ = ln(2) / half_life_secs`.
fn half_life_lambda(half_life_days: f64) -> f64 {
    std::f64::consts::LN_2 / (half_life_days * 86400.0)
}

/// A time-decayed record for a (prefix, application) pair.
///
/// The score decays exponentially over time with a configurable half-life.
/// On each launch, the effective (decayed) score is incremented by 1.
/// On each boost, the effective score is incremented by the boost weight.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryRecord {
    pub score: f64,
    pub last_update: f64,
    pub n: u32,
}

impl HistoryRecord {
    fn new(now: f64) -> Self {
        Self {
            score: 0.0,
            last_update: now,
            n: 0,
        }
    }

    fn effective(&self, now: f64, lambda: f64) -> f64 {
        let dt = (now - self.last_update).max(0.0);
        self.score * (-lambda * dt).exp()
    }

    fn record_launch(&mut self, now: f64, lambda: f64) {
        self.score = self.effective(now, lambda) + 1.0;
        self.last_update = now;
        self.n += 1;
    }

    fn record_boost(&mut self, now: f64, weight: f64, samples: u32, lambda: f64) {
        self.score = self.effective(now, lambda) + weight;
        self.last_update = now;
        self.n += samples;
    }
}

pub struct KeyedHistoryRecord {
    pub key: HistoryKey,
    pub record: HistoryRecord,
}

/// One recorded history key inside a [`QueryHistory`].
#[derive(Debug, Clone)]
pub struct QueryHistoryEntry {
    /// The history key the record is stored under.
    pub key: HistoryKey,
    /// The decayed raw score, as of when the history was built.
    pub effective: f64,
    /// [`effective`](Self::effective) normalized against the history's
    /// [`peak`](QueryHistory::peak), in `0.0..=1.0` (`0.0` when the peak is
    /// zero).
    pub score: f64,
    /// The record's sample count `n` (launches and boost samples).
    pub samples: u32,
}

/// The precomputed history for one query: every recorded key with its
/// decayed score, plus the query-level confidence and peak.
///
/// Built once per query by [`HistoryStore::history_for`], which is the only
/// place records are decayed. Two views project from the same precomputed
/// history:
///
/// - Providers see it via `QueryContext::history`, where
///   [`score`](QueryHistoryEntry::score) is normalized against the peak over
///   **every** recorded key (the only view possible before results exist).
/// - Final ranking ([`history_scoring`](HistoryStore::history_scoring))
///   recomputes a peak and confidence over only the keys **present in the
///   result set**, so unrelated records cannot inflate them.
#[derive(Debug, Clone, Default)]
pub struct QueryHistory {
    entries: Vec<QueryHistoryEntry>,
    confidence: f64,
    peak: f64,
}

impl QueryHistory {
    /// A [`QueryHistory`] with no recorded history.
    pub const EMPTY: Self = Self {
        entries: Vec::new(),
        confidence: 0.0,
        peak: 0.0,
    };

    /// Every recorded key, sorted for a stable order.
    pub fn entries(&self) -> &[QueryHistoryEntry] {
        &self.entries
    }

    /// Every recorded key, sorted for a stable order.
    pub fn keys(&self) -> impl Iterator<Item = &HistoryKey> {
        self.entries.iter().map(|e| &e.key)
    }

    /// The entry recorded under `key`, if any.
    pub fn get(&self, key: &str) -> Option<&QueryHistoryEntry> {
        self.entries
            .binary_search_by(|e| e.key.as_str().cmp(key))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// Query-level confidence in `0.0..=1.0`: `n / (n + k)` over every
    /// recorded key, so it rises as the exact query accumulates evidence.
    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    /// The highest [`effective`](QueryHistoryEntry::effective) score among
    /// the recorded keys, the normalizer behind [`score`](QueryHistoryEntry::score).
    pub fn peak(&self) -> f64 {
        self.peak
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub struct HistoryStore {
    data: HistoryData,
    path: Option<PathBuf>,
    lambda: f64,
    confidence_k: f64,
}

impl Default for HistoryStore {
    fn default() -> Self {
        let scoring = ScoringConfig::default();
        Self {
            data: HashMap::new(),
            path: None,
            lambda: half_life_lambda(scoring.half_life_days),
            confidence_k: scoring.confidence_k,
        }
    }
}

impl HistoryStore {
    /// Create the history store backed by `data_dir.join(HISTORY_FILE)`. The
    /// file is only read (and later written) when not in dry-run mode.
    pub fn new_with_config(
        data_dir: impl AsRef<Path>,
        dry_run: bool,
        scoring: &ScoringConfig,
    ) -> anyhow::Result<Self> {
        let path = data_dir.as_ref().join(HISTORY_FILE);
        let data = if !dry_run && path.exists() {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| anyhow::anyhow!("failed to read history file: {e}"))?;
            serde_json::from_str(&content)
                .map_err(|e| anyhow::anyhow!("failed to parse history file: {e}"))?
        } else {
            HashMap::new()
        };
        Ok(HistoryStore {
            data,
            path: if dry_run { None } else { Some(path) },
            lambda: half_life_lambda(scoring.half_life_days),
            confidence_k: scoring.confidence_k,
        })
    }

    fn flush_to_disk(&self) {
        let Some(ref path) = self.path else {
            return;
        };
        let json = match serde_json::to_string(&self.data) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[history] failed to serialize: {e}");
                return;
            }
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &json) {
            eprintln!("[history] failed to write: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            eprintln!("[history] failed to rename: {e}");
        }
    }

    // logic
    fn prefixes(query: &str) -> impl Iterator<Item = &str> {
        (0..=query.len()).filter_map(move |i| {
            if i == 0 {
                Some("")
            } else {
                query.is_char_boundary(i).then(|| &query[..i])
            }
        })
    }

    pub fn record_launch(&mut self, query: &str, history_key: &str) {
        let now = timestamp();
        for prefix in Self::prefixes(query) {
            self.data
                .entry(prefix.to_string())
                .or_default()
                .entry(history_key.to_string())
                .or_insert_with(|| HistoryRecord::new(now))
                .record_launch(now, self.lambda);
        }
        self.flush_to_disk();
    }

    pub fn record_boost(&mut self, query: &str, history_key: &str, weight: f64, samples: u32) {
        let now = timestamp();
        self.data
            .entry(query.to_string())
            .or_default()
            .entry(history_key.to_string())
            .or_insert_with(|| HistoryRecord::new(now))
            .record_boost(now, weight, samples, self.lambda);
        self.flush_to_disk();
    }

    pub fn delete(&mut self, query: &str, history_key: &str) {
        if let Some(history_keys) = self.data.get_mut(query) {
            history_keys.remove(history_key);
            if history_keys.is_empty() {
                self.data.remove(query);
            }
        }
        self.flush_to_disk();
    }

    /// The precomputed [`QueryHistory`] for the exact `query`, decayed at
    /// the time of the call. Empty when the query has no recorded history.
    ///
    /// Exact match only. [`record_launch`] also fans the key out to every
    /// prefix of the launched query, but those records belong to the
    /// prefixes, not to `query` itself. This is the only place records are
    /// decayed: providers (`QueryContext::history`) and ranking
    /// ([`history_scoring`]) both project from the returned history.
    pub fn history_for(&self, query: &str) -> QueryHistory {
        let Some(prefix_history) = self.data.get(query) else {
            return QueryHistory::EMPTY;
        };

        let now = timestamp();
        let mut entries: Vec<QueryHistoryEntry> = prefix_history
            .iter()
            .map(|(key, record)| QueryHistoryEntry {
                key: key.clone(),
                effective: record.effective(now, self.lambda),
                score: 0.0,
                samples: record.n,
            })
            .collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));

        let peak = entries.iter().map(|e| e.effective).fold(0.0f64, f64::max);
        let n: f64 = entries.iter().map(|e| e.samples as f64).sum();
        let confidence = n / (n + self.confidence_k);
        for entry in &mut entries {
            entry.score = if peak > 0.0 {
                entry.effective / peak
            } else {
                0.0
            };
        }

        QueryHistory {
            entries,
            confidence,
            peak,
        }
    }

    /// Blend base scores with a precomputed [`QueryHistory`].
    ///
    /// The query-level [`confidence`](QueryHistory::confidence) and
    /// [`peak`](QueryHistory::peak) look over the whole query; the final blend
    /// narrows to the keys **present in the result set** so records left
    /// behind by launch fan-out for entries the query does not surface
    /// cannot inflate confidence or the normalizer.
    pub fn history_scoring<T>(
        &self,
        history: &QueryHistory,
        base_scored: Vec<BaseScored<T>>,
    ) -> Vec<Scored<T>> {
        // Only records for entries actually present in the result set may
        // influence confidence and normalization; launch fan-out leaves
        // records for apps that the current query does not surface.
        let present: HashMap<&str, &QueryHistoryEntry> = base_scored
            .iter()
            .filter_map(|c| c.history_key.as_deref().and_then(|key| history.get(key)))
            .map(|e| (e.key.as_str(), e))
            .collect();

        let n: f64 = present.values().map(|e| e.samples as f64).sum();
        let confidence = n / (n + self.confidence_k);
        let max_effective = present.values().map(|e| e.effective).fold(0.0f64, f64::max);

        base_scored
            .into_iter()
            .map(|c| {
                let (history_score, combined) = match &c.history_key {
                    Some(key) => {
                        let h = present
                            .get(key.as_str())
                            .map(|e| {
                                if max_effective > 0.0 {
                                    e.effective / max_effective
                                } else {
                                    0.0
                                }
                            })
                            .unwrap_or(0.0);
                        let combined = confidence * h + (1.0 - confidence) * c.base_score;
                        (Some(h), combined)
                    }
                    None => (None, c.base_score),
                };
                Scored {
                    entry: c.entry,
                    rank: c.rank,
                    history_key: c.history_key,
                    base_score: c.base_score,
                    history_score,
                    combined,
                }
            })
            .collect()
    }

    pub fn list_entries(&self, prefix: &str) -> Vec<KeyedHistoryRecord> {
        let now = timestamp();
        self.data
            .get(prefix)
            .map(|history_keys| {
                history_keys
                    .iter()
                    .map(|(history_key, record)| {
                        let mut entry = record.clone();
                        entry.score = record.effective(now, self.lambda);
                        KeyedHistoryRecord {
                            key: history_key.clone(),
                            record: entry,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub fn timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(history: &HistoryStore, query: &str, history_key: &str) -> f64 {
        history
            .history_for(query)
            .get(history_key)
            .map(|entry| entry.score)
            .unwrap_or(0.0)
    }

    fn confidence(history: &HistoryStore, query: &str) -> f64 {
        history.history_for(query).confidence()
    }

    #[test]
    fn untrained_prefix_falls_through_to_query_score() {
        let history = HistoryStore::default();
        let score = score(&history, "fi", "Firefox");
        assert_eq!(score, 0.0);
    }

    #[test]
    fn trained_prefix_has_score() {
        let mut history = HistoryStore::default();
        for _ in 0..10 {
            history.record_launch("fi", "Firefox");
        }
        let score = score(&history, "fi", "Firefox");
        assert!(score > 0.0);
    }

    #[test]
    fn fan_out_trains_shorter_prefix() {
        let mut history = HistoryStore::default();
        for _ in 0..5 {
            history.record_launch("fire", "Firefox");
        }
        let score = score(&history, "fi", "Firefox");
        let confidence = confidence(&history, "fi");
        assert!(score > 0.0);
        assert!(confidence > 0.0);
    }

    #[test]
    fn migration_scenario() {
        let mut history = HistoryStore::default();
        for _ in 0..20 {
            history.record_launch("f", "Firefox");
        }
        for _ in 0..10 {
            history.record_launch("fi", "Firefox");
        }
        for _ in 0..30 {
            history.record_launch("f", "NewApp");
        }

        let score_f = score(&history, "f", "NewApp");
        let score_f_firefox = score(&history, "f", "Firefox");
        assert!(score_f > score_f_firefox);

        let score_fi = score(&history, "fi", "Firefox");
        let score_fi_new = score(&history, "fi", "NewApp");
        assert!(score_fi > score_fi_new);
    }

    #[test]
    fn boost_changes_ranking() {
        let mut history = HistoryStore::default();
        for _ in 0..5 {
            history.record_launch("f", "Firefox");
        }
        history.record_boost("f", "Finder", 10.0, 5);

        let score_firefox = score(&history, "f", "Firefox");
        let score_finder = score(&history, "f", "Finder");
        assert!(score_finder > score_firefox);
    }

    #[test]
    fn boost_does_not_fan_out() {
        let mut history = HistoryStore::default();
        history.record_boost("fi", "Finder", 10.0, 5);
        let confidence = confidence(&history, "f");
        assert_eq!(confidence, 0.0);
    }

    #[test]
    fn delete_removes_association() {
        let mut history = HistoryStore::default();
        for _ in 0..5 {
            history.record_launch("f", "Firefox");
        }
        history.delete("f", "Firefox");
        let confidence = confidence(&history, "f");
        assert_eq!(confidence, 0.0);
    }

    #[test]
    fn list_entries_returns_data() {
        let mut history = HistoryStore::default();
        history.record_launch("f", "Firefox");
        history.record_launch("f", "Firefox");
        let entries = history.list_entries("f");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "Firefox");
        assert_eq!(entries[0].record.n, 2);
    }

    #[test]
    fn list_entries_empty_for_unknown_prefix() {
        let history = HistoryStore::default();
        let entries = history.list_entries("zzz");
        assert!(entries.is_empty());
    }

    #[test]
    fn history_for_reports_recorded_history_for_the_exact_query() {
        let mut history = HistoryStore::default();
        assert!(history.history_for("fi").is_empty());

        history.record_boost("fi", "gimp", 2.0, 3);
        history.record_boost("fi", "Firefox", 1.0, 1);
        let view = history.history_for("fi");

        assert_eq!(
            view.keys().cloned().collect::<Vec<_>>(),
            vec!["Firefox".to_string(), "gimp".to_string()],
            "keys are sorted for a stable order"
        );
        assert!(view.peak() > 0.0);
        assert_eq!(
            view.confidence(),
            4.0 / (4.0 + history.confidence_k),
            "confidence sums the sample counts of every recorded key"
        );
        assert_eq!(
            view.get("gimp").unwrap().score,
            1.0,
            "the peak key normalizes to 1.0"
        );
        let firefox = view.get("Firefox").unwrap();
        assert!(
            firefox.score > 0.0 && firefox.score < 1.0,
            "a weaker key normalizes into the peak's scale"
        );
        assert_eq!(firefox.samples, 1);

        history.record_launch("fire", "Thunar");
        assert_eq!(
            history
                .history_for("fire")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["Thunar".to_string()],
            "a launch is recorded under the exact query it fanned out to"
        );
        assert_eq!(
            history
                .history_for("fi")
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                "Firefox".to_string(),
                "Thunar".to_string(),
                "gimp".to_string()
            ],
            "fan-out also records the key under every shorter prefix"
        );
        assert!(history.history_for("zzz").is_empty());
    }

    #[test]
    fn history_scoring_ignores_records_not_in_result_set() {
        let mut history = HistoryStore::default();
        for _ in 0..10 {
            history.record_launch("fi", "Firefox");
        }
        for _ in 0..2 {
            history.record_launch("fi", "Gimp");
        }

        fn base<'a>(name: &'a str, score: f64, key: Option<&str>) -> BaseScored<&'a str> {
            BaseScored {
                entry: name,
                rank: crate::engine::scoring::Rank::Score(0.0),
                history_key: key.map(str::to_string),
                base_score: score,
            }
        }

        let scored = history.history_scoring(
            &history.history_for("fi"),
            vec![base("Gimp", 0.7, Some("Gimp")), base("Other", 0.6, None)],
        );

        let gimp = scored.iter().find(|s| s.entry == "Gimp").unwrap();
        let other = scored.iter().find(|s| s.entry == "Other").unwrap();

        assert_eq!(
            gimp.history_score.unwrap(),
            1.0,
            "peak (max_effective) must be computed only over present entries"
        );
        let confidence = 2.0 / (2.0 + history.confidence_k);
        assert!(
            (gimp.combined - (confidence * 1.0 + (1.0 - confidence) * 0.7)).abs() < 1e-6,
            "Firefox's launches must not inflate confidence"
        );
        assert_eq!(other.combined, 0.6);
    }

    #[test]
    fn history_scoring_matches_per_key_formula() {
        let mut history = HistoryStore::default();
        for _ in 0..10 {
            history.record_launch("fi", "Firefox");
        }
        for _ in 0..2 {
            history.record_launch("fi", "Gimp");
        }

        fn base<'a>(name: &'a str, score: f64, key: Option<&str>) -> BaseScored<&'a str> {
            BaseScored {
                entry: name,
                rank: crate::engine::scoring::Rank::Score(0.0),
                history_key: key.map(str::to_string),
                base_score: score,
            }
        }

        let scored = history.history_scoring(
            &history.history_for("fi"),
            vec![
                base("Firefox", 0.8, Some("Firefox")),
                base("Gimp", 0.7, Some("Gimp")),
                base("Other", 0.6, None),
            ],
        );

        let firefox = scored.iter().find(|s| s.entry == "Firefox").unwrap();
        let gimp = scored.iter().find(|s| s.entry == "Gimp").unwrap();
        let other = scored.iter().find(|s| s.entry == "Other").unwrap();

        assert_eq!(firefox.history_score.unwrap(), 1.0);
        assert!((gimp.history_score.unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(other.history_score, None);

        assert!(
            (score(&history, "fi", "Gimp") - gimp.history_score.unwrap()).abs() < 1e-6,
            "hoisted path and single-key history_score must agree"
        );

        let confidence = 12.0 / (12.0 + history.confidence_k);
        assert!((firefox.combined - (confidence * 1.0 + (1.0 - confidence) * 0.8)).abs() < 1e-6);
        assert!((gimp.combined - (confidence * 0.2 + (1.0 - confidence) * 0.7)).abs() < 1e-6);
        assert_eq!(other.combined, 0.6);
        assert!(firefox.combined > other.combined && other.combined > gimp.combined);
    }
}
