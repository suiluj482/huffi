use nucleo::pattern::{Atom, AtomKind, CaseMatching, Normalization, Pattern};

use super::config::ScoringConfig;
use crate::engine::provider::QueryGroup;

use super::EntryAnnotation;
use crate::engine::provider::{MatchField, Rank};

/// A single fuzzy atom or a parsed multi-word pattern, behind one scoring API.
trait ScoredPattern {
    fn score(
        &self,
        fuzzy_matcher: &mut nucleo::Matcher,
        haystack: nucleo::Utf32Str<'_>,
    ) -> Option<u32>;
}

impl ScoredPattern for Atom {
    fn score(
        &self,
        fuzzy_matcher: &mut nucleo::Matcher,
        haystack: nucleo::Utf32Str<'_>,
    ) -> Option<u32> {
        Atom::score(self, haystack, fuzzy_matcher).map(u32::from)
    }
}

impl ScoredPattern for Pattern {
    fn score(
        &self,
        fuzzy_matcher: &mut nucleo::Matcher,
        haystack: nucleo::Utf32Str<'_>,
    ) -> Option<u32> {
        Pattern::score(self, haystack, fuzzy_matcher)
    }
}

pub struct BaseScorer {
    fuzzy_matcher: nucleo::Matcher,
    empty_query_score: f64,
    short_field_bonus: f64,
    split_query_words: bool,
}

impl Default for BaseScorer {
    fn default() -> Self {
        Self::new(&ScoringConfig::default())
    }
}

