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

    fn confidence(&self, history_keys: &PrefixHistory) -> f64 {
        let n = history_keys
            .values()
            .map(|record| record.n as f64)
            .sum::<f64>();
        n / (n + self.confidence_k)
    }

    /// Peak effective score in a `history_keys` map.
    fn max_effective(&self, history_keys: &PrefixHistory, now: f64) -> f64 {
        history_keys
            .values()
            .map(|record| record.effective(now, self.lambda))
            .fold(0.0f64, f64::max)
    }

    /// Effective score of `history_key` normalized against the `history_keys`
    /// peak, reusing a previously computed `now` and `max_effective`.
    ///
    /// Returns `0.0` when the peak is zero or the key is absent.
    fn history_score(
        &self,
        history_keys: &PrefixHistory,
        history_key: &str,
        now: f64,
        max_effective: f64,
    ) -> f64 {
        if max_effective <= 0.0 {
            return 0.0;
        }
        history_keys
            .get(history_key)
            .map(|record| record.effective(now, self.lambda) / max_effective)
            .unwrap_or(0.0)
    }

    pub fn history_scoring<T>(
        &self,
        query: &str,
        base_scored: Vec<BaseScored<T>>,
    ) -> Vec<Scored<T>> {
        let now = timestamp();
        let empty = HashMap::new();
        let history_keys = self.data.get(query).unwrap_or(&empty);

        // Only records for entries actually present in the result set may
        // influence confidence and normalization; launch fan-out leaves
        // records for apps that the current query does not surface.
        let history_keys: PrefixHistory = base_scored
            .iter()
            .filter_map(|c| {
                c.history_key.as_ref().and_then(|key| {
                    history_keys
                        .get(key)
                        .map(|record| (key.clone(), record.clone()))
                })
            })
            .collect();

        let confidence = self.confidence(&history_keys);
        let max_effective = self.max_effective(&history_keys, now);

        base_scored
            .into_iter()
            .map(|c| {
                let (history_score, combined) = match &c.history_key {
                    Some(key) => {
                        let h = self.history_score(&history_keys, key, now, max_effective);
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
        let now = timestamp();
        let empty = HashMap::new();
        let history_keys = history.data.get(query).unwrap_or(&empty);
        let max_effective = history.max_effective(history_keys, now);
        history.history_score(history_keys, history_key, now, max_effective)
    }

    fn confidence(history: &HistoryStore, query: &str) -> f64 {
        let empty = HashMap::new();
        let history_keys = history.data.get(query).unwrap_or(&empty);
        history.confidence(history_keys)
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
            "fi",
            vec![
                base("Gimp", 0.7, Some("Gimp")),
                base("Other", 0.6, None),
            ],
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
            "fi",
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
