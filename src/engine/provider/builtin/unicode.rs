//! Unicode characters and emoji, looked up by name, shortcode, or code point.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Deserialize;

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry, parse_extra_config,
};
use crate::engine::scoring::MatchField;

/// Trigger prefix: `:snowman` copies ☃ to the clipboard.
const PREFIX: &str = ":";

/// Id of the one row shown while nothing has been typed after the prefix.
const PLACEHOLDER_ID: &str = "unicode-placeholder";

/// Per-provider tuning knobs. When provided via
/// `[engine.provider.builtin.unicode.extra]`, the fields are parsed from the
/// arbitrary extra config, mirroring [`super::DesktopConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default)]
pub struct UnicodeConfig {
    /// Fuzzy-match weight for the name Unicode gives the character.
    pub weight_name: f32,
    /// Fuzzy-match weight for each of an emoji's shortcodes, e.g. `smile` or
    /// `+1`. Weighted above the name because a shortcode is the token people
    /// actually type: `smile` means 😄, not the ☺ whose Unicode name happens
    /// to start with "smiling".
    pub weight_shortcode: f32,
    /// Fuzzy-match weight for the hex code point, e.g. `1f600`. A code point
    /// is a handful of characters competing against whole words, so it is a
    /// weak signal; `0.0` leaves the field out entirely, which also saves the
    /// engine a match to run on every row of every keystroke.
    pub weight_codepoint: f32,
}

impl Default for UnicodeConfig {
    fn default() -> Self {
        Self {
            weight_name: 1.0,
            weight_shortcode: 1.3,
            weight_codepoint: 0.6,
        }
    }
}

/// Provides Unicode characters and emoji, matched against the name Unicode
/// gives them, the emoji shortcodes they are known by, and their code point.
///
/// Triggered by the `:` prefix. Selecting a row found by name copies the
/// character; selecting one found by code point copies the number instead, so
/// both spellings are reachable from either. Either copy is recorded in history
/// like any other launch, so what actually gets pasted drifts to the top of
/// later queries.
///
/// The rows are built once in [`Provider::init`]. Enumerating every named
/// character takes long enough to belong in `init()` rather than in
/// [`Provider::query`].
pub struct UnicodeProvider {
    config: UnicodeConfig,
    /// `None` until `init()` builds it, so a provider that was never
    /// initialized answers nothing instead of enumerating Unicode mid-keystroke.
    characters: Option<Arc<Characters>>,
}

impl Default for UnicodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UnicodeProvider {
    pub fn new() -> Self {
        Self {
            config: UnicodeConfig::default(),
            characters: None,
        }
    }
}

impl Provider for UnicodeProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta::builder("unicode")
            .prefix(PREFIX)
            .prefix_only(true)
            .build()
    }

    fn init(&mut self, ctx: InitContext) -> ProviderResult {
        match parse_extra_config::<UnicodeConfig>(&ctx.extra) {
            Err(result) => return result,
            Ok(Some(config)) => self.config = config,
            Ok(None) => {}
        }
        self.characters = Some(Arc::new(Characters::build(self.config)));
        ProviderResult::Ok
    }

    fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
        let Some(characters) = &self.characters else {
            return vec![];
        };
        if ctx.query.is_empty() {
            return vec![placeholder()];
        }
        // A query that names a code point answers with that character and
        // nothing else: `:4e00` is one character, not a search for names that
        // happen to start with those digits. It is the one piece of matching
        // the provider does itself, because the engine's matcher would read
        // `u+1f600` as a typo rather than as the character it names.
        if let Some(hits) = characters.code_point_query(ctx.query) {
            return hits;
        }
        characters.rows.to_vec()
    }
}

/// The characters the provider knows: one row each for the engine to rank,
/// and a way to look a character up by code point.
struct Characters {
    rows: Vec<Entry>,
    /// Code point to the rows holding it. Emoji are indexed first, so a
    /// sequence claims the code points it is built from and `:2603` answers
    /// with ☃️ rather than with a bare ☃ as well.
    by_code_point: HashMap<u32, Vec<u32>>,
    /// Code points an emoji has already claimed.
    covered: HashSet<u32>,
}