impl BaseScorer {
    /// Construct a scorer from the engine's ranking configuration.
    pub fn new(scoring: &ScoringConfig) -> Self {
        Self {
            fuzzy_matcher: nucleo::Matcher::new(nucleo::Config::DEFAULT),
            empty_query_score: scoring.empty_query_score,
            short_field_bonus: scoring.short_field_bonus,
            split_query_words: scoring.split_query_words,
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
        let mut base_annotations: Vec<EntryAnnotation> = Vec::new();
        let mut fuzzy: Vec<EntryAnnotation> = Vec::new();
        let mut haystack_buf: Vec<char> = Vec::new();
        for (group, g) in groups.iter().enumerate() {
            if g.query.is_empty() {
                base_annotations.extend(
                    (0..g.entries.len())
                        .map(|index| EntryAnnotation::new(group, index, self.empty_query_score)),
                );
                continue;
            }

            // Queries are matched through nucleo's pattern API rather than the
            // raw [`nucleo::Matcher`]: the matcher's `fuzzy_match` panics
            // ("should have been caught by prefilter") for some case-mismatched
            // needles the caller cannot predict, while atoms and patterns run
            // nucleo's own prefilter and return `None` instead.
            let atom;
            let pattern;
            let needle: &dyn ScoredPattern = if self.split_query_words {
                // Splits on spaces (`\ ` escapes one) and parses ^, ', $ and !.
                pattern = Pattern::parse(&g.query, CaseMatching::Ignore, Normalization::Smart);
                &pattern
            } else {
                atom = Atom::new(
                    &g.query,
                    CaseMatching::Ignore,
                    Normalization::Smart,
                    AtomKind::Fuzzy,
                    false,
                );
                &atom
            };
            let query_len = g.query.chars().count();

            for (index, entry) in g.entries.iter().enumerate() {
                match entry.rank {
                    Rank::Score(score) => {
                        base_annotations.push(EntryAnnotation::new(group, index, score as f64));
                    }
                    Rank::MatchFields(ref fields) => {
                        if let Some(r) = score_fields(
                            &mut self.fuzzy_matcher,
                            needle,
                            fields,
                            &mut haystack_buf,
                            query_len,
                            self.short_field_bonus,
                        ) && r > 0.0
                        {
                            fuzzy.push(EntryAnnotation::new(group, index, r));
                        }
                    }
                }
            }
        }

        normalize_in_place(&mut fuzzy);
        base_annotations.extend(fuzzy);
        base_annotations
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
    needle: &dyn ScoredPattern,
    field: &str,
    haystack_buf: &mut Vec<char>,
) -> Option<u32> {
    let haystack = nucleo::Utf32Str::new(field, haystack_buf);
    needle.score(fuzzy_matcher, haystack)
}

/// Score all match fields of one entry and return their weighted average.
///
/// `short_field_bonus` mildly favors entries with a match field whose length
/// is close to the query: the shortest matching field gets the full bonus
/// when it is no longer than the query, loses one point per extra character,
/// and gets nothing once it is longer by that many characters. Non-matching
/// fields are never rewarded.
///
/// The bonus is added once per entry, after the weighted average, rather than
/// to each field before weighting. Otherwise a field's weight would amplify
/// the preference — e.g. an exact shortcode match would outrank an exact name
/// match purely because shortcodes are weighted higher.
fn score_fields(
    fuzzy_matcher: &mut nucleo::Matcher,
    needle: &dyn ScoredPattern,
    fields: &[MatchField],
    haystack_buf: &mut Vec<char>,
    query_len: usize,
    short_field_bonus: f64,
) -> Option<f64> {
    if fields.is_empty() {
        return None;
    }

    let mut total = 0.0;
    let mut weight_sum = 0.0;
    let mut best_bonus: f64 = 0.0;

    for field in fields {
        let w = field.weight as f64;
        if w == 0.0 {
            continue;
        }
        weight_sum += w;
        if let Some(s) = score(fuzzy_matcher, needle, &field.text, haystack_buf) {
            total += s as f64 * w;
            let excess = field.text.chars().count().saturating_sub(query_len);
            let bonus = (short_field_bonus - excess as f64).max(0.0);
            best_bonus = best_bonus.max(bonus);
        }
    }

    if weight_sum == 0.0 {
        return None;
    }

    Some(total / weight_sum + best_bonus)
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

    /// Score fields with the length preference disabled, mirroring the
    /// behavior before `short_field_bonus` existed.
    fn score_fields_plain(
        fuzzy_matcher: &mut nucleo::Matcher,
        needle: &dyn ScoredPattern,
        fields: &[MatchField],
        haystack_buf: &mut Vec<char>,
    ) -> Option<f64> {
        score_fields(fuzzy_matcher, needle, fields, haystack_buf, 0, 0.0)
    }

    #[test]
    fn score_matches() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fi");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields_a = fields(&[("Firefox", 1.0)]);
        let fields_b = fields(&[("Gimp", 1.0)]);
        assert!(
            score_fields_plain(&mut fuzzy_matcher, &needle, &fields_a, &mut haystack_buf).unwrap()
                > 0.0
        );
        assert_eq!(
            score_fields_plain(&mut fuzzy_matcher, &needle, &fields_b, &mut haystack_buf),
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
            let scored =
                score_fields_plain(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf);
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
        assert!(
            score_fields_plain(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf).is_some()
        );
    }

    #[test]
    fn score_fields_returns_zero_when_no_field_matches() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("zzz");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields = fields(&[("Firefox", 1.0), ("Browse the Web", 0.5)]);
        assert_eq!(
            score_fields_plain(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf),
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
            score_fields_plain(&mut fuzzy_matcher, &needle, &fields_a, &mut haystack_buf).unwrap();
        let score_b =
            score_fields_plain(&mut fuzzy_matcher, &needle, &fields_b, &mut haystack_buf).unwrap();
        assert!(score_b > score_a);
    }

    #[test]
    fn score_fields_empty_returns_none() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fi");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields = fields(&[]);
        assert!(
            score_fields_plain(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf).is_none()
        );
    }

    #[test]
    fn score_fields_skips_zero_weight_fields() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fi");
        let mut haystack_buf: Vec<char> = Vec::new();
        let with_zero = fields(&[("Firefox", 1.0), ("Infinite nonsense", 0.0)]);
        let without = fields(&[("Firefox", 1.0)]);
        assert_eq!(
            score_fields_plain(&mut fuzzy_matcher, &needle, &with_zero, &mut haystack_buf),
            score_fields_plain(&mut fuzzy_matcher, &needle, &without, &mut haystack_buf),
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
            score_fields_plain(&mut fuzzy_matcher, &needle, &fields, &mut haystack_buf),
            None
        );
    }

