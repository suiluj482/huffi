use std::path::Path;

use anyhow::Context;

use crate::engine::scoring::QueryGroup;

use super::config::ProviderConfig;
use super::{CalculatorProvider, DesktopEntryProvider, Entry, EntryMeta, Provider};

pub struct ProviderCollection {
    providers: Vec<Box<dyn Provider>>,
    /// Huffi's data folder; each provider gets `data_dir/providers/<id>/`.
    data_dir: std::path::PathBuf,
    /// Skip creating on-disk state when true.
    dry_run: bool,
}

/// A registered provider and its trigger prefixes.
pub struct ProviderInfo {
    pub id: String,
    pub prefixes: Vec<String>,
}

/// The result of resolving the global prefix for a query.
///
/// The longest declared provider prefix that the query starts with wins;
/// there is at most one active prefix per query.
pub struct PreprocessedQuery {
    pub original_query: String,
    pub prefix: Option<String>,
    pub query: String,
}

impl ProviderCollection {
    /// Construct the collection with the built-in providers configured from
    /// `config`. Each provider is registered with `add_provider`, which
    /// provisions its `<data_dir>/providers/<provider id>/` folder (when
    /// not in dry-run mode) and calls its [`init`](Provider::init).
    pub fn new_with_config(
        data_dir: impl AsRef<Path>,
        dry_run: bool,
        config: &ProviderConfig,
    ) -> anyhow::Result<Self> {
        let mut collection = Self {
            providers: Vec::new(),
            data_dir: data_dir.as_ref().to_path_buf(),
            dry_run,
        };
        collection.add_provider(Box::new(DesktopEntryProvider::new(
            freedesktop_desktop_entry::default_paths().collect(),
            config.desktop,
        )))?;
        collection.add_provider(Box::new(CalculatorProvider::new()))?;
        Ok(collection)
    }
}

impl ProviderCollection {
    pub fn add_provider(&mut self, mut provider: Box<dyn Provider>) -> anyhow::Result<()> {
        let dir = self.data_dir.join("providers").join(provider.id());
        if !self.dry_run {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create data dir {}", dir.display()))?;
        }
        provider.init(&dir);
        self.providers.push(provider);
        Ok(())
    }

    /// Resolve the global prefix for a query.
    ///
    /// Matches the longest declared provider prefix that the query starts
    /// with. If several prefixes are tied in length, the first declared wins.
    pub fn preprocess_query(&self, query: &str) -> PreprocessedQuery {
        let mut longest: Option<&str> = None;
        for provider in &self.providers {
            for prefix in provider.prefixes() {
                if !prefix.is_empty()
                    && query.starts_with(prefix)
                    && longest.is_none_or(|current| prefix.len() > current.len())
                {
                    longest = Some(prefix);
                }
            }
        }

        match longest {
            Some(prefix) => PreprocessedQuery {
                original_query: query.to_string(),
                prefix: Some(prefix.to_string()),
                query: query[prefix.len()..].to_string(),
            },
            None => PreprocessedQuery {
                original_query: query.to_string(),
                prefix: None,
                query: query.to_string(),
            },
        }
    }

    /// Query providers without scoring (raw entries).
    pub fn entries(&mut self, pre: &PreprocessedQuery) -> Vec<Entry> {
        self.grouped_entries(pre)
            .into_iter()
            .flat_map(|g| g.entries)
            .collect()
    }

    /// Query each provider and group its entries with the query they should
    /// be fuzzy-scored against. Entries are annotated with their provider id.
    ///
    /// Scoring itself is owned by [`crate::engine::Engine`]; export the raw
    /// groups here so the engine can hand them to the
    /// [`Scorer`](crate::engine::scoring::Scorer).
    pub(crate) fn grouped_entries(
        &mut self,
        pre: &PreprocessedQuery,
    ) -> Vec<QueryGroup<EntryMeta>> {
        self.providers
            .iter_mut()
            .map(|p| {
                let matched = pre
                    .prefix
                    .as_deref()
                    .is_some_and(|pfx| p.prefixes().contains(&pfx));
                let (prefix, query) = if matched {
                    (pre.prefix.as_deref(), &pre.query)
                } else {
                    (None, &pre.original_query)
                };
                let mut entries = p.query(prefix, query);
                for e in entries.iter_mut() {
                    e.entry.provider_id = Some(p.id().to_string());
                }
                QueryGroup {
                    query: query.to_string(),
                    entries,
                }
            })
            .collect()
    }

    /// Find an entry by ID within the current query's results.
    pub fn find(&mut self, query: &str, entry_id: &str) -> Option<Entry> {
        let pre = self.preprocess_query(query);
        self.entries(&pre)
            .into_iter()
            .find(|s| s.entry.id == entry_id)
    }

