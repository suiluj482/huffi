use std::collections::BTreeMap;

use rink_core::output::{NumberParts, QueryReply};
use rink_core::{eval, simple_context};

use crate::engine::provider::{
    Entry, Icon, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
};

/// Id shared by every entry this provider returns, and the key its single
/// result is tracked under in history.
const ENTRY_ID: &str = "huffi-calculator";

/// Shown when the user has typed the `=` prefix but nothing after it.
const PLACEHOLDER: &str = "type to calculate";

/// Every detail key this provider can produce, across all result kinds.
///
/// A theme declares the subset it wants rendered by adding a `detail-<key>`
/// widget to its calculator template; anything not declared is simply not
/// shown. Exposed so a template can be checked against the keys that actually
/// exist rather than against a copy of them.
pub const DETAIL_KEYS: &[&str] = &[
    "quantity",
    "dimensions",
    "exact",
    "approx",
    "human",
    "absolute",
    "doc",
    "properties",
    "def",
    "value",
];

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
            return vec![
                self.base(PLACEHOLDER)
                    .clipboard(PLACEHOLDER)
                    .set_query(format!("={PLACEHOLDER}"))
                    .score(1.0),
            ];
        }

        match eval(rink, ctx.query) {
            // `Display` for a reply is rink's own one-line rendering, used
            // here for both the row title and the clipboard copy.
            Ok(reply) => {
                let title = reply.to_string();
                vec![
                    self.base(&title)
                        .variant(variant_name(&reply))
                        .details(details_from_reply(&reply))
                        .clipboard(title.clone())
                        .set_query(format!("={title}"))
                        .score(1.0),
                ]
            }
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
}

/// Stable, kebab-case name for a kind of rink result, used as the layout-variant
/// directory in a theme (`providers/calculator/<variant>/entry.ui`). Mapped
/// explicitly rather than derived from the enum variant so that rink's own
/// naming can't leak into theme paths.
fn variant_name(reply: &QueryReply) -> &'static str {
    match reply {
        QueryReply::Number(_) => "number",
        QueryReply::Date(_) => "date",
        QueryReply::Substance(_) => "substance",
        QueryReply::Duration(_) => "duration",
        QueryReply::Def(_) => "def",
        QueryReply::Conversion(_) => "conversion",
        QueryReply::Factorize(_) => "factorize",
        QueryReply::UnitsFor(_) => "units-for",
        QueryReply::UnitList(_) => "unit-list",
        QueryReply::Search(_) => "search",
    }
}

/// Pull structured metadata out of a rink result into named display fields.
///
/// A theme decides which of these to render by declaring matching
/// `detail-<key>` widgets; see [`crate::engine::provider::EntryMeta::details`].
/// Only the kinds with something worth separating from the title are handled —
/// the rest render bare. Every kind still gets a
/// [`variant_name`](variant_name), so adding details for one later is a
/// template-only change.
///
/// Keys emitted here:
/// - numbers: `quantity`, `dimensions` (only when it isn't already the unit),
///   `exact` and `approx` (whichever rink can represent)
/// - dates: `human` (chrono-humanized, e.g. `in 3 days`), `absolute` (rink's
///   own timestamp string, which carries the time zone)
/// - substances: `doc`, `properties` (a joined `name: value` list)
/// - unit definitions: `def`, `value`, `doc`
fn details_from_reply(reply: &QueryReply) -> BTreeMap<String, String> {
    let mut details = BTreeMap::new();
    match reply {
        QueryReply::Number(n) => {
            put(&mut details, "quantity", n.quantity.as_deref());
            if n.dimensions.as_deref() != n.unit.as_deref() {
                put(&mut details, "dimensions", n.dimensions.as_deref());
            }
            put(&mut details, "exact", number_part(n, "e").as_deref());
            put(&mut details, "approx", number_part(n, "a").as_deref());
        }
        QueryReply::Date(d) => {
            put(&mut details, "human", d.human.as_deref());
            put(&mut details, "absolute", Some(&d.string));
        }
        QueryReply::Substance(s) => {
            put(&mut details, "doc", s.doc.as_ref().map(|d| d.text.as_str()));
            let properties: Vec<String> = s
                .properties
                .iter()
                .filter_map(|p| number_value(&p.value).map(|v| format!("{}: {}", p.name, v)))
                .collect();
            if !properties.is_empty() {
                details.insert("properties".into(), properties.join(", "));
            }
        }
        QueryReply::Def(d) => {
            put(&mut details, "def", d.def.as_deref());
            put(
                &mut details,
                "value",
                d.value.as_ref().and_then(number_value).as_deref(),
            );
            put(
                &mut details,
                "doc",
                d.doc.as_ref().map(|doc| doc.text.as_str()),
            );
        }
        _ => {}
    }
    details
}