impl Characters {
    fn build(config: UnicodeConfig) -> Self {
        let mut characters = Self {
            rows: Vec::new(),
            by_code_point: HashMap::new(),
            covered: HashSet::new(),
        };

        for emoji in emojis::iter() {
            let Some(ch) = emoji.as_str().chars().next() else {
                continue;
            };
            // An emoji is named by CLDR (`snowman`) at least as often as by
            // Unicode (`SNOWMAN WITH SNOW`), and the short name is the one a
            // person searching for it would recognise.
            let cldr = emoji.name();
            let name = if cldr.is_empty() {
                match unicode_names2::name(ch) {
                    Some(name) => name.to_string(),
                    None => continue,
                }
            } else {
                cldr.to_string()
            };
            let shortcodes: Vec<String> = emoji.shortcodes().map(str::to_string).collect();
            characters.add(config, ch, name, Some(emoji.as_str()), &shortcodes);
        }

        for code in 0..=char::MAX as u32 {
            let Some(ch) = char::from_u32(code) else {
                continue;
            };
            if characters.covered.contains(&code) {
                continue;
            }
            let Some(name) = unicode_names2::name(ch) else {
                continue;
            };
            let name = name.to_string();
            // The CJK and Hangul names are formulas rather than words —
            // `CJK UNIFIED IDEOGRAPH-4E00` — and there are over a hundred
            // thousand of them. Asking for one by code point still works.
            if name.starts_with("CJK") || name.starts_with("HANGUL") {
                continue;
            }
            characters.add(config, ch, name, None, &[]);
        }

        characters
    }

    /// Record one character: a row for the engine to rank, and every code
    /// point of the text that would be pasted for it.
    fn add(
        &mut self,
        config: UnicodeConfig,
        ch: char,
        name: String,
        sequence: Option<&str>,
        shortcodes: &[String],
    ) {
        let display = match sequence {
            Some(sequence) => sequence.to_string(),
            None => ch.to_string(),
        };
        // Every code point is listed in the id, because a flag and the same
        // regional indicator on its own are different characters to paste.
        let code_points: Vec<String> = display.chars().map(|c| format!("{:x}", c as u32)).collect();
        let id = format!("unicode-{}", code_points.join("-"));

        // The text is handed over as Unicode spells it: the engine's matcher
        // normalizes case on both sides, and every alias gets a field of its
        // own so that every spelling a person might type reaches the emoji.
        let mut fields = Vec::with_capacity(2 + shortcodes.len());
        fields.push(MatchField {
            text: name.clone(),
            weight: config.weight_name,
        });
        for shortcode in shortcodes {
            fields.push(MatchField {
                text: shortcode.clone(),
                weight: config.weight_shortcode,
            });
        }
        if config.weight_codepoint > 0.0 {
            fields.push(MatchField {
                text: format!("{:x}", ch as u32),
                weight: config.weight_codepoint,
            });
        }

        let row = self.rows.len() as u32;
        for c in display.chars() {
            self.by_code_point.entry(c as u32).or_default().push(row);
            self.covered.insert(c as u32);
        }

        // `Tab` walks the other way from a code point row, which offers the
        // name: this row offers the number, so `:snowman` and `:u+2603` are one
        // keystroke apart. Spelled `u+` because the bare form only reads as a
        // code point from `100` up, which would leave `§` and `A` unreachable.
        // For a sequence this is its first code point, and every sequence built
        // on it answers that number, so `Tab` on 🇩🇪 lists the flags.
        let code_point = format!("u+{:x}", ch as u32);

        self.rows.push(
            entry(id.clone(), display.clone())
                .subtitle(name)
                .clipboard(display)
                .history_key(id)
                .set_query(format!("{PREFIX}{code_point}"))
                .match_fields(fields),
        );
    }

