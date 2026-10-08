use std::collections::BTreeMap;

use rink_core::output::{NumberParts, QueryReply};
use rink_core::{eval, simple_context};

use crate::engine::provider::{
    Entry, Icon, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
};

/// Id shared by every entry this provider returns, and the key its single
/// stable result is tracked under in history. It is the provider id itself:
/// with one entry there is nothing to disambiguate, so the key is just the
/// namespace.
const ENTRY_ID: &str = "calculator";

/// Shown when the user has typed the `=` prefix but nothing after it.
const PLACEHOLDER: &str = "type to calculate";

/// Every detail key this provider can produce, across all result kinds.
///
/// These are the fields that don't fit in a row's title, subtitle, or comment:
/// structured values a theme may lay out however it likes. A theme declares the
/// subset it wants rendered by adding a `detail-<key>` widget to its calculator
/// template; anything not declared is simply not shown, so emitting a field a
/// theme ignores costs nothing. Exposed so a template can be checked against the
/// keys that actually exist rather than a copy of them.
///
/// Deliberately absent: an exact or approximate reading of a number, which the
/// title already *is*, and prose, which is the comment.
pub const DETAIL_KEYS: &[&str] = &["quantity", "dimensions", "properties", "def"];

pub struct CalculatorProvider {
    rink: Option<rink_core::Context>,
    icon: Option<Icon>,
}

impl Default for CalculatorProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CalculatorProvider {
    pub fn new() -> Self {
        Self {
            rink: None,
            icon: None,
        }
    }
}

impl Provider for CalculatorProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta::builder("calculator")
            .prefix("=")
            .prefix_only(true)
            .build()
    }

    fn init(&mut self, _ctx: InitContext) -> ProviderResult {
        match simple_context() {
            Ok(rink) => self.rink = Some(rink),
            Err(e) => return ProviderResult::Other(format!("failed to init rink context: {e}")),
        }
        self.icon = Some(Icon::Name("accessories-calculator".into()));
        ProviderResult::Ok
    }

    fn query(&mut self, ctx: QueryContext) -> Vec<Entry> {
        let Some(rink) = self.rink.as_mut() else {
            return vec![];
        };

        if ctx.query.is_empty() {
            // The placeholder is a value like any other, so it is copied and
            // re-queried on selection just as a result would be.
            return vec![self.row(PLACEHOLDER).score(1.0)];
        }

        match eval(rink, ctx.query) {
            Ok(reply) => vec![self.present(&reply)],
            // A failed expression is a legitimate result: the error becomes the
            // title, and there is nothing to copy or re-query.
            Err(err) => vec![self.base(&err.to_string()).score(1.0)],
        }
    }
}

impl CalculatorProvider {
    /// The common part of every entry: stable id, history key, and icon.
    fn base(&self, title: &str) -> crate::engine::provider::EntryBuilder {
        let mut e = entry(ENTRY_ID, title).history_key(ENTRY_ID);
        if let Some(icon) = &self.icon {
            e = e.icon(icon.clone());
        }
        e
    }

