use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::config::ScoringConfig;

use super::{BaseScored, Scored};

pub type HistoryKey = String;

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

pub struct HistoryStore {
    data: HashMap<String, HashMap<HistoryKey, HistoryRecord>>,
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
                .or_insert_with(|| HistoryRecord {
                    score: 0.0,
                    last_update: now,
                    n: 0,
                })
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
            .or_insert_with(|| HistoryRecord {
                score: 0.0,
                last_update: now,
                n: 0,
            })
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

    pub fn confidence(&self, query: &str) -> f64 {
        let n = self
            .data
            .get(query)
            .map(|history_keys| {
                history_keys
                    .values()
                    .map(|record| record.n as f64)
                    .sum::<f64>()
            })
            .unwrap_or(0.0);
        n / (n + self.confidence_k)
    }

    pub fn history_score(&self, query: &str, history_key: &str) -> f64 {
        let now = timestamp();
        let Some(history_keys) = self.data.get(query) else {
            return 0.0;
        };
        let max_effective = history_keys
            .values()
            .map(|record| record.effective(now, self.lambda))
            .fold(0.0f64, f64::max);
        if max_effective <= 0.0 {
            return 0.0;
        }
        let raw = history_keys
            .get(history_key)
            .map(|record| record.effective(now, self.lambda))
            .unwrap_or(0.0);
        raw / max_effective
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

    pub fn history_scoring<T>(
        &self,
        query: &str,
        base_scored: Vec<BaseScored<T>>,
    ) -> Vec<Scored<T>> {
        let confidence = self.confidence(query);

        base_scored
            .into_iter()
            .map(|c| {
                let (history_score, combined) = match &c.history_key {
                    Some(key) => {
                        let h = self.history_score(query, key);
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

    #[test]
    fn untrained_prefix_falls_through_to_query_score() {
        let history = HistoryStore::default();
        let score = history.history_score("fi", "Firefox");
        assert_eq!(score, 0.0);
    }

    #[test]
    fn trained_prefix_has_score() {
        let mut history = HistoryStore::default();
        for _ in 0..10 {
            history.record_launch("fi", "Firefox");
        }
        let score = history.history_score("fi", "Firefox");
        assert!(score > 0.0);
    }

    #[test]
    fn fan_out_trains_shorter_prefix() {
        let mut history = HistoryStore::default();
        for _ in 0..5 {
            history.record_launch("fire", "Firefox");
        }
        let score = history.history_score("fi", "Firefox");
        let confidence = history.confidence("fi");
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

        let score_f = history.history_score("f", "NewApp");
        let score_f_firefox = history.history_score("f", "Firefox");
        assert!(score_f > score_f_firefox);

        let score_fi = history.history_score("fi", "Firefox");
        let score_fi_new = history.history_score("fi", "NewApp");
        assert!(score_fi > score_fi_new);
    }

    #[test]
    fn boost_changes_ranking() {
        let mut history = HistoryStore::default();
        for _ in 0..5 {
            history.record_launch("f", "Firefox");
        }
        history.record_boost("f", "Finder", 10.0, 5);

        let score_firefox = history.history_score("f", "Firefox");
        let score_finder = history.history_score("f", "Finder");
        assert!(score_finder > score_firefox);
    }

    #[test]
    fn boost_does_not_fan_out() {
        let mut history = HistoryStore::default();
        history.record_boost("fi", "Finder", 10.0, 5);
        let confidence = history.confidence("f");
        assert_eq!(confidence, 0.0);
    }

    #[test]
    fn delete_removes_association() {
        let mut history = HistoryStore::default();
        for _ in 0..5 {
            history.record_launch("f", "Firefox");
        }
        history.delete("f", "Firefox");
        let confidence = history.confidence("f");
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
}
