use rink_core::{one_line, simple_context};

use crate::engine::provider::{
    Entry, Icon, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
};

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
        ProviderMeta {
            id: "calculator".into(),
            prefixes: vec!["=".into()],
            enabled: true,
        }
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
        let Some(_prefix) = ctx.prefix else {
            return vec![];
        };

        let Some(rink) = self.rink.as_mut() else {
            return vec![];
        };

        let result = if ctx.query.is_empty() {
            Ok("type to calculate".into())
        } else {
            one_line(rink, ctx.query)
        };

        let (title, value) = match result {
            Ok(value) => (value.clone(), Some(value)),
            Err(err) => (err, None),
        };

        let mut e = entry("huffi-calculator", &title).history_key("huffi-calculator");

        if let Some(value) = value {
            e = e.clipboard(value.clone());
            e = e.set_query(format!("={value}"));
        }

        if let Some(ref icon) = self.icon {
            e = e.icon(icon.clone());
        }

        vec![e.score(1.0)]
    }
}
