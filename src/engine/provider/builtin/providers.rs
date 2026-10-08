use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
};
use crate::engine::scoring::MatchField;

/// Lists the registered providers and scopes the next query to one of them.
///
/// Triggered by the target prefix (default `\`). A bare prefix (or a partial
/// `\desk`) lists providers, filtered by name and id. Selecting an entry
/// replaces the query with `\<id> ` — the recognized form for targeting that
/// provider exclusively — leaving the user to type what they want to search
/// for.
///
/// The provider does not hold a live view of the collection: it is built with
/// the list the engine reports at registration time, which is where providers
/// are fixed. Disabled providers are omitted, since targeting one would do
/// nothing.
pub struct ProvidersProvider {
    providers: Vec<ProviderMeta>,
    target_prefix: String,
}

impl ProvidersProvider {
    /// Build the provider from the engine's registered providers and the
    /// configured target prefix, e.g.
    /// `ProvidersProvider::new(engine.providers(), engine.target_prefix())`.
    /// Call it after every other provider has been registered; the provider
    /// itself need not be (and is not expected to be) in the list.
    pub fn new(providers: Vec<ProviderMeta>, target_prefix: impl Into<String>) -> Self {
        Self {
            providers,
            target_prefix: target_prefix.into(),
        }
    }

    /// A human-readable description of how the provider is triggered.
    fn trigger(meta: &ProviderMeta) -> String {
        if meta.prefixes.is_empty() {
            "always active".to_string()
        } else {
            meta.prefixes.join(", ")
        }
    }
}

impl Provider for ProvidersProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta::builder("providers")
            .prefix(self.target_prefix.clone())
            .prefix_only(true)
            .build()
    }

    fn init(&mut self, _ctx: InitContext) -> ProviderResult {
        ProviderResult::Ok
    }

    fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
        self.providers
            .iter()
            .filter(|meta| meta.enabled)
            .map(|meta| {
                let target = format!("{}{} ", self.target_prefix, meta.id);
                let mut fields = vec![MatchField {
                    text: meta.name.clone(),
                    weight: 1.0,
                }];
                if meta.name != meta.id {
                    fields.push(MatchField {
                        text: meta.id.clone(),
                        weight: 1.0,
                    });
                }
                for prefix in &meta.prefixes {
                    fields.push(MatchField {
                        text: prefix.clone(),
                        weight: 1.0,
                    });
                }
                entry(format!("provider-{}", meta.id), meta.name.clone())
                    .subtitle(Self::trigger(meta))
                    .history_key(format!("provider-{}", meta.id))
                    .set_query(target.clone())
                    .action_set_query(target)
                    .match_fields(fields)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::Action;

    fn meta(id: &str, name: &str, prefixes: &[&str], enabled: bool) -> ProviderMeta {
        ProviderMeta {
            id: id.to_string(),
            name: name.to_string(),
            prefixes: prefixes.iter().map(|p| (*p).to_string()).collect(),
            enabled,
            prefix_only: !prefixes.is_empty(),
        }
    }

    fn provider() -> ProvidersProvider {
        let mut p = ProvidersProvider::new(
            vec![
                meta("desktop", "desktop", &[], true),
                meta("calculator", "Calc", &["="], true),
                meta("gone", "Gone", &["!"], false),
            ],
            "\\",
        );
        assert!(matches!(
            p.init(InitContext {
                data_dir: std::path::Path::new("/tmp/providers"),
                extra: None,
            }),
            ProviderResult::Ok
        ));
        p
    }

    fn query(p: &mut ProvidersProvider) -> Vec<Entry> {
        p.query(QueryContext {
            prefix: Some("\\"),
            query: "",
            original: "\\",
        })
    }

    #[test]
    fn lists_only_enabled_providers() {
        let entries = query(&mut provider());
        let ids: Vec<_> = entries.iter().map(|e| e.entry.id.as_str()).collect();
        assert_eq!(ids, vec!["provider-desktop", "provider-calculator"]);
    }

    #[test]
    fn entry_targets_its_provider_on_both_tab_and_enter() {
        let mut p = provider();
        let entries = query(&mut p);
        let calc = entries
            .iter()
            .find(|e| e.entry.id == "provider-calculator")
            .expect("calculator entry");
        assert_eq!(
            calc.entry.set_query.as_ref().map(|s| s.query.as_str()),
            Some("\\calculator ")
        );
        match &calc.entry.action {
            Action::SetQuery { suggestion } => {
                assert_eq!(suggestion.query, "\\calculator ");
                assert!(!suggestion.keep_prefix);
            }
            other => panic!("expected SetQuery action, got {other:?}"),
        }
    }

    #[test]
    fn subtitle_reports_triggers_or_always_active() {
        let mut p = provider();
        let entries = query(&mut p);
        let calc = entries
            .iter()
            .find(|e| e.entry.id == "provider-calculator")
            .unwrap();
        assert_eq!(calc.entry.subtitle.as_deref(), Some("="));
        let desktop = entries
            .iter()
            .find(|e| e.entry.id == "provider-desktop")
            .unwrap();
        assert_eq!(desktop.entry.subtitle.as_deref(), Some("always active"));
    }
}