    /// Arrange one rink result into a row.
    ///
    /// A result's own `Display` is a complete *sentence* — `water: <doc> {…}`,
    /// `Definition: lightyear = …` — which makes a poor title and a hopeless thing
    /// to copy. Each part is instead the smallest canonical form of one fact.
    ///
    /// Variants name *layouts*, not result kinds, so two kinds that render the same
    /// way share one: a conversion is rink's bare value with no distinguishing
    /// structure, so it is a `number` row. Only three layouts exist because only
    /// three arrangements of these parts do. Named explicitly rather than derived
    /// from the enum variant, so rink's own naming can't leak into theme paths.
    fn present(&self, reply: &QueryReply) -> Entry {
        let row = match reply {
            // `number_value` rather than rink's `n u w`: the trailing `w` is
            // the parenthesised quantity, which travels as the `quantity`
            // detail instead, and `n` would prefix a bare approximation with
            // `approx.`. A title reading `approx. 1.609 km (length)` is doing
            // three jobs.
            QueryReply::Number(parts) => self
                .row(&number_value(parts).unwrap_or_else(|| reply.to_string()))
                .variant("number")
                .details(number_details(parts)),
            QueryReply::Conversion(c) => self
                .row(&number_value(&c.value).unwrap_or_else(|| reply.to_string()))
                .variant("number")
                .details(number_details(&c.value)),
            // `string` rather than `rfc3339`: it is what a person reads, and it
            // is the form that parses back when the row is tab-selected. The
            // relative form belongs beside it, not in place of it.
            QueryReply::Date(d) => {
                let row = self.row(&d.string);
                match &d.human {
                    Some(human) => row.subtitle(human),
                    None => row,
                }
            }
            QueryReply::Substance(s) => {
                let mut row = self.row(&s.name).variant("info");
                // `amount` is the quantity of the substance, which for a bare
                // `=water` is a dimensionless 1 and says nothing. Only worth a
                // line when it carries a unit, as in `=2 kg water`.
                if s.amount.unit.is_some()
                    && let Some(amount) = number_value(&s.amount)
                {
                    row = row.subtitle(amount);
                }
                if let Some(doc) = &s.doc {
                    row = row.comment(doc.text.clone());
                }
                let properties: Vec<String> = s
                    .properties
                    .iter()
                    .filter_map(|p| number_value(&p.value).map(|v| format!("{}: {}", p.name, v)))
                    .collect();
                if !properties.is_empty() {
                    row = row.detail("properties", properties.join(", "));
                }
                row
            }
            QueryReply::Def(d) => {
                let mut row = self.row(&d.canon_name).variant("info");
                if let Some(value) = d.value.as_ref().and_then(number_value) {
                    row = row.subtitle(value);
                }
                if let Some(doc) = &d.doc {
                    row = row.comment(doc.text.clone());
                }
                if let Some(def) = presentable(d.def.as_deref()) {
                    row = row.detail("def", def);
                }
                row
            }
            // These read as a single line of already-formatted text, so there is
            // nothing to take apart and the stock title-only row fits.
            QueryReply::Duration(_)
            | QueryReply::Factorize(_)
            | QueryReply::UnitsFor(_)
            | QueryReply::UnitList(_)
            | QueryReply::Search(_) => self.row(&reply.to_string()).variant("list"),
        };
        // A result is the answer to what was typed, so it always ranks above
        // the providers' own suggestions, however well those match the query.
        row.score(1.0)
    }

    /// Start a row from its headline reading.
    ///
    /// The title is also the clipboard value and the query that re-evaluates it,
    /// because all three are the same canonical form of one fact. Every result
    /// needs all three, so they are set once here rather than in each arm.
    ///
    /// The re-evaluating query keeps the prefix the user typed instead of
    /// carrying its own, so a `prefixes` override in the config file is
    /// followed rather than stranded on a `=` that no longer triggers anything.
    fn row(&self, title: &str) -> crate::engine::provider::EntryBuilder {
        self.base(title)
            .clipboard(title)
            .set_query_keeping_prefix(title)
    }
}

/// The fields a numeric result contributes: its physical quantity, and its
/// dimensionality when that says something the unit doesn't.
fn number_details(parts: &NumberParts) -> BTreeMap<String, String> {
    let mut details = BTreeMap::new();
    put(&mut details, "quantity", parts.quantity.as_deref());
    if parts.dimensions.as_deref() != parts.unit.as_deref() {
        put(&mut details, "dimensions", parts.dimensions.as_deref());
    }
    details
}