/// Record a detail under `key`, skipping anything unset or blank. rink's
/// formatter compacts whitespace, so a part the pattern didn't match comes
/// back as an empty string rather than `None`.
fn put(details: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
        details.insert(key.to_owned(), value.to_owned());
    }
}

/// Format a number with rink's token DSL (`e` exact, `a` approximate, `u` unit,
/// `q` quantity), or `None` when the pattern matched nothing.
fn number_part(parts: &NumberParts, pattern: &str) -> Option<String> {
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
    number_part(parts, pattern)
}

#[cfg(test)]
mod tests {
    use rink_core::output::{DateReply, DefReply, DocString, PropertyReply, SubstanceReply};

    use super::*;

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

    #[test]
    fn number_exposes_quantity_and_approximation() {
        let reply = QueryReply::Number(NumberParts {
            approx_value: Some("1.609".into()),
            unit: Some("km".into()),
            quantity: Some("length".into()),
            ..Default::default()
        });
        let details = details_from_reply(&reply);
        assert_eq!(detail(&details, "quantity"), Some("length"));
        assert_eq!(detail(&details, "approx"), Some("1.609"));
        // Nothing to show for the parts this reply has no value for.
        assert_eq!(detail(&details, "exact"), None);
        // `dimensions` equals `unit` here, so it isn't worth repeating.
        assert_eq!(detail(&details, "dimensions"), None);
    }

    #[test]
    fn number_exposes_dimensionality_when_it_differs_from_the_unit() {
        let reply = QueryReply::Number(NumberParts {
            exact_value: Some("1/3".into()),
            approx_value: Some("0.333".into()),
            unit: Some("m/s".into()),
            dimensions: Some("L T^-1".into()),
            ..Default::default()
        });
        let details = details_from_reply(&reply);
        assert_eq!(detail(&details, "dimensions"), Some("L T^-1"));
        assert_eq!(detail(&details, "exact"), Some("1/3"));
        assert_eq!(detail(&details, "approx"), Some("0.333"));
    }

    #[test]
    fn dimensionless_number_omits_quantity_and_dimensions() {
        let details = details_from_reply(&QueryReply::Number(NumberParts {
            exact_value: Some("42".into()),
            ..Default::default()
        }));
        assert_eq!(detail(&details, "exact"), Some("42"));
        assert_eq!(detail(&details, "quantity"), None);
        assert_eq!(detail(&details, "dimensions"), None);
    }

    #[test]
    fn date_exposes_humanized_and_absolute_time() {
        let reply = QueryReply::Date(DateReply {
            year: 2026,
            month: 10,
            day: 3,
            hour: 0,
            minute: 0,
            second: 0,
            nanosecond: 0,
            human: Some("in 3 days".into()),
            string: "2026-10-03 00:00:00 [Europe/Berlin]".into(),
            rfc3339: "2026-10-03T00:00:00+00:00".into(),
        });
        let details = details_from_reply(&reply);
        assert_eq!(detail(&details, "human"), Some("in 3 days"));
        assert_eq!(
            detail(&details, "absolute"),
            Some("2026-10-03 00:00:00 [Europe/Berlin]")
        );
    }

    #[test]
    fn date_without_humanization_still_exposes_absolute_time() {
        let reply = QueryReply::Date(DateReply {
            year: 2026,
            month: 10,
            day: 3,
            hour: 0,
            minute: 0,
            second: 0,
            nanosecond: 0,
            human: None,
            string: "2026-10-03 00:00:00 [Europe/Berlin]".into(),
            rfc3339: "2026-10-03T00:00:00+00:00".into(),
        });
        let details = details_from_reply(&reply);
        assert_eq!(detail(&details, "human"), None);
        assert_eq!(
            detail(&details, "absolute"),
            Some("2026-10-03 00:00:00 [Europe/Berlin]")
        );
    }

    #[test]
    fn substance_exposes_doc_and_properties() {
        let details = details_from_reply(&QueryReply::Substance(substance("water")));
        assert_eq!(detail(&details, "doc"), Some("a substance"));
        assert_eq!(
            detail(&details, "properties"),
            Some("molar mass: 18.015 g/mol")
        );
    }

