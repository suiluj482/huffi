use super::{BaseScored, MatchField, Rank, Scoreable};

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

    pub fn base_scoring<T>(&mut self, entries: Vec<Scoreable<T>>, query: &str) -> Vec<BaseScored<T>> {
        if query.is_empty() {
            return entries
                .into_iter()
                .map(|s| BaseScored {
                    entry: s.entry,
                    rank: s.rank,
                    history_key: s.history_key,
                    base_score: EMPTY_QUERY_SCORE,
                })
                .collect();
        }

        let mut needle_buf = Vec::new();
        let needle = nucleo::Utf32Str::new(query, &mut needle_buf);

        let scored: Vec<(Scoreable<T>, f64)> = entries
            .into_iter()
            .filter_map(|s| {
                let raw = match &s.rank {
                    Rank::MatchFields(fields) => score_fields(&mut self.fuzzy_matcher, needle, fields)?,
                    Rank::Score(score) => *score as f64,
                };
                if raw > 0.0 {
                    Some((s, raw))
                } else {
                    None
                }
            })
            .collect();

        let max_raw = scored.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
        if max_raw <= 0.0 {
            return vec![];
        }

        scored
            .into_iter()
            .map(|(s, raw)| BaseScored {
                entry: s.entry,
                rank: s.rank,
                history_key: s.history_key,
                base_score: raw / max_raw,
            })
            .collect()
    }
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
        assert_eq!(score_fields(&mut fuzzy_matcher, needle, &fields_b), Some(0.0));
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
}