    /// The rows for a query that names a code point outright: `2603`,
    /// `u+1f600`, `0x1f600`.
    ///
    /// `None` for a query that does not name one, which is every query that
    /// wants the engine to match it against the rows — and an empty answer for
    /// a code point nothing holds, since the query asked for that character and
    /// not for a search.
    ///
    /// A code point row shows the character and pastes the number. The
    /// character is already on screen, and what a person takes out of a lookup
    /// by number is the number itself — into a shell, a `printf`, a commit
    /// message. `Tab` offers the name, so both spellings are one keystroke away.
    ///
    /// Scored rather than matched: no name and no shortcode can fuzzy-match
    /// `2603`, so a row of match fields would score nothing and the engine would
    /// drop it.
    fn code_point_query(&self, query: &str) -> Option<Vec<Entry>> {
        let code = parse_code_point(query)?;
        let Some(rows) = self.by_code_point.get(&code) else {
            return Some(unindexed_character_hit(code));
        };
        let number = format!("{code:x}");
        Some(
            rows.iter()
                .map(|row| {
                    let row = &self.rows[*row as usize];
                    let (id, display) = (&row.entry.id, &row.entry.title);
                    let name = row.entry.subtitle.as_deref().unwrap_or_default();
                    entry(id.clone(), display.clone())
                        .subtitle(name)
                        .clipboard(number.clone())
                        .history_key(id.clone())
                        .set_query(format!("{PREFIX}{}", name.to_lowercase()))
                        .score(1.0)
                })
                .collect(),
        )
    }
}

/// The row for a character the rows leave out but the query names outright.
///
/// `U+4E00` is a CJK ideograph, and there are a hundred thousand of those in a
/// block whose names are formulas rather than words, so they are no rows to
/// rank. A query that asks for the character by number is still answered:
/// number it by hand and you mean exactly that character, which no amount of
/// name matching would have guessed.
///
/// Like every code point row it pastes the number and offers the name to `Tab`.
fn unindexed_character_hit(code: u32) -> Vec<Entry> {
    let Some(ch) = char::from_u32(code) else {
        return vec![];
    };
    let Some(name) = unicode_names2::name(ch) else {
        return vec![];
    };
    let name = name.to_string();
    let number = format!("{code:x}");
    let id = format!("unicode-{number}");
    vec![
        entry(id.clone(), ch.to_string())
            .subtitle(name.clone())
            .clipboard(number)
            .history_key(id)
            .set_query(format!("{PREFIX}{}", name.to_lowercase()))
            .score(1.0),
    ]
}

