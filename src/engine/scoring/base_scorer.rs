use super::{BaseScored, MatchField, QueryGroup, Rank, Scoreable};

pub const EMPTY_QUERY_SCORE: f64 = 0.8;

pub struct BaseScorer {
    fuzzy_matcher: nucleo::Matcher,
}

impl Default for BaseScorer {
    fn default() -> Self {
        BaseScorer {
            fuzzy_matcher: nucleo::Matcher::new(nucleo::Config::DEFAULT),
        }
    }
}

impl BaseScorer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Score each group against its own query, then normalize globally.
    ///
    /// [`Rank::Score`] entries are moved straight onto the result (providers
    /// promise a score in `0.0..=1.0`, so no normalization applies).
    /// [`Rank::MatchFields`] entries are fuzzy-scored and normalized against
    /// the best fuzzy match in the whole batch. Entries from a group with an
    /// empty query keep the empty-query baseline and are exempt as well.
    pub fn base_scoring<T>(&mut self, groups: Vec<QueryGroup<T>>) -> Vec<BaseScored<T>> {
        let mut base_scored = Vec::new();
        let mut raw = Vec::new();
        for g in groups {
            if g.query.is_empty() {
                base_scored.extend(g.entries.into_iter().map(|s| BaseScored {
                    entry: s.entry,
                    rank: s.rank,
                    history_key: s.history_key,
                    base_score: EMPTY_QUERY_SCORE,
                }));
                continue;
            }

            let mut needle_buf = Vec::new();
            let needle = nucleo::Utf32Str::new(&g.query, &mut needle_buf);

            for s in g.entries {
                match s.rank {
                    Rank::Score(score) => base_scored.push(BaseScored {
                        entry: s.entry,
                        rank: Rank::Score(score),
                        history_key: s.history_key,
                        base_score: score as f64,
                    }),
                    Rank::MatchFields(fields) => {
                        if let Some(r) = score_fields(&mut self.fuzzy_matcher, needle, &fields)
                            && r > 0.0
                        {
                            raw.push((
                                Scoreable {
                                    entry: s.entry,
                                    rank: Rank::MatchFields(fields),
                                    history_key: s.history_key,
                                },
                                r,
                            ));
                        }
                    }
                }
            }
        }

        base_scored.extend(normalize(raw));
        base_scored
    }
}

/// Normalize raw fuzzy scores against the batch maximum.
///
/// Only [`Rank::MatchFields`] entries reach this step; their base score is
/// the raw score divided by the maximum across the whole batch, so results
/// from different providers remain comparable.
pub fn normalize<T>(raw: Vec<(Scoreable<T>, f64)>) -> Vec<BaseScored<T>> {
    let max = raw.iter().map(|(_, r)| *r).fold(0.0f64, f64::max);

    raw.into_iter()
        .map(|(s, r)| BaseScored {
            entry: s.entry,
            rank: s.rank,
            history_key: s.history_key,
            base_score: if max > 0.0 { r / max } else { 0.0 },
        })
        .collect()
}

fn score(
    fuzzy_matcher: &mut nucleo::Matcher,
    needle: nucleo::Utf32Str<'_>,
    field: &str,
) -> Option<u16> {
    let mut haystack_buf = Vec::new();
    let haystack = nucleo::Utf32Str::new(field, &mut haystack_buf);
    fuzzy_matcher.fuzzy_indices(haystack, needle, &mut Vec::new())
}