    #[test]
    fn def_exposes_definition_value_and_doc() {
        let reply = QueryReply::Def(Box::new(DefReply {
            canon_name: "lightyear".into(),
            def: Some("9460730472580800 m".into()),
            def_expr: None,
            value: Some(NumberParts {
                approx_value: Some("9.461".into()),
                ..Default::default()
            }),
            doc: Some(DocString::new("a distance")),
        }));
        let details = details_from_reply(&reply);
        assert_eq!(detail(&details, "def"), Some("9460730472580800 m"));
        assert_eq!(detail(&details, "value"), Some("9.461"));
        assert_eq!(detail(&details, "doc"), Some("a distance"));
    }

    #[test]
    fn def_without_value_or_doc_omits_them() {
        let reply = QueryReply::Def(Box::new(DefReply {
            canon_name: "foo".into(),
            def: Some("1 m".into()),
            def_expr: None,
            value: None,
            doc: None,
        }));
        let details = details_from_reply(&reply);
        assert_eq!(detail(&details, "def"), Some("1 m"));
        assert_eq!(detail(&details, "value"), None);
        assert_eq!(detail(&details, "doc"), None);
    }

    #[test]
    fn kinds_without_details_yield_nothing() {
        let reply = QueryReply::Search(rink_core::output::SearchReply { results: vec![] });
        assert!(details_from_reply(&reply).is_empty());
    }

    /// Checks which keys real replies yield, as opposed to which the minimal
    /// fixtures above hand-build. Real `NumberParts` carry a `raw_unit` and real
    /// dates carry a time zone, so the fixtures are easy to get subtly wrong —
    /// which is exactly how `rfc3339` (a local time with no offset, despite the
    /// name) got mistaken for the better `string` field.
    ///
    /// Asserts only key presence, not values, so rink's data can change without
    /// breaking this.
    #[test]
    fn real_replies_expose_the_documented_keys() {
        let mut ctx = simple_context().unwrap();
        let mut reply = |q: &str| eval(&mut ctx, q).unwrap();

        let number = details_from_reply(&reply("18.015 g/mol"));
        assert!(number.contains_key("exact") || number.contains_key("approx"));
        assert_eq!(
            number.get("quantity").map(String::as_str),
            Some("molar_mass")
        );

        let date = details_from_reply(&reply("now"));
        assert!(date.contains_key("human"), "{date:?}");
        assert!(date.contains_key("absolute"), "{date:?}");

        let substance = details_from_reply(&reply("water"));
        assert!(
            substance
                .get("properties")
                .is_some_and(|p| p.contains("density:")),
            "{substance:?}"
        );

        // `1/3 m/s` normalizes to base units, so the unit is a compound
        // expression rather than the one that was typed.
        let def = details_from_reply(&reply("lightyear"));
        assert!(def.contains_key("def"), "{def:?}");
        assert!(def.contains_key("value"), "{def:?}");

        // No reply may emit a key a theme has no widget id for.
        for q in [
            "18.015 g/mol",
            "1/3 m/s",
            "42",
            "now",
            "water",
            "lightyear",
            "3 kWh",
        ] {
            let reply = reply(q);
            let details = details_from_reply(&reply);
            let unexpected: Vec<&str> = details
                .keys()
                .map(String::as_str)
                .filter(|key| !DETAIL_KEYS.contains(key))
                .collect();
            assert!(unexpected.is_empty(), "{q:?} produced {unexpected:?}");
        }
    }

    #[test]
    fn variant_names_are_distinct_and_path_safe() {
        let names = [
            variant_name(&QueryReply::Number(NumberParts::default())),
            variant_name(&QueryReply::Date(DateReply {
                year: 2026,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
                nanosecond: 0,
                human: None,
                string: String::new(),
                rfc3339: String::new(),
            })),
            variant_name(&QueryReply::Substance(substance("water"))),
            variant_name(&QueryReply::UnitsFor(rink_core::output::UnitsForReply {
                units: vec![],
                of: NumberParts::default(),
            })),
            variant_name(&QueryReply::UnitList(rink_core::output::UnitListReply {
                rest: NumberParts::default(),
                list: vec![],
            })),
        ];
        for name in names {
            assert!(
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "variant name {name:?} must be usable as a directory name"
            );
        }
        let mut unique = names.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "variant names must be distinct");
    }
}