/// Read a query that names a code point outright: `u+1f600`, `U+1F600`,
/// `0x1f600`, or a bare `2603`.
///
/// A prefixed query is taken at its word — the prefix is a claim that the rest
/// is a number, so `u+face` is `U+FACE`. A bare one is only read as a code
/// point when it is short enough to be one and holds a digit, which is what
/// keeps `face` and `beef` names instead of numbers.
fn parse_code_point(query: &str) -> Option<u32> {
    let (digits, prefixed) = match strip_code_point_prefix(query) {
        Some(digits) => (digits, true),
        None => (query, false),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bare = (3..=6).contains(&digits.len()) && digits.chars().any(|c| c.is_ascii_digit());
    if !prefixed && !bare {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// Strip a `u+`, `U+`, `0x` or `0X` prefix, handing back the digits after it.
///
/// `None` for a query carrying no such prefix, including a bare `0` or a `u`
/// that no `+` follows — a lone `0` is a digit to search with, not a prefix.
fn strip_code_point_prefix(query: &str) -> Option<&str> {
    if let Some(rest) = query.strip_prefix(['u', 'U']) {
        return rest.strip_prefix('+');
    }
    query.strip_prefix('0')?.strip_prefix(['x', 'X'])
}

/// The one row shown when the prefix is typed and nothing after it: without it
/// the window is empty, and nothing says what the prefix does.
fn placeholder() -> Entry {
    entry(PLACEHOLDER_ID, "Search unicode characters")
        .subtitle("a name, a shortcode, or a code point")
        .score(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::Action;
    use crate::engine::scoring::Rank;

    /// Enumerating Unicode takes a moment, so it is done once for the module.
    fn characters() -> &'static Arc<Characters> {
        static CHARACTERS: std::sync::OnceLock<Arc<Characters>> = std::sync::OnceLock::new();
        CHARACTERS.get_or_init(|| Arc::new(Characters::build(UnicodeConfig::default())))
    }

    /// A provider over the shared rows, without paying for `init()` per test.
    fn provider() -> UnicodeProvider {
        UnicodeProvider {
            config: UnicodeConfig::default(),
            characters: Some(characters().clone()),
        }
    }

    fn query(provider: &mut UnicodeProvider, text: &str) -> Vec<Entry> {
        provider.query(QueryContext {
            prefix: Some(PREFIX),
            query: text,
            original: text,
        })
    }

    fn row_for(characters: &Characters, ch: char) -> &Entry {
        let rows = characters.by_code_point.get(&(ch as u32)).expect("a row");
        &characters.rows[rows[0] as usize]
    }

    fn titles(rows: &[Entry]) -> Vec<&str> {
        rows.iter().map(|row| row.entry.title.as_str()).collect()
    }

    fn field_texts(row: &Entry) -> Vec<&str> {
        match &row.rank {
            Rank::MatchFields(fields) => fields.iter().map(|f| f.text.as_str()).collect(),
            Rank::Score(_) => vec![],
        }
    }

    #[test]
    fn prefix_is_colon_and_gated_on_it() {
        let meta = provider().meta();
        assert_eq!(meta.id, "unicode");
        assert_eq!(meta.prefixes, vec![":"]);
        assert!(meta.prefix_only);
    }

    #[test]
    fn an_empty_query_offers_a_placeholder() {
        let rows = query(&mut provider(), "");
        assert_eq!(titles(&rows), vec!["Search unicode characters"]);
        assert_eq!(rows[0].entry.id, PLACEHOLDER_ID);
        assert_eq!(
            rows[0].entry.subtitle.as_deref(),
            Some("a name, a shortcode, or a code point")
        );
    }

    /// A name query is the engine's work: every row is handed over, and the
    /// engine keeps the ones that match.
    #[test]
    fn a_name_query_hands_over_every_row() {
        let mut provider = provider();
        for text in ["snowman", "face", "+1", "snowman_with_snow"] {
            assert_eq!(
                query(&mut provider, text).len(),
                characters().rows.len(),
                "for {text:?}"
            );
        }
    }

    #[test]
    fn a_code_point_query_answers_with_that_character() {
        let mut provider = provider();
        assert_eq!(titles(&query(&mut provider, "2603")), vec!["☃️"]);
        assert_eq!(titles(&query(&mut provider, "u+1f600")), vec!["😀"]);
        assert_eq!(titles(&query(&mut provider, "U+1F600")), vec!["😀"]);
    }

    /// `0x` is the spelling every other tool prints, so it is read too.
    #[test]
    fn a_code_point_query_reads_the_0x_prefix() {
        let mut provider = provider();
        assert_eq!(titles(&query(&mut provider, "0x2603")), vec!["☃️"]);
        assert_eq!(titles(&query(&mut provider, "0X1F600")), vec!["😀"]);
    }

    /// The CJK names are left out of the index, so this is the only way to
    /// reach 一 by name.
    #[test]
    fn a_code_point_query_reaches_a_character_the_names_leave_out() {
        assert_eq!(titles(&query(&mut provider(), "4e00")), vec!["一"]);
    }

    #[test]
    fn a_code_point_hit_is_scored_and_suggests_the_name() {
        let rows = query(&mut provider(), "2603");
        assert!(matches!(rows[0].rank, Rank::Score(_)));
        assert_eq!(rows[0].entry.set_query.as_deref(), Some(":snowman"));
    }

    #[test]
    fn a_hex_spelled_name_is_not_read_as_a_code_point() {
        for query in ["face", "beef", "cafe", "+1", "deadbeef"] {
            assert_eq!(parse_code_point(query), None, "for {query:?}");
        }
        for (query, code) in [
            ("2603", 0x2603),
            ("1f600", 0x1f600),
            ("u+1f600", 0x1f600),
            ("0x1f600", 0x1f600),
        ] {
            assert_eq!(parse_code_point(query), Some(code), "for {query:?}");
        }
    }

    /// A prefix is a claim that the rest is a number, so hex letters in a
    /// spelled-out query are digits. Only a *bare* query has to prove it.
    #[test]
    fn a_prefixed_query_may_be_pure_hex_letters() {
        assert_eq!(parse_code_point("u+face"), Some(0xface));
        assert_eq!(parse_code_point("0xface"), Some(0xface));
        assert_eq!(parse_code_point("face"), None);
    }

    /// A code point below `100` has no bare spelling that reads as one, which
    /// is why the suggestion spells it `u+`.
    #[test]
    fn a_low_code_point_is_reachable_through_the_suggestion() {
        let row = row_for(characters(), '§');
        assert_eq!(
            row.entry.set_query.as_deref(),
            Some(":u+a7"),
            "the suggestion must parse back as a code point"
        );
        assert_eq!(titles(&query(&mut provider(), "u+a7")), vec!["§"]);
    }

    #[test]
    fn an_unassigned_code_point_answers_with_nothing() {
        assert!(query(&mut provider(), "u+10ffff").is_empty());
    }

    /// A code point row pastes the number rather than the character: the
    /// character is on screen already, and the number is what a person looks up
    /// a code point for. The character is one `Tab` away, as its name.
    #[test]
    fn selecting_a_code_point_row_copies_the_number() {
        let rows = query(&mut provider(), "2603");
        let Action::Clipboard { value } = &rows[0].entry.action else {
            panic!("expected a clipboard action, got {:?}", rows[0].entry.action);
        };
        assert_eq!(value, "2603");
        assert_eq!(rows[0].entry.title, "☃️", "the character is still shown");
        assert_eq!(rows[0].entry.set_query.as_deref(), Some(":snowman"));
    }

    /// The mirror image: a row found by name pastes the character and offers
    /// the number to `Tab`.
    #[test]
    fn selecting_a_name_row_copies_the_character_and_suggests_the_number() {
        let row = row_for(characters(), '→');
        let Action::Clipboard { value } = &row.entry.action else {
            panic!("expected a clipboard action, got {:?}", row.entry.action);
        };
        assert_eq!(value, "→");
        assert_eq!(row.entry.set_query.as_deref(), Some(":u+2192"));
    }

    /// Every row's suggestion is a query the provider answers, at any code
    /// point: the round trip is the whole point of offering it.
    #[test]
    fn every_row_suggests_a_query_it_answers() {
        // A sequence suggests its first code point, which every sequence built
        // on it answers — so `Tab` on 🇩🇪 lists the flags rather than one flag.
        let flag = "🇩🇪".chars().next().expect("a code point");
        for ch in ['☃', '→', '§', '😀', '🐝', flag] {
            let suggestion = row_for(characters(), ch).entry.set_query.clone();
            let query = suggestion.expect("a suggestion");
            let query = query.strip_prefix(PREFIX).expect("prefixed");
            assert!(
                parse_code_point(query).is_some(),
                "{query:?} does not read as a code point"
            );
        }
    }

    #[test]
    fn every_character_carries_a_stable_history_key() {
        // The snowman's id spells out the whole text it pastes, variation
        // selector and all, so history keeps one key per pasted character.
        let row = row_for(characters(), '☃');
        assert_eq!(row.entry.id, "unicode-2603-fe0f");
        assert_eq!(row.history_key.as_deref(), Some("unicode-2603-fe0f"));
        let row = row_for(characters(), '→');
        assert_eq!(row.entry.id, "unicode-2192");
        assert_eq!(row.history_key.as_deref(), Some("unicode-2192"));
    }

    /// A regional indicator belongs to every flag built on it, and each of those
    /// pastes a different character, so each is a row with an id of its own.
    #[test]
    fn sequences_sharing_a_code_point_have_different_ids() {
        let rows = query(&mut provider(), "1f1e9");
        let ids: HashSet<&str> = rows.iter().map(|row| row.entry.id.as_str()).collect();
        assert_eq!(ids.len(), rows.len(), "no two flags share an id");
        for id in ["unicode-1f1e9-1f1ea", "unicode-1f1e9-1f1ff"] {
            assert!(ids.contains(id), "{id} in {ids:?}");
        }
        // The bare indicator is not a row: the flags claimed the code point.
        assert!(!ids.contains("unicode-1f1e9"), "{ids:?}");
    }

    /// An emoji is built from characters that have names of their own, and the
    /// sequence is the one worth pasting.
    #[test]
    fn an_emoji_stands_in_for_the_character_it_is_built_from() {
        assert_eq!(titles(&query(&mut provider(), "2603")), vec!["☃️"]);
        let rows = characters()
            .rows
            .iter()
            .filter(|row| row.entry.title == "☃")
            .count();
        assert_eq!(rows, 0, "the bare character is not a row of its own");
    }

    #[test]
    fn a_sequence_emoji_is_pasted_whole() {
        let rows = query(&mut provider(), "flag_de");
        let flag = rows.iter().find(|row| row.entry.id == "unicode-1f1e9-1f1ea");
        let Action::Clipboard { value } = &flag.expect("the German flag").entry.action else {
            panic!("expected a clipboard action");
        };
        assert_eq!(value, "🇩🇪");
    }

    /// Every alias is a field of its own, so `emojis`' second and third
    /// spelling reaches the emoji as well as the first.
    #[test]
    fn every_shortcode_of_an_emoji_is_a_field() {
        let row = row_for(characters(), '🐝');
        let fields = field_texts(row);
        for shortcode in ["bee", "honeybee"] {
            assert!(fields.contains(&shortcode), "{shortcode:?} in {fields:?}");
        }
        assert!(fields.contains(&"honeybee"), "the name is a field too");
    }

    #[test]
    fn a_row_matches_its_name_and_every_shortcode() {
        let row = row_for(characters(), '😀');
        assert_eq!(field_texts(row), vec!["grinning face", "grinning", "1f600"]);
    }

    /// The engine's matcher normalizes case on both sides, so the rows carry
    /// Unicode's spelling rather than a folded copy of it.
    #[test]
    fn a_name_is_carried_as_unicode_spells_it() {
        assert!(field_texts(row_for(characters(), '→')).contains(&"RIGHTWARDS ARROW"));
    }

    #[test]
    fn the_index_skips_the_cjk_and_hangul_names() {
        let rows = &characters().rows;
        assert!(!rows.iter().any(|row| row.entry.subtitle.as_deref() == Some("CJK UNIFIED IDEOGRAPH-4E00")));
        assert!(!rows.iter().any(|row| row.entry.subtitle.as_deref().unwrap_or_default().starts_with("HANGUL")));
    }

    #[test]
    fn the_index_covers_the_named_characters() {
        let rows = characters().rows.len();
        // Every named character outside the CJK and Hangul blocks, plus the
        // emoji sequences. An order of magnitude is the point here: a build
        // that silently indexed nothing would still pass a `> 0` assertion.
        assert!(rows > 30_000, "only {rows} rows");
        assert!(rows < 60_000, "{rows} rows");
    }

    #[test]
    fn no_character_is_indexed_twice() {
        let mut seen = HashSet::new();
        for row in &characters().rows {
            assert!(
                seen.insert(row.entry.id.clone()),
                "twice: {}",
                row.entry.id
            );
        }
    }

    #[test]
    fn weights_come_from_the_extra_config() {
        let config = UnicodeConfig {
            weight_name: 2.0,
            weight_shortcode: 3.0,
            weight_codepoint: 0.0,
        };
        let characters = Characters::build(config);
        let Rank::MatchFields(fields) = &row_for(&characters, '😀').rank else {
            panic!("expected match fields");
        };
        assert_eq!(fields[0].text, "grinning face");
        assert_eq!(fields[0].weight, 2.0);
        assert_eq!(fields[1].text, "grinning");
        assert_eq!(fields[1].weight, 3.0);
        // A weight of zero leaves the field out, so the engine is not asked to
        // match the code point on every row of every keystroke.
        assert_eq!(fields.len(), 2);
    }

    /// The index is left with the defaults and the error is non-critical, which is
    /// how every provider reports a config it could not read.
    #[test]
    fn an_invalid_extra_config_keeps_the_defaults() {
        let mut provider = UnicodeProvider::new();
        let result = provider.init(InitContext {
            data_dir: &std::env::temp_dir(),
            extra: Some(serde_json::json!({ "weight_name": "not a number" })),
        });
        assert!(!matches!(result, ProviderResult::Config { critical: true, .. }));
        assert_eq!(provider.config, UnicodeConfig::default());
    }

    #[test]
    fn extra_config_is_applied_on_init() {
        let mut provider = UnicodeProvider::new();
        provider
            .init(InitContext {
                data_dir: &std::env::temp_dir(),
                extra: Some(serde_json::json!({ "weight_shortcode": 4.0 })),
            });
        assert_eq!(provider.config.weight_shortcode, 4.0);
    }

    #[test]
    fn a_provider_that_was_never_initialized_answers_nothing() {
        let mut provider = UnicodeProvider::new();
        assert!(query(&mut provider, "snowman").is_empty());
        assert!(query(&mut provider, "").is_empty());
    }

    /// What Unicode's names start with, which is how the index decides what to
    /// leave out: run with `cargo test probe_name_prefixes -- --ignored
    /// --nocapture`.
    #[test]
    #[ignore]
    fn probe_name_prefixes() {
        let mut total = 0;
        let mut prefixes: std::collections::BTreeMap<String, usize> = Default::default();
        for code in 0..=char::MAX as u32 {
            let Some(ch) = char::from_u32(code) else {
                continue;
            };
            let Some(name) = unicode_names2::name(ch) else {
                continue;
            };
            total += 1;
            let head: String = name.to_string().chars().take(24).collect();
            let key = if head.starts_with("CJK") {
                "CJK*".to_string()
            } else if head.starts_with("HANGUL") {
                "HANGUL*".to_string()
            } else {
                head.clone()
            };
            *prefixes.entry(key).or_default() += 1;
        }
        println!("total named = {total}");
        for (k, v) in prefixes.iter().filter(|(_, v)| **v > 500).rev() {
            println!("{v:>7}  {k}");
        }
        for c in ['\u{2603}', '\u{2260}', '\u{3b1}', '\u{4e00}', '\u{fb33}'] {
            println!("{:?} {:?} -> {:?}", c as u32, c, unicode_names2::name(c));
        }
    }

    /// Profiling harness (run: `cargo test --release profile_unicode -- --ignored
    /// --nocapture`). The rows are handed to the engine whole, so the
    /// interesting number is what one query costs before the engine ranks
    /// anything.
    #[test]
    #[ignore]
    fn profile_unicode_keystroke_cost() {
        use std::time::Instant;

        let t = Instant::now();
        let characters = Arc::new(Characters::build(UnicodeConfig::default()));
        eprintln!(
            "[prof] built {} rows in {:?}",
            characters.rows.len(),
            t.elapsed()
        );
        let mut provider = UnicodeProvider {
            config: UnicodeConfig::default(),
            characters: Some(characters),
        };
        for text in ["", "s", "sm", "smo", "snow", "snowman", "smile", "be", "heart", "2603", "u+1f600", "+1", "zzzz"] {
            let t = Instant::now();
            let rows = query(&mut provider, text);
            eprintln!(
                "[prof] query {text:>8}: {} rows in {:?}",
                rows.len(),
                t.elapsed()
            );
        }
    }
}