fn score_fields(
    fuzzy_matcher: &mut nucleo::Matcher,
    needle: nucleo::Utf32Str<'_>,
    fields: &[MatchField],
) -> Option<f64> {
    if fields.is_empty() {
        return None;
    }

    let mut total = 0.0;
    let mut weight_sum = 0.0;

    for field in fields {
        let w = field.weight as f64;
        weight_sum += w;
        if let Some(s) = score(fuzzy_matcher, needle, &field.text) {
            total += s as f64 * w;
        }
    }

    if weight_sum == 0.0 {
        return None;
    }

    Some(total / weight_sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, f32)]) -> Vec<MatchField> {
        pairs
            .iter()
            .map(|(text, weight)| MatchField {
                text: text.to_string(),
                weight: *weight,
            })
            .collect()
    }

    #[test]
    fn score_matches() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let mut pattern_buf = Vec::new();
        let needle = nucleo::Utf32Str::new("fi", &mut pattern_buf);
        let fields_a = fields(&[("Firefox", 1.0)]);
        let fields_b = fields(&[("Gimp", 1.0)]);
        assert!(score_fields(&mut fuzzy_matcher, needle, &fields_a).unwrap() > 0.0);
        assert_eq!(
            score_fields(&mut fuzzy_matcher, needle, &fields_b),
            Some(0.0)
        );
    }

    #[test]
    fn score_fields_matches_independent_fields() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let mut pattern_buf = Vec::new();
        let needle = nucleo::Utf32Str::new("fi", &mut pattern_buf);
        let fields = fields(&[("Firefox", 1.0), ("Browse the Web", 0.5)]);
        assert!(score_fields(&mut fuzzy_matcher, needle, &fields).is_some());
    }

    #[test]
    fn score_fields_returns_zero_when_no_field_matches() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let mut pattern_buf = Vec::new();
        let needle = nucleo::Utf32Str::new("zzz", &mut pattern_buf);
        let fields = fields(&[("Firefox", 1.0), ("Browse the Web", 0.5)]);
        assert_eq!(score_fields(&mut fuzzy_matcher, needle, &fields), Some(0.0));
    }

    #[test]
    fn score_fields_weighted_average() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let mut pattern_buf = Vec::new();
        let needle = nucleo::Utf32Str::new("fi", &mut pattern_buf);
        let fields_a = fields(&[("Firefox", 1.0), ("Unrelated", 1.0)]);
        let fields_b = fields(&[("Firefox", 1.0), ("Fireshot", 1.0)]);
        let score_a = score_fields(&mut fuzzy_matcher, needle, &fields_a).unwrap();
        let score_b = score_fields(&mut fuzzy_matcher, needle, &fields_b).unwrap();
        assert!(score_b > score_a);
    }

    #[test]
    fn score_fields_empty_returns_none() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let mut pattern_buf = Vec::new();
        let needle = nucleo::Utf32Str::new("fi", &mut pattern_buf);
        let fields = fields(&[]);
        assert!(score_fields(&mut fuzzy_matcher, needle, &fields).is_none());
    }

    fn scoreable<T>(entry: T, rank: Rank) -> Scoreable<T> {
        Scoreable {
            entry,
            rank,
            history_key: None,
        }
    }

    #[test]
    fn normalize_uses_global_max_for_match_fields() {
        let mf = |v: f32| {
            scoreable(
                (),
                Rank::MatchFields(vec![MatchField {
                    text: "x".into(),
                    weight: v,
                }]),
            )
        };
        let raw = vec![(mf(1.0), 300.0), (mf(2.0), 100.0)];
        let scored = normalize(raw);
        assert_eq!(scored[0].base_score, 1.0);
        assert!((scored[1].base_score - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_empty_returns_empty() {
        assert!(normalize::<()>(vec![]).is_empty());
    }

    #[test]
    fn base_scoring_empty_query_group_keeps_baseline() {
        let mut bs = BaseScorer::new();
        let groups = vec![QueryGroup {
            query: String::new(),
            entries: vec![scoreable((), Rank::Score(1.0))],
        }];
        let scored = bs.base_scoring(groups);
        assert_eq!(scored[0].base_score, EMPTY_QUERY_SCORE);
    }

    #[test]
    fn base_scoring_score_entries_bypass_normalization() {
        let mut bs = BaseScorer::new();
        let groups = vec![QueryGroup {
            query: "fi".into(),
            entries: vec![
                scoreable((), Rank::Score(0.5)),
                scoreable((), Rank::MatchFields(fields(&[("Firefox", 1.0)]))),
            ],
        }];
        let scored = bs.base_scoring(groups);
        let score = scored
            .iter()
            .find(|s| matches!(s.rank, Rank::Score(_)))
            .unwrap();
        let fuzzy = scored
            .iter()
            .find(|s| matches!(s.rank, Rank::MatchFields(_)))
            .unwrap();
        assert_eq!(
            score.base_score, 0.5,
            "Score entries must not be normalized"
        );
        assert_eq!(fuzzy.base_score, 1.0);
    }

    #[test]
    fn base_scoring_per_group_query() {
        let mut bs = BaseScorer::new();
        let groups = vec![
            QueryGroup {
                query: "fi".into(),
                entries: vec![scoreable(
                    (),
                    Rank::MatchFields(fields(&[("Firefox", 1.0)])),
                )],
            },
            QueryGroup {
                query: "==fi".into(),
                entries: vec![scoreable(
                    (),
                    Rank::MatchFields(fields(&[("Firefox", 1.0)])),
                )],
            },
        ];
        let scored = bs.base_scoring(groups);
        assert_eq!(
            scored.len(),
            1,
            "only the group matching its query survives"
        );
        assert_eq!(scored[0].base_score, 1.0);
    }
}
