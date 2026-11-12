use nucleo::pattern::{Atom, AtomKind, CaseMatching, Normalization};

use super::config::ScoringConfig;
use crate::engine::provider::QueryGroup;

use super::{EntryAnnotation, MatchField, Rank};

pub struct BaseScorer {
    fuzzy_matcher: nucleo::Matcher,
    empty_query_score: f64,
}

impl Default for BaseScorer {
    fn default() -> Self {
        Self::new(ScoringConfig::default().empty_query_score)
    }
}

impl BaseScorer {
    /// Construct a scorer that assigns `empty_query_score` to entries
    /// queried with an empty query.
    pub fn new(empty_query_score: f64) -> Self {
        Self {
            fuzzy_matcher: nucleo::Matcher::new(nucleo::Config::DEFAULT),
            empty_query_score,
        }
    }

    /// Score each group against its own query, then normalize globally.
    ///
    /// [`Rank::Score`] entries are moved straight onto the result (providers
    /// promise a score in `0.0..=1.0`, so no normalization applies).
    /// [`Rank::MatchFields`] entries are fuzzy-scored and normalized against
    /// the best fuzzy match in the whole batch. Entries from a group with an
    /// empty query keep the empty-query baseline and are exempt as well.
    ///
    /// Entries are read in place and referenced by position, so nothing here
    /// moves or copies an [`Entry`](crate::engine::provider::Entry).
    pub fn base_scoring(&mut self, groups: &[QueryGroup]) -> Vec<EntryAnnotation> {
        let mut base_scored: Vec<EntryAnnotation> = Vec::new();
        let mut fuzzy: Vec<EntryAnnotation> = Vec::new();
        let mut haystack_buf: Vec<char> = Vec::new();
        for (group, g) in groups.iter().enumerate() {
            let group = group as u32;
            if g.query.is_empty() {
                base_scored.extend(
                    (0..g.entries.len() as u32)
                        .map(|index| EntryAnnotation::new(group, index, self.empty_query_score)),
                );
                continue;
            }

            // The query is matched through a fuzzy [`Atom`] rather than the
            // raw [`nucleo::Matcher`]: the matcher's `fuzzy_match` panics
            // ("should have been caught by prefilter") for some case-mismatched
            // needles the caller cannot predict, while an atom runs nucleo's
            // own prefilter and returns `None` instead.
            let needle = Atom::new(
                &g.query,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
                false,
            );

            for (index, entry) in g.entries.iter().enumerate() {
                let index = index as u32;
                match entry.rank {
                    Rank::Score(score) => {
                        base_scored.push(EntryAnnotation::new(group, index, score as f64));
                    }
                    Rank::MatchFields(ref fields) => {
                        if let Some(r) = score_fields(
                            &mut self.fuzzy_matcher,
                            &needle,
                            fields,
                            &mut haystack_buf,
                        ) && r > 0.0
                        {
                            fuzzy.push(EntryAnnotation::new(group, index, r));
                        }
                    }
                }
            }
        }

        normalize_in_place(&mut fuzzy);
        base_scored.extend(fuzzy);
        base_scored
    }
}

/// Normalize raw fuzzy scores against the batch maximum, in place.
///
/// Only [`Rank::MatchFields`] entries reach this step; their base score is
/// the raw score divided by the maximum across the whole batch, so results
/// from different providers remain comparable.
fn normalize_in_place(fuzzy: &mut [EntryAnnotation]) {
    let max = fuzzy
        .iter()
        .map(|annotation| annotation.base_score)
        .fold(0.0f64, f64::max);
    for annotation in fuzzy.iter_mut() {
        annotation.base_score = if max > 0.0 {
            annotation.base_score / max
        } else {
            0.0
        };
        annotation.combined = annotation.base_score;
    }
}

fn score(
    fuzzy_matcher: &mut nucleo::Matcher,
    needle: &Atom,
    field: &str,
    haystack_buf: &mut Vec<char>,
) -> Option<u16> {
    let haystack = nucleo::Utf32Str::new(field, haystack_buf);
    needle.score(haystack, fuzzy_matcher)
}