    /// `Firefox XYZ` has no extra `e` after the match, so nucleo scores both
    /// fields identically and only the length bonus separates them.
    #[test]
    fn short_field_bonus_prefers_shorter_field() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fire");
        let mut haystack_buf: Vec<char> = Vec::new();
        let short = fields(&[("Firefox", 1.0)]);
        let long = fields(&[("Firefox XYZ", 1.0)]);

        let short_score = score_fields(
            &mut fuzzy_matcher,
            &needle,
            &short,
            &mut haystack_buf,
            4,
            6.0,
        )
        .unwrap();
        let long_score = score_fields(
            &mut fuzzy_matcher,
            &needle,
            &long,
            &mut haystack_buf,
            4,
            6.0,
        )
        .unwrap();
        assert!(short_score > long_score, "{short_score} vs {long_score}");
    }

    #[test]
    fn short_field_bonus_zero_keeps_raw_scoring() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fire");
        let mut haystack_buf: Vec<char> = Vec::new();
        let short = fields(&[("Firefox", 1.0)]);
        let long = fields(&[("Firefox XYZ", 1.0)]);

        let short_score = score_fields(
            &mut fuzzy_matcher,
            &needle,
            &short,
            &mut haystack_buf,
            4,
            0.0,
        )
        .unwrap();
        let long_score = score_fields(
            &mut fuzzy_matcher,
            &needle,
            &long,
            &mut haystack_buf,
            4,
            0.0,
        )
        .unwrap();
        assert_eq!(short_score, long_score);
    }

    #[test]
    fn short_field_bonus_does_not_reward_non_matches() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fire");
        let mut haystack_buf: Vec<char> = Vec::new();
        let unmatched = fields(&[("Gimp", 1.0)]);

        assert_eq!(
            score_fields(
                &mut fuzzy_matcher,
                &needle,
                &unmatched,
                &mut haystack_buf,
                4,
                6.0,
            ),
            Some(0.0)
        );
    }

    #[test]
    fn base_scoring_prefers_shorter_match_field() {
        let mut bs = BaseScorer::default();
        let groups = vec![group(
            "fire",
            vec![
                crate::engine::provider::entry("long", "Firefox XYZ")
                    .match_fields(fields(&[("Firefox XYZ", 1.0)])),
                crate::engine::provider::entry("short", "Firefox")
                    .match_fields(fields(&[("Firefox", 1.0)])),
            ],
        )];
        let annotations = bs.base_scoring(&groups);
        let score_of = |id: &str| {
            annotations
                .iter()
                .find(|a| a.resolve(&groups).id == id)
                .unwrap()
                .base_score
        };
        assert!(score_of("short") > score_of("long"));
    }

    /// The pattern the scorer builds when `split_query_words` is enabled.
    fn words(query: &str) -> Pattern {
        Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart)
    }

    #[test]
    fn word_splitting_requires_every_token() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = words("fire fox");
        let mut haystack_buf: Vec<char> = Vec::new();
        let both = fields(&[("Firefox", 1.0)]);
        let one = fields(&[("Firebird", 1.0)]);

        let both_score = score_fields(
            &mut fuzzy_matcher,
            &needle,
            &both,
            &mut haystack_buf,
            8,
            0.0,
        )
        .unwrap();
        let one_score =
            score_fields(&mut fuzzy_matcher, &needle, &one, &mut haystack_buf, 8, 0.0).unwrap();
        assert!(both_score > 0.0);
        assert_eq!(one_score, 0.0, "a missing word must not match");
    }

    #[test]
    fn word_splitting_off_keeps_the_whitespace_needle() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("fire fox");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields = fields(&[("Firefox", 1.0)]);

        assert_eq!(
            score_fields(
                &mut fuzzy_matcher,
                &needle,
                &fields,
                &mut haystack_buf,
                8,
                0.0,
            ),
            Some(0.0),
            "without splitting, the space is part of the fuzzy needle"
        );
    }

    #[test]
    fn word_splitting_parses_operators() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = words("^fire fox$");
        let mut haystack_buf: Vec<char> = Vec::new();
        let matching = fields(&[("Firefox", 1.0)]);
        let non_prefix = fields(&[("Mozilla Firefox", 1.0)]);

        let matching_score = score_fields(
            &mut fuzzy_matcher,
            &needle,
            &matching,
            &mut haystack_buf,
            10,
            0.0,
        )
        .unwrap();
        let non_prefix_score = score_fields(
            &mut fuzzy_matcher,
            &needle,
            &non_prefix,
            &mut haystack_buf,
            10,
            0.0,
        )
        .unwrap();
        assert!(matching_score > 0.0);
        assert_eq!(non_prefix_score, 0.0);
    }

    #[test]
    fn word_splitting_escaped_space_stays_one_token() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let escaped = words(r"fire\ fox");
        let split = words("fire fox");
        let mut haystack_buf: Vec<char> = Vec::new();
        let fields = fields(&[("Firefox", 1.0)]);

        assert_eq!(
            score_fields(
                &mut fuzzy_matcher,
                &escaped,
                &fields,
                &mut haystack_buf,
                9,
                0.0,
            ),
            Some(0.0),
            r"an escaped space keeps fire\ fox as one needle"
        );
        assert!(
            score_fields(
                &mut fuzzy_matcher,
                &split,
                &fields,
                &mut haystack_buf,
                8,
                0.0,
            )
            .unwrap()
                > 0.0
        );
    }

    #[test]
    fn base_scoring_can_split_query_words() {
        let config = ScoringConfig {
            split_query_words: true,
            short_field_bonus: 0.0,
            ..Default::default()
        };
        let mut bs = BaseScorer::new(&config);
        let groups = vec![group(
            "fire fox",
            vec![
                crate::engine::provider::entry("both", "Firefox")
                    .match_fields(fields(&[("Firefox", 1.0)])),
                crate::engine::provider::entry("one", "Firebird")
                    .match_fields(fields(&[("Firebird", 1.0)])),
            ],
        )];

        let annotations = bs.base_scoring(&groups);
        assert_eq!(
            annotations.len(),
            1,
            "only the entry matching all words survives"
        );
        assert_eq!(annotations[0].resolve(&groups).id, "both");
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

    /// Two rows with one exact field each were tied before the bonus. A
    /// heavier field must not turn that tie into a win: the preference is for
    /// short fields, not for short fields that happen to be weighted higher.
    #[test]
    fn short_field_bonus_is_independent_of_field_weight() {
        let mut fuzzy_matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let needle = atom("snowman");
        let mut haystack_buf: Vec<char> = Vec::new();
        // ☃️: exact name, long shortcode. ⛄: long name, exact shortcode.
        let name_match = fields(&[("snowman", 1.0), ("snowman_with_snow", 1.3), ("2603", 0.6)]);
        let shortcode_match = fields(&[
            ("snowman without snow", 1.0),
            ("snowman", 1.3),
            ("26c4", 0.6),
        ]);

        let a = score_fields(
            &mut fuzzy_matcher,
            &needle,
            &name_match,
            &mut haystack_buf,
            7,
            6.0,
        )
        .unwrap();
        let b = score_fields(
            &mut fuzzy_matcher,
            &needle,
            &shortcode_match,
            &mut haystack_buf,
            7,
            6.0,
        )
        .unwrap();
        assert_eq!(a, b, "{a} vs {b}");
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
        let annotations = bs.base_scoring(&groups);
        assert_eq!(
            annotations[0].base_score,
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
        let annotations = bs.base_scoring(&groups);
        let rank_of = |a: &EntryAnnotation| a.resolve(&groups).rank.clone();
        let score = annotations
            .iter()
            .find(|a| matches!(rank_of(a), Rank::Score(_)))
            .unwrap();
        let fuzzy = annotations
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
        let annotations = bs.base_scoring(&groups);
        assert_eq!(
            annotations.len(),
            1,
            "only the group matching its query survives"
        );
        assert_eq!(annotations[0].base_score, 1.0);
    }
}
