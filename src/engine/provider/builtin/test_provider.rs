use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext,
};

pub struct TestProvider {
    id: String,
    prefixes: Vec<&'static str>,
    entries: Vec<Entry>,
}

impl TestProvider {
    pub fn new(id: &str, entries: Vec<Entry>) -> Self {
        Self {
            id: id.into(),
            prefixes: Vec::new(),
            entries,
        }
    }

    pub fn with_prefixes(id: &str, prefixes: Vec<&'static str>, entries: Vec<Entry>) -> Self {
        Self {
            id: id.into(),
            prefixes,
            entries,
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

    fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
        self.entries.clone()
    }
}