fn score_fields(
    fuzzy_matcher: &mut nucleo::Matcher,
    needle: &Atom,
    fields: &[MatchField],
    haystack_buf: &mut Vec<char>,
) -> Option<f64> {
    if fields.is_empty() {
        return None;
    }

    let mut total = 0.0;
    let mut weight_sum = 0.0;

    for field in fields {
        let w = field.weight as f64;
        if w == 0.0 {
            continue;
        }
        weight_sum += w;
        if let Some(s) = score(fuzzy_matcher, needle, &field.text, haystack_buf) {
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

    /// The needle atom a query is matched through, mirroring the scorer.
    fn atom(query: &str) -> Atom {
        Atom::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        )
    }

    #[test]
    fn score_matches() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fi");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields_a = fields(&[("Firefox", 1.0)]);
        let fields_b = fields(&[("Gimp", 1.0)]);
        assert!(
            score_fields(&mut fuzzy_matcher, &needle, &fields_a, &mut haystack_buf).unwrap() > 0.0
        );
        assert_eq!(
            score_fields(&mut fuzzy_matcher, &needle, &fields_b, &mut haystack_buf),
            Some(0.0)
        );
    }

    /// A needle whose casing differs from the haystack used to reach
    /// `nucleo`'s raw matcher and panic with "should have been caught by
    /// prefilter". Matching through an atom must return a score instead.
    #[test]
    fn case_mismatched_needle_does_not_panic() {
        for (query, text) in [("Aa", "AAA"), ("aBc", "ABC"), ("FI", "fi"), ("fI", "Fi")] {
            let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
            let needle = atom(query);
            let mut haystack_buf: Vec<char> = Vec::new();
            let fields = fields(&[(text, 1.0)]);
            let scored = score_fields(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf);
            assert!(
                scored.unwrap_or(0.0) > 0.0,
                "{query:?} should match {text:?}, got {scored:?}"
            );
        }
    }

    #[test]
    fn score_fields_matches_independent_fields() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fi");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields = fields(&[("Firefox", 1.0), ("Browse the Web", 0.5)]);
        assert!(score_fields(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf).is_some());
    }

    #[test]
    fn score_fields_returns_zero_when_no_field_matches() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("zzz");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields = fields(&[("Firefox", 1.0), ("Browse the Web", 0.5)]);
        assert_eq!(
            score_fields(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf),
            Some(0.0)
        );
    }

    #[test]
    fn score_fields_weighted_average() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fi");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields_a = fields(&[("Firefox", 1.0), ("Unrelated", 1.0)]);
        let fields_b = fields(&[("Firefox", 1.0), ("Fireshot", 1.0)]);
        let score_a =
            score_fields(&mut fuzzy_matcher, &needle, &fields_a, &mut haystack_buf).unwrap();
        let score_b =
            score_fields(&mut fuzzy_matcher, &needle, &fields_b, &mut haystack_buf).unwrap();
        assert!(score_b > score_a);
    }

    #[test]
    fn score_fields_empty_returns_none() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fi");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields = fields(&[]);
        assert!(score_fields(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf).is_none());
    }

    #[test]
    fn score_fields_skips_zero_weight_fields() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fi");
        let mut haystack_buf: Vec<char> = Vec::new();
        let with_zero = fields(&[("Firefox", 1.0), ("Infinite nonsense", 0.0)]);
        let without = fields(&[("Firefox", 1.0)]);
        assert_eq!(
            score_fields(&mut fuzzy_matcher, &needle, &with_zero, &mut haystack_buf),
            score_fields(&mut fuzzy_matcher, &needle, &without, &mut haystack_buf),
            "a zero-weight field must not influence the score"
        );
    }

    #[test]
    fn score_fields_all_zero_returns_none() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fi");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields = fields(&[("Firefox", 0.0), ("Zzz", 0.0)]);
        assert_eq!(
            score_fields(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf),
            None
        );
    }

    fn ranked(rank: Rank) -> crate::engine::provider::Entry {
        match rank {
            Rank::Score(score) => crate::engine::provider::entry("e", "t").score(score),
            Rank::MatchFields(f) => crate::engine::provider::entry("e", "t").match_fields(f),
        }
    }

    fn group(query: &str, entries: Vec<crate::engine::provider::Entry>) -> QueryGroup {
        QueryGroup {
            query: query.to_string(),
            provider_id: "p".to_string(),
            entries: entries.into(),
        }
    }

    #[test]
    fn normalize_uses_global_max_for_match_fields() {
        let mut fuzzy = vec![
            EntryAnnotation::new(0, 0, 300.0),
            EntryAnnotation::new(1, 0, 100.0),
        ];
        normalize_in_place(&mut fuzzy);
        assert_eq!(fuzzy[0].base_score, 1.0);
        assert!((fuzzy[1].base_score - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(fuzzy[0].combined, 1.0);
        assert!((fuzzy[1].combined - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_empty_returns_empty() {
        let mut fuzzy: Vec<EntryAnnotation> = vec![];
        normalize_in_place(&mut fuzzy);
        assert!(fuzzy.is_empty());
    }

    #[test]
    fn base_scoring_empty_query_group_keeps_baseline() {
        let mut bs = BaseScorer::default();
        let groups = vec![group("", vec![ranked(Rank::Score(1.0))])];
        let scored = bs.base_scoring(&groups);
        assert_eq!(
            scored[0].base_score,
            ScoringConfig::default().empty_query_score
        );
    }

    #[test]
    fn base_scoring_score_entries_bypass_normalization() {
        let mut bs = BaseScorer::default();
        let groups = vec![group(
            "fi",
            vec![
                ranked(Rank::Score(0.5)),
                ranked(Rank::MatchFields(fields(&[("Firefox", 1.0)]))),
            ],
        )];
        let scored = bs.base_scoring(&groups);
        let rank_of = |a: &EntryAnnotation| {
            groups[a.group as usize].entries[a.index as usize]
                .rank
                .clone()
        };
        let score = scored
            .iter()
            .find(|a| matches!(rank_of(a), Rank::Score(_)))
            .unwrap();
        let fuzzy = scored
            .iter()
            .find(|a| matches!(rank_of(a), Rank::MatchFields(_)))
            .unwrap();
        assert_eq!(
            score.base_score, 0.5,
            "Score entries must not be normalized"
        );
        assert_eq!(fuzzy.base_score, 1.0);
    }

    #[test]
    fn base_scoring_per_group_query() {
        let mut bs = BaseScorer::default();
        let groups = vec![
            group(
                "fi",
                vec![ranked(Rank::MatchFields(fields(&[("Firefox", 1.0)])))],
            ),
            group(
                "==fi",
                vec![ranked(Rank::MatchFields(fields(&[("Firefox", 1.0)])))],
            ),
        ];
        let scored = bs.base_scoring(&groups);
        assert_eq!(
            scored.len(),
            1,
            "only the group matching its query survives"
        );
        assert_eq!(scored[0].base_score, 1.0);
    }
}