/// A detail's value, with blank treated as absent: rink's formatter compacts
/// whitespace, so a part the pattern didn't match comes back as an empty string
/// rather than `None`, and an empty detail would render a blank label.
fn presentable(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Record a detail under `key`, skipping anything unset or blank. rink's
/// formatter compacts whitespace, so a part the pattern didn't match comes
/// back as an empty string rather than `None`.
fn put(details: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = presentable(value) {
        details.insert(key.to_owned(), value.to_owned());
    }
}

/// Format a number with rink's token DSL (`e` exact, `a` approximate, `u` unit,
/// `q` quantity), or `None` when the pattern matched nothing. Absent parts
/// leave their separator behind, hence the trim.
fn number_text(parts: &NumberParts, pattern: &str) -> Option<String> {
    let rendered = parts.format(pattern);
    let trimmed = rendered.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Format a self-contained number for a detail value: the unit after the exact
/// value when rink has one, otherwise after the approximation. Deliberately
/// avoids rink's combined `n` token, which prefixes a bare approximation with
/// `approx.` — noise in a short detail line.
///
/// The spaces in the pattern are load-bearing: rink passes unrecognized
/// characters straight through, which is how its own `Display` gets a space
/// between a number and its unit.
fn number_value(parts: &NumberParts) -> Option<String> {
    let pattern = if parts.exact_value.is_some() {
        "e u"
    } else {
        "a u"
    };
    number_text(parts, pattern)
}

#[cfg(test)]
mod tests {
    use rink_core::output::{
        ConversionReply, DateReply, DefReply, DocString, Factorization, PropertyReply, SearchReply,
        SubstanceReply, UnitListReply, UnitsForReply,
    };

    use super::*;

    /// One result's entry fields, so assertions read `shown.title` rather than
    /// `shown.entry.title`.
    fn shown(reply: &QueryReply) -> crate::engine::provider::EntryMeta {
        CalculatorProvider::new().present(reply).entry
    }

    fn detail<'a>(details: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
        details.get(key).map(String::as_str)
    }

    fn substance(name: &str) -> SubstanceReply {
        SubstanceReply {
            name: name.into(),
            doc: Some(DocString::new("a substance")),
            amount: NumberParts {
                approx_value: Some("1".into()),
                ..Default::default()
            },
            properties: vec![PropertyReply {
                name: "molar mass".into(),
                value: NumberParts {
                    approx_value: Some("18.015".into()),
                    unit: Some("g/mol".into()),
                    ..Default::default()
                },
                doc: None,
            }],
        }
    }

    fn date(human: Option<&str>) -> DateReply {
        DateReply {
            year: 2026,
            month: 10,
            day: 3,
            hour: 0,
            minute: 0,
            second: 0,
            nanosecond: 0,
            human: human.map(str::to_owned),
            string: "2026-10-03 00:00:00 [Europe/Berlin]".into(),
            rfc3339: "2026-10-03T00:00:00+00:00".into(),
        }
    }

    #[test]
    fn number_title_leaves_the_quantity_to_its_own_detail() {
        let shown = shown(&QueryReply::Number(NumberParts {
            approx_value: Some("1.609".into()),
            unit: Some("km".into()),
            quantity: Some("length".into()),
            ..Default::default()
        }));
        // Not rink's `n u w`, which would read `1.609 km (length)`.
        assert_eq!(shown.title, "1.609 km");
        assert_eq!(detail(&shown.details, "quantity"), Some("length"));
        // `dimensions` equals `unit` here, so it isn't worth repeating.
        assert_eq!(detail(&shown.details, "dimensions"), None);
        assert_eq!(shown.variant.as_deref(), Some("number"));
    }

    /// The title is the exact reading when rink has one and the approximation
    /// when it doesn't, never both — so a detail repeating either could only
    /// ever duplicate it.
    #[test]
    fn number_details_never_repeat_the_title() {
        let cases = [
            NumberParts {
                exact_value: Some("1/3".into()),
                unit: Some("m/s".into()),
                ..Default::default()
            },
            NumberParts {
                approx_value: Some("1.414213".into()),
                unit: Some("m".into()),
                ..Default::default()
            },
            NumberParts {
                exact_value: Some("42".into()),
                ..Default::default()
            },
        ];
        for parts in cases {
            let shown = shown(&QueryReply::Number(parts.clone()));
            assert!(
                !shown.details.contains_key("exact") && !shown.details.contains_key("approx"),
                "{shown:?}"
            );
        }
    }

    #[test]
    fn number_exposes_dimensionality_when_it_differs_from_the_unit() {
        let shown = shown(&QueryReply::Number(NumberParts {
            exact_value: Some("1/3".into()),
            unit: Some("m/s".into()),
            dimensions: Some("L T^-1".into()),
            ..Default::default()
        }));
        assert_eq!(shown.title, "1/3 m/s");
        assert_eq!(detail(&shown.details, "dimensions"), Some("L T^-1"));
    }

    #[test]
    fn dimensionless_number_omits_quantity_and_dimensions() {
        let shown = shown(&QueryReply::Number(NumberParts {
            exact_value: Some("42".into()),
            ..Default::default()
        }));
        assert_eq!(shown.title, "42");
        assert_eq!(detail(&shown.details, "quantity"), None);
        assert_eq!(detail(&shown.details, "dimensions"), None);
    }

    /// A conversion is rink's bare value with nothing distinguishing it, so it
    /// is the same layout and must not claim a variant of its own.
    #[test]
    fn conversion_shares_the_number_layout() {
        let shown = shown(&QueryReply::Conversion(Box::new(ConversionReply {
            value: NumberParts {
                exact_value: Some("1/3".into()),
                unit: Some("m/s".into()),
                quantity: Some("speed".into()),
                ..Default::default()
            },
        })));
        assert_eq!(shown.title, "1/3 m/s");
        assert_eq!(detail(&shown.details, "quantity"), Some("speed"));
        assert_eq!(shown.variant.as_deref(), Some("number"));
    }

    #[test]
    fn date_splits_readable_time_from_relative_time() {
        let shown = shown(&QueryReply::Date(date(Some("in 3 days"))));
        assert_eq!(shown.title, "2026-10-03 00:00:00 [Europe/Berlin]");
        assert_eq!(shown.subtitle.as_deref(), Some("in 3 days"));
        assert!(shown.comment.is_none());
        // Both forms are spoken for, so there is nothing left for details.
        assert!(shown.details.is_empty(), "{:?}", shown.details);
        // Title plus subtitle is the stock row layout.
        assert_eq!(shown.variant, None);
    }

    #[test]
    fn date_without_humanization_leaves_the_subtitle_unset() {
        let shown = shown(&QueryReply::Date(date(None)));
        assert_eq!(shown.title, "2026-10-03 00:00:00 [Europe/Berlin]");
        assert_eq!(shown.subtitle, None);
        assert!(shown.details.is_empty());
    }

    #[test]
    fn substance_uses_name_amount_and_prose() {
        let shown = shown(&QueryReply::Substance(substance("water")));
        assert_eq!(shown.title, "water");
        // The fixture's amount is a dimensionless 1, which says nothing.
        assert_eq!(shown.subtitle, None);
        assert_eq!(shown.comment.as_deref(), Some("a substance"));
        assert_eq!(
            detail(&shown.details, "properties"),
            Some("molar mass: 18.015 g/mol")
        );
        assert_eq!(shown.variant.as_deref(), Some("info"));
    }

    /// `=2 kg water` has an amount worth a line of its own, unlike `=water`.
    #[test]
    fn substance_with_a_unit_amount_gets_a_subtitle() {
        let mut reply = substance("water");
        reply.amount = NumberParts {
            exact_value: Some("2".into()),
            unit: Some("kilogram".into()),
            ..Default::default()
        };
        let shown = shown(&QueryReply::Substance(reply));
        assert_eq!(shown.title, "water");
        assert_eq!(shown.subtitle.as_deref(), Some("2 kilogram"));
    }

    #[test]
    fn def_uses_name_value_and_prose() {
        let shown = shown(&QueryReply::Def(Box::new(DefReply {
            canon_name: "lightyear".into(),
            def: Some("9460730472580800 m".into()),
            def_expr: None,
            value: Some(NumberParts {
                approx_value: Some("9.461".into()),
                ..Default::default()
            }),
            doc: Some(DocString::new("a distance")),
        })));
        assert_eq!(shown.title, "lightyear");
        assert_eq!(shown.subtitle.as_deref(), Some("9.461"));
        assert_eq!(shown.comment.as_deref(), Some("a distance"));
        assert_eq!(detail(&shown.details, "def"), Some("9460730472580800 m"));
        assert_eq!(shown.variant.as_deref(), Some("info"));
    }

    #[test]
    fn def_without_value_or_doc_omits_them() {
        let shown = shown(&QueryReply::Def(Box::new(DefReply {
            canon_name: "foo".into(),
            def: Some("1 m".into()),
            def_expr: None,
            value: None,
            doc: None,
        })));
        assert_eq!(shown.title, "foo");
        assert_eq!(shown.subtitle, None);
        assert_eq!(shown.comment, None);
        assert_eq!(detail(&shown.details, "def"), Some("1 m"));
    }

    #[test]
    fn substance_without_doc_or_properties_omits_them() {
        let bare = SubstanceReply {
            name: "unobtainium".into(),
            doc: None,
            amount: NumberParts::default(),
            properties: vec![],
        };
        let shown = shown(&QueryReply::Substance(bare));
        assert_eq!(shown.title, "unobtainium");
        assert_eq!(shown.subtitle, None);
        assert_eq!(shown.comment, None);
        assert!(shown.details.is_empty(), "{:?}", shown.details);
    }

    /// These kinds are already a single line of text, so they keep rink's
    /// rendering verbatim and take nothing else.
    #[test]
    fn single_line_kinds_keep_rinks_rendering() {
        let cases = [
            QueryReply::Duration(Box::new(rink_core::output::DurationReply {
                raw: NumberParts::default(),
                years: NumberParts::default(),
                months: NumberParts::default(),
                weeks: NumberParts::default(),
                days: NumberParts::default(),
                hours: NumberParts::default(),
                minutes: NumberParts::default(),
                seconds: NumberParts::default(),
            })),
            QueryReply::Factorize(rink_core::output::FactorizeReply {
                factorizations: vec![Factorization {
                    units: BTreeMap::new(),
                }],
            }),
            QueryReply::UnitsFor(UnitsForReply {
                units: vec![],
                of: NumberParts::default(),
            }),
            QueryReply::UnitList(UnitListReply {
                rest: NumberParts::default(),
                list: vec![],
            }),
            QueryReply::Search(SearchReply { results: vec![] }),
        ];
        for reply in cases {
            let shown = shown(&reply);
            assert_eq!(shown.title, reply.to_string());
            assert_eq!(shown.subtitle, None);
            assert_eq!(shown.comment, None);
            assert!(shown.details.is_empty());
            assert_eq!(shown.variant.as_deref(), Some("list"));
        }
    }

    /// Every kind must produce something to show and something to copy, and the
    /// title is both, so an empty one would be an invisible row.
    #[test]
    fn every_kind_produces_a_non_empty_title() {
        let cases = [
            QueryReply::Number(NumberParts {
                exact_value: Some("42".into()),
                ..Default::default()
            }),
            QueryReply::Conversion(Box::new(ConversionReply {
                value: NumberParts {
                    exact_value: Some("42".into()),
                    ..Default::default()
                },
            })),
            QueryReply::Date(date(Some("in 3 days"))),
            QueryReply::Substance(substance("water")),
            QueryReply::Def(Box::new(DefReply {
                canon_name: "foo".into(),
                def: None,
                def_expr: None,
                value: None,
                doc: None,
            })),
            QueryReply::UnitsFor(UnitsForReply {
                units: vec![],
                of: NumberParts::default(),
            }),
            QueryReply::Search(SearchReply { results: vec![] }),
        ];
        for reply in cases {
            let shown = shown(&reply);
            assert!(!shown.title.trim().is_empty(), "{reply:?}");
        }
    }

    /// Variant names become directory names in a theme, so they must stay
    /// path-safe — and there must be no more of them than there are layouts.
    /// A conversion deliberately shares `number` rather than claiming one.
    #[test]
    fn variant_names_are_path_safe_and_collapse_to_three_layouts() {
        let names = [
            shown(&QueryReply::Number(NumberParts::default())).variant,
            shown(&QueryReply::Conversion(Box::new(ConversionReply {
                value: NumberParts::default(),
            })))
            .variant,
            shown(&QueryReply::Substance(substance("water"))).variant,
            shown(&QueryReply::Def(Box::new(DefReply {
                canon_name: "foo".into(),
                def: None,
                def_expr: None,
                value: None,
                doc: None,
            })))
            .variant,
            shown(&QueryReply::Search(SearchReply { results: vec![] })).variant,
        ];
        let mut unique: Vec<String> = Vec::new();
        for name in names {
            let name = name.as_deref().expect("every listed kind names a layout");
            assert!(
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "variant name {name:?} must be usable as a directory name"
            );
            if !unique.iter().any(|u| u.as_str() == name) {
                unique.push(name.to_owned());
            }
        }
        unique.sort_unstable();
        assert_eq!(unique, ["info", "list", "number"].map(String::from));
        // A date is deliberately absent: title plus subtitle needs no variant.
        assert_eq!(shown(&QueryReply::Date(date(None))).variant, None);
    }

    /// Checks which parts real replies populate, as opposed to which the
    /// minimal fixtures above hand-build. Real `NumberParts` carry a `raw_unit`
    /// and real dates carry a time zone, so the fixtures are easy to get subtly
    /// wrong — which is how `rfc3339` (a local time with no offset, despite the
    /// name) got mistaken for the better `string` field.
    ///
    /// Asserts shape, not values, so rink's data can change freely.
    #[test]
    fn real_replies_fill_the_expected_parts() {
        let mut ctx = simple_context().unwrap();
        let mut show = |q: &str| shown(&eval(&mut ctx, q).unwrap());

        let number = show("18.015 g/mol");
        assert_eq!(number.variant.as_deref(), Some("number"));
        // Note rink normalises to base units, so the title is `0.018015
        // kilogram / mole`, not what was typed. That is the canonical reading
        // and what the clipboard gets; the unit that was typed is not
        // recoverable from the reply.
        assert!(!number.title.is_empty());
        assert_eq!(
            number.details.get("quantity").map(String::as_str),
            Some("molar_mass")
        );
        // No reply may emit a key a theme has no widget id for.
        for key in number.details.keys() {
            assert!(DETAIL_KEYS.contains(&key.as_str()), "{key:?}");
        }

        // `now` is the one kind with no variant, and it must say so.
        let date = show("now");
        assert_eq!(date.variant, None);
        assert!(!date.title.is_empty());
        assert!(date.subtitle.is_some(), "{date:?}");

        let substance = show("water");
        assert_eq!(substance.variant.as_deref(), Some("info"));
        assert_eq!(substance.title, "water");
        assert!(substance.subtitle.is_none(), "{substance:?}");
        assert!(
            substance
                .details
                .get("properties")
                .is_some_and(|p| p.contains("density:")),
            "{:?}",
            substance.details
        );

        // A documented unit is where the comment earns its place: rink's docs
        // are sparse for substances but present for many units.
        let unit = show("pascal");
        assert_eq!(unit.variant.as_deref(), Some("info"));
        assert_eq!(unit.title, "pascal");
        assert!(
            unit.comment
                .as_deref()
                .is_some_and(|c| !c.trim().is_empty()),
            "{unit:?}"
        );
        assert!(unit.subtitle.is_some(), "{unit:?}");

        // `1/3 m/s` normalizes to base units, so the unit is a compound
        // expression rather than the one that was typed.
        let def = show("lightyear");
        assert_eq!(def.variant.as_deref(), Some("info"));
        assert_eq!(def.title, "lightyear");
        assert!(def.subtitle.is_some(), "{def:?}");
        assert!(def.details.contains_key("def"), "{:?}", def.details);

        for q in [
            "18.015 g/mol",
            "1/3 m/s",
            "42",
            "now",
            "water",
            "lightyear",
            "3 kWh",
        ] {
            let shown = show(q);
            let unexpected: Vec<&str> = shown
                .details
                .keys()
                .map(String::as_str)
                .filter(|key| !DETAIL_KEYS.contains(key))
                .collect();
            assert!(unexpected.is_empty(), "{q:?} produced {unexpected:?}");
        }
    }

    /// The title doubles as the query re-evaluated on tab, so a title built from
    /// prose must not be pasted back in as nonsense. Re-querying it has to
    /// reproduce the same title.
    #[test]
    fn titles_round_trip_as_queries() {
        let mut ctx = simple_context().unwrap();
        for q in ["water", "lightyear", "18.015 g/mol"] {
            let title = shown(&eval(&mut ctx, q).unwrap()).title;
            let again = eval(&mut ctx, &title)
                .map(|reply| shown(&reply).title)
                .unwrap_or_else(|err| err.to_string());
            assert_eq!(again, title, "re-querying the title of {q:?} moved it");
        }
    }
}