    /// List registered providers and their trigger prefixes.
    pub fn providers(&self) -> Vec<ProviderInfo> {
        self.providers
            .iter()
            .map(|p| ProviderInfo {
                id: p.id().into(),
                prefixes: p.prefixes().iter().map(|s| s.to_string()).collect(),
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::engine::provider::{Provider, TestProvider, entry};
    use crate::engine::scoring::MatchField;

    type CallLog = Arc<Mutex<Vec<(String, Option<String>, String)>>>;

    /// Tests only: a dry-run collection against an ephemeral folder, so no
    /// providers can touch the real data dir.
    fn collection() -> ProviderCollection {
        let dir = std::env::temp_dir().join(format!("huffi-providers-{}", std::process::id()));
        ProviderCollection::new_with_config(dir, true, &ProviderConfig::default()).unwrap()
    }

    struct TrackingProvider {
        id: String,
        prefixes: Vec<&'static str>,
        calls: CallLog,
    }

    impl TrackingProvider {
        fn new(id: &str, prefixes: Vec<&'static str>, calls: CallLog) -> Self {
            Self {
                id: id.into(),
                prefixes,
                calls,
            }
        }
    }

    impl Provider for TrackingProvider {
        fn id(&self) -> &str {
            &self.id
        }

        fn prefixes(&self) -> &[&str] {
            &self.prefixes
        }

        fn init(&mut self, _data_dir: &Path) {}

        fn query(&mut self, prefix: Option<&str>, query: &str) -> Vec<Entry> {
            self.calls.lock().unwrap().push((
                self.id.clone(),
                prefix.map(String::from),
                query.to_string(),
            ));
            vec![entry(&self.id, &self.id).history_key(&self.id).score(1.0)]
        }
    }

    #[test]
    fn preprocess_no_prefix_matches() {
        let c = collection();
        let pre = c.preprocess_query("firefox");
        assert_eq!(pre.prefix, None);
        assert_eq!(pre.original_query, "firefox");
        assert_eq!(pre.query, "firefox");
    }

    #[test]
    fn preprocess_single_prefix_matches() {
        let c = collection();
        let pre = c.preprocess_query("= 2 + 2");
        assert_eq!(pre.prefix.as_deref(), Some("="));
        assert_eq!(pre.original_query, "= 2 + 2");
        assert_eq!(pre.query, " 2 + 2");
    }

    #[test]
    fn preprocess_longest_prefix_wins() {
        let mut c = collection();
        c.add_provider(Box::new(TrackingProvider::new(
            "long",
            vec!["=="],
            Arc::new(Mutex::new(Vec::new())),
        )))
        .unwrap();
        let pre = c.preprocess_query("== 2 + 2");
        assert_eq!(pre.prefix.as_deref(), Some("=="));
        assert_eq!(pre.query, " 2 + 2");
    }

    #[test]
    fn preprocess_multiple_prefixes_on_one_provider() {
        let mut c = collection();
        c.add_provider(Box::new(TrackingProvider::new(
            "calc",
            vec!["=", "=="],
            Arc::new(Mutex::new(Vec::new())),
        )))
        .unwrap();
        let pre = c.preprocess_query("== 2");
        assert_eq!(pre.prefix.as_deref(), Some("=="));
        assert_eq!(pre.query, " 2");
    }

    #[test]
    fn entries_dispatch_original_query_to_provider_without_global_prefix() {
        let mut c = collection();
        let calls = Arc::new(Mutex::new(Vec::new()));
        c.add_provider(Box::new(TrackingProvider::new(
            "short",
            vec!["="],
            Arc::clone(&calls),
        )))
        .unwrap();
        c.add_provider(Box::new(TrackingProvider::new(
            "long",
            vec!["=="],
            Arc::clone(&calls),
        )))
        .unwrap();

        let pre = c.preprocess_query("== 2");
        let _ = c.entries(&pre);

        let log = calls.lock().unwrap();
        let short = log.iter().find(|(id, _, _)| id == "short").unwrap();
        let long = log.iter().find(|(id, _, _)| id == "long").unwrap();
        assert_eq!(short, &("short".to_string(), None, "== 2".to_string()));
        assert_eq!(
            long,
            &("long".to_string(), Some("==".to_string()), " 2".to_string())
        );
    }

    fn match_fields_entry(id: &str, text: &str) -> Entry {
        entry(id, id).history_key(id).match_fields(vec![MatchField {
            text: text.into(),
            weight: 1.0,
        }])
    }

    #[test]
    fn entries_are_stamped_with_provider_id() {
        let mut c = collection();
        c.add_provider(Box::new(TestProvider::new(
            "desktop",
            vec![match_fields_entry("desktop", "Firefox")],
        )))
        .unwrap();
        let pre = c.preprocess_query("firefox");
        let entries = c.entries(&pre);
        for e in &entries {
            assert!(e.entry.provider_id.is_some());
        }
        assert!(
            entries
                .iter()
                .any(|e| e.entry.provider_id.as_deref() == Some("desktop"))
        );
    }

    #[test]
    fn providers_lists_ids_and_prefixes() {
        let c = collection();
        let providers = c.providers();
        assert!(
            providers
                .iter()
                .any(|p| p.id == "desktop" && p.prefixes.is_empty())
        );
        assert!(
            providers
                .iter()
                .any(|p| p.id == "calculator" && p.prefixes == vec!["="])
        );
    }
}
