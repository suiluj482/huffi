use crate::engine::provider::{Entry, Provider};

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
    fn id(&self) -> &str {
        &self.id
    }

    fn prefixes(&self) -> &[&str] {
        &self.prefixes
    }

    fn init(&mut self) {}

    fn query(&mut self, _prefix: Option<&str>, _query: &str) -> Vec<Entry> {
        self.entries.clone()
    }
}
