use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext,
};
use std::sync::Arc;

pub struct TestProvider {
    id: String,
    prefixes: Vec<&'static str>,
    entries: Arc<[Entry]>,
}

impl TestProvider {
    pub fn new(id: &str, entries: Vec<Entry>) -> Self {
        Self {
            id: id.into(),
            prefixes: Vec::new(),
            entries: Arc::from(entries),
        }
    }

    pub fn with_prefixes(id: &str, prefixes: Vec<&'static str>, entries: Vec<Entry>) -> Self {
        Self {
            id: id.into(),
            prefixes,
            entries: Arc::from(entries),
        }
    }
}

impl Provider for TestProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta::builder(&self.id)
            .prefixes(self.prefixes.iter().copied())
            .build()
    }

    fn init(&mut self, _ctx: InitContext) -> ProviderResult {
        ProviderResult::Ok
    }

    fn query(&mut self, _ctx: QueryContext) -> Arc<[Entry]> {
        Arc::clone(&self.entries)
    }
}
