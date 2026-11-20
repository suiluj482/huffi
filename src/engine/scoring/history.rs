use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::config::ScoringConfig;

use crate::engine::provider::QueryGroup;

use super::EntryAnnotation;

pub type HistoryKey = String;

pub type PrefixHistory = HashMap<HistoryKey, HistoryRecord>;
pub type HistoryData = HashMap<String, PrefixHistory>;

/// Name of the history file inside the huffi data folder.
pub const HISTORY_FILE: &str = "history.json";

/// Which prefix buckets a delete (or zero) operation targets.
///
/// The exact query is always included. "Broader" prefixes are the shorter
/// strings the query starts with (including the empty prefix); "narrower"
/// prefixes are existing buckets that start with the query. A narrower bucket
/// is therefore more specific, not merely longer — `browser` is not narrower
/// than `fire`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteScope {
    /// Every bucket holding the key, whatever its relation to the query.
    Everywhere,
    /// Only the bucket for the exact query.
    Exact,
    /// The exact query and all of its broader prefixes, including `""`.
    Broader,
    /// The exact query and existing narrower buckets that hold the key.
    Narrower,
}

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

    fn zero(&mut self, now: f64) {
        self.score = 0.0;
        self.n = 0;
        self.last_update = now;
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
    /// The record's sample count `n` (recorded uses and boost samples).
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

/// Every key ever used, independent of the query it was used under.
///
/// [`record_launch`](HistoryStore::record_launch) fans a recorded use out to
/// every prefix of the query, including the empty prefix. Enter and accepting
/// a Tab suggestion both record through it. The empty-prefix bucket is
/// therefore a global index of used keys: no per-query lookup or decay pass is
/// needed to ask "which keys have been used at all?" This is the view
/// providers use to reconstruct entries that the current listing does not
/// contain (see `QueryContext::known_history_keys`).
///
/// It deliberately carries no scores and is not prefix-scoped. Ranking still
/// uses the scoped [`QueryHistory`] from [`HistoryStore::history_for`].
#[derive(Debug, Clone, Default)]
pub struct KnownHistory {
    keys: Vec<HistoryKey>,
}

impl KnownHistory {
    /// A [`KnownHistory`] with no recorded uses.
    pub const EMPTY: Self = Self { keys: Vec::new() };

    /// Every known key, sorted for a stable order.
    pub fn keys(&self) -> impl Iterator<Item = &HistoryKey> {
        self.keys.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    fn from_data(data: &HistoryData) -> Self {
        let mut keys: Vec<HistoryKey> = data
            .get("")
            .map(|prefix_history| prefix_history.keys().cloned().collect())
            .unwrap_or_default();
        keys.sort();
        Self { keys }
    }
}

pub struct HistoryStore {
    data: HistoryData,
    /// Cached unscoped key list, rebuilt on load and every history write.
    known: KnownHistory,
    path: Option<PathBuf>,
    lambda: f64,
    confidence_k: f64,
}

impl Default for HistoryStore {
    fn default() -> Self {
        let scoring = ScoringConfig::default();
        Self {
            data: HashMap::new(),
            known: KnownHistory::EMPTY,
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
        let known = KnownHistory::from_data(&data);
        Ok(HistoryStore {
            data,
            known,
            path: if dry_run { None } else { Some(path) },
            lambda: half_life_lambda(scoring.half_life_days),
            confidence_k: scoring.confidence_k,
        })
    }

    /// The unscoped view of every key ever used. Cached, so reading it is
    /// free; it is rebuilt only when history changes.
    pub fn known_history_keys(&self) -> &KnownHistory {
        &self.known
    }

    fn rebuild_known(&mut self) {
        self.known = KnownHistory::from_data(&self.data);
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

    /// Record a use of `history_key` under `query`. A real launch and an
    /// accepted Tab suggestion are both uses, so both call this. The record
    /// fans out to every prefix of `query`.
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
        self.rebuild_known();
        self.flush_to_disk();
    }

    /// Boost `history_key` at `query` and every prefix of it, like a launch
    /// with the configured boost weight and sample count instead of 1.
    pub fn record_boost(&mut self, query: &str, history_key: &str, weight: f64, samples: u32) {
        let now = timestamp();
        for prefix in Self::prefixes(query) {
            self.data
                .entry(prefix.to_string())
                .or_default()
                .entry(history_key.to_string())
                .or_insert_with(|| HistoryRecord::new(now))
                .record_boost(now, weight, samples, self.lambda);
        }
        self.rebuild_known();
        self.flush_to_disk();
    }

    /// Remove `history_key` from the buckets selected by `scope`, or, when
    /// `zero_only`, clear its score and samples in place instead. Zeroing
    /// keeps the record (and so the key's place in [`KnownHistory`]); removing
    /// it from the empty prefix drops the key from the unscoped view.
    pub fn delete(&mut self, query: &str, history_key: &str, scope: DeleteScope, zero_only: bool) {
        // Collect first: the match borrows `data` immutably while the loop
        // below mutates it.
        let targets: Vec<String> = match scope {
            DeleteScope::Everywhere => self
                .data
                .iter()
                .filter(|(_, keys)| keys.contains_key(history_key))
                .map(|(prefix, _)| prefix.clone())
                .collect(),
            DeleteScope::Exact => vec![query.to_string()],
            DeleteScope::Broader => Self::prefixes(query).map(str::to_string).collect(),
            DeleteScope::Narrower => std::iter::once(query.to_string())
                .chain(
                    self.data
                        .iter()
                        .filter(|(prefix, keys)| {
                            prefix.starts_with(query)
                                && prefix.as_str() != query
                                && keys.contains_key(history_key)
                        })
                        .map(|(prefix, _)| prefix.clone()),
                )
                .collect(),
        };

        let now = timestamp();
        for prefix in targets {
            let Some(keys) = self.data.get_mut(&prefix) else {
                continue;
            };
            if zero_only {
                if let Some(record) = keys.get_mut(history_key) {
                    record.zero(now);
                }
            } else {
                keys.remove(history_key);
                if keys.is_empty() {
                    self.data.remove(&prefix);
                }
            }
        }
        self.rebuild_known();
        self.flush_to_disk();
    }

    /// The precomputed [`QueryHistory`] for the exact `query`, decayed at
    /// the time of the call. Empty when the query has no recorded history.
    ///
    /// Exact match only. [`record_launch`] also fans the key out to every
    /// prefix of the used query, but those records belong to the prefixes,
    /// not to `query` itself. This is the only place records are
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

    /// Blend usage history into the annotations in place.
    ///
    /// Every entry is annotated before this step, including non-matches with
    /// a zero base score. `present` narrows the query's records to keys that
    /// actually have an entry in these results, so stale history for a key
    /// no provider returned cannot inflate confidence or the normalizer. The
    /// caller drops zero-scored annotations after the blend.
    ///
    /// `groups` resolves each annotation to the entry that carries its
    /// history key; the scores it produces replace `combined`.
    pub fn history_scoring(
        &self,
        history: &QueryHistory,
        groups: &[QueryGroup],
        annotations: &mut [EntryAnnotation],
    ) {
        // Only keys with an entry in this result set may influence confidence
        // and normalization; a history record whose app is no longer returned
        // by any provider must not skew the blend.
        let present: HashMap<&str, &QueryHistoryEntry> = annotations
            .iter()
            .filter_map(|a| {
                let entry = a.resolve(groups);
                entry
                    .history_key
                    .as_deref()
                    .and_then(|key| history.get(key))
            })
            .map(|e| (e.key.as_str(), e))
            .collect();

        let n: f64 = present.values().map(|e| e.samples as f64).sum();
        let confidence = n / (n + self.confidence_k);
        let max_effective = present.values().map(|e| e.effective).fold(0.0f64, f64::max);

        for a in annotations.iter_mut() {
            let entry = a.resolve(groups);
            let combined = match &entry.history_key {
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
                    a.history_score = Some(h);
                    confidence * h + (1.0 - confidence) * a.base_score
                }
                None => a.base_score,
            };
            a.combined = combined;
        }
    }

    pub fn list_prefix_history(&self, prefix: &str) -> Vec<KeyedHistoryRecord> {
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
    fn boost_fans_out_like_launch() {
        let mut history = HistoryStore::default();
        history.record_boost("fi", "Finder", 10.0, 5);
        for prefix in ["", "f", "fi"] {
            let view = history.history_for(prefix);
            let entry = view.get("Finder").expect("boost reaches every prefix");
            assert_eq!(entry.samples, 5);
            assert!(entry.effective > 0.0);
        }
        assert!(history.history_for("fir").get("Finder").is_none());
    }

    #[test]
    fn delete_removes_association() {
        let mut history = HistoryStore::default();
        for _ in 0..5 {
            history.record_launch("f", "Firefox");
        }
        history.delete("f", "Firefox", DeleteScope::Everywhere, false);
        let confidence = confidence(&history, "f");
        assert_eq!(confidence, 0.0);
    }

    fn delete_fixture() -> HistoryStore {
        let mut history = HistoryStore::default();
        for query in ["fire", "firefox", "browser"] {
            history.record_launch(query, "Firefox");
        }
        history
    }

    fn has(history: &HistoryStore, prefix: &str, key: &str) -> bool {
        history
            .list_prefix_history(prefix)
            .iter()
            .any(|e| e.key == key)
    }

    #[test]
    fn delete_exact_only_keeps_other_buckets() {
        let mut history = delete_fixture();
        history.delete("fire", "Firefox", DeleteScope::Exact, false);

        assert!(!has(&history, "fire", "Firefox"));
        for prefix in ["", "f", "fi", "fir", "firefox", "browser"] {
            assert!(has(&history, prefix, "Firefox"), "kept {prefix:?}");
        }
    }

    #[test]
    fn delete_broader_keeps_narrower_and_unrelated_buckets() {
        let mut history = delete_fixture();
        history.delete("fire", "Firefox", DeleteScope::Broader, false);

        for prefix in ["", "f", "fi", "fir", "fire"] {
            assert!(!has(&history, prefix, "Firefox"), "deleted {prefix:?}");
        }
        assert!(has(&history, "firefox", "Firefox"));
        assert!(has(&history, "browser", "Firefox"));
    }

    #[test]
    fn delete_narrower_means_more_specific_not_just_longer() {
        let mut history = delete_fixture();
        history.record_launch("firefighter", "Firefox");
        history.delete("fire", "Firefox", DeleteScope::Narrower, false);

        for prefix in ["fire", "firefox", "firefighter"] {
            assert!(!has(&history, prefix, "Firefox"), "deleted {prefix:?}");
        }
        assert!(
            has(&history, "browser", "Firefox"),
            "browser is longer but not narrower than fire"
        );
        for prefix in ["", "f", "fi", "fir"] {
            assert!(has(&history, prefix, "Firefox"), "kept {prefix:?}");
        }
    }

    #[test]
    fn delete_everywhere_removes_key_from_all_buckets() {
        let mut history = delete_fixture();
        history.delete("fire", "Firefox", DeleteScope::Everywhere, false);

        for prefix in ["", "f", "fi", "fir", "fire", "firefox", "browser"] {
            assert!(!has(&history, prefix, "Firefox"), "deleted {prefix:?}");
        }
        assert!(history.known_history_keys().is_empty());
    }

    #[test]
    fn zero_boost_keeps_records_but_clears_their_scores() {
        let mut history = delete_fixture();
        history.record_boost("fire", "Firefox", 10.0, 5);
        history.delete("fire", "Firefox", DeleteScope::Exact, true);

        let view = history.history_for("fire");
        let entry = view.get("Firefox").expect("zeroing keeps the record");
        assert_eq!(entry.effective, 0.0);
        assert_eq!(entry.samples, 0);
        assert_eq!(confidence(&history, "fire"), 0.0);
        assert!(has(&history, "fire", "Firefox"));
        assert!(
            history
                .known_history_keys()
                .keys()
                .any(|key| key == "Firefox")
        );
    }

    #[test]
    fn list_prefix_history_returns_data() {
        let mut history = HistoryStore::default();
        history.record_launch("f", "Firefox");
        history.record_launch("f", "Firefox");
        let entries = history.list_prefix_history("f");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "Firefox");
        assert_eq!(entries[0].record.n, 2);
    }

    #[test]
    fn list_prefix_history_empty_for_unknown_prefix() {
        let history = HistoryStore::default();
        let entries = history.list_prefix_history("zzz");
        assert!(entries.is_empty());
    }

    /// One group of entries identified by their `id`, plus the un-scored
    /// annotations `history_scoring` fills in.
    fn groups_and_base(
        rows: &[(&str, f64, Option<&str>)],
    ) -> (Vec<QueryGroup>, Vec<EntryAnnotation>) {
        let mut entries = Vec::new();
        let mut annotations = Vec::new();
        for (index, (id, base_score, key)) in rows.iter().enumerate() {
            let mut builder = crate::engine::provider::entry(*id, *id);
            if let Some(key) = key {
                builder = builder.history_key(*key);
            }
            entries.push(builder.score(*base_score as f32));
            annotations.push(EntryAnnotation::new(0, index, *base_score));
        }
        let groups = vec![QueryGroup {
            query: String::new(),
            provider_id: "p".to_string(),
            entries: entries.into(),
        }];
        (groups, annotations)
    }

    fn scored_by_id<'a>(
        groups: &'a [QueryGroup],
        annotations: &'a [EntryAnnotation],
    ) -> Vec<(&'a str, &'a EntryAnnotation)> {
        annotations
            .iter()
            .map(|a| (a.resolve(groups).id.as_str(), a))
            .collect()
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
    fn known_history_keys_is_the_unscoped_global_key_list() {
        let mut history = HistoryStore::default();
        assert!(history.known_history_keys().is_empty());

        history.record_launch("/etc/ho", "files-/etc/hosts");
        history.record_launch("fire", "desktop-firefox");

        assert_eq!(
            history
                .known_history_keys()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                "desktop-firefox".to_string(),
                "files-/etc/hosts".to_string()
            ],
            "launches are visible regardless of the query they fanned out from"
        );
        assert!(
            history.history_for("notes").is_empty(),
            "precondition: the global list is not the scoped view"
        );
    }

    #[test]
    fn known_history_keys_follows_the_delete_scope() {
        let mut history = HistoryStore::default();
        history.record_launch("fi", "Firefox");

        history.delete("fi", "Firefox", DeleteScope::Exact, false);
        assert!(
            history
                .known_history_keys()
                .keys()
                .any(|key| key == "Firefox"),
            "a scoped delete leaves the global key index alone"
        );

        history.delete("fi", "Firefox", DeleteScope::Everywhere, false);
        assert!(
            !history
                .known_history_keys()
                .keys()
                .any(|key| key == "Firefox"),
            "deleting the global bucket removes the key from the unscoped view"
        );
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

        let (groups, mut annotations) =
            groups_and_base(&[("Gimp", 0.7, Some("Gimp")), ("Other", 0.6, None)]);
        history.history_scoring(&history.history_for("fi"), &groups, &mut annotations);

        let rows = scored_by_id(&groups, &annotations);
        let gimp = rows[0].1;
        let other = rows[1].1;
        assert_eq!(rows[0].0, "Gimp");
        assert_eq!(rows[1].0, "Other");

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

        let (groups, mut annotations) = groups_and_base(&[
            ("Firefox", 0.8, Some("Firefox")),
            ("Gimp", 0.7, Some("Gimp")),
            ("Other", 0.6, None),
        ]);
        history.history_scoring(&history.history_for("fi"), &groups, &mut annotations);

        let rows = scored_by_id(&groups, &annotations);
        let find = |id: &str| rows.iter().find(|(i, _)| *i == id).unwrap().1;
        let firefox = find("Firefox");
        let gimp = find("Gimp");
        let other = find("Other");

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
