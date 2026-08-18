use rink_core::{simple_context, one_line};

use crate::provider::{Entry, Provider, entry, lookup_icon};

pub struct CalculatorProvider {
    id: String,
    ctx: Option<rink_core::Context>,
    icon: Option<String>,
}

impl Default for CalculatorProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CalculatorProvider {
    pub fn new() -> Self {
        Self {
            id: "calculator".into(),
            ctx: None,
            icon: None,
        }
    }
}

impl Provider for CalculatorProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn prefixes(&self) -> &[&str] {
        &["="]
    }

    fn init(&mut self) {
        match simple_context() {
            Ok(ctx) => {
                eprintln!("[calculator] rink context initialized");
                self.ctx = Some(ctx);
            }
            Err(e) => {
                eprintln!("[calculator] failed to init rink context: {e}");
            }
        }
        self.icon = lookup_icon("accessories-calculator");
    }

    fn query(&mut self, prefix: Option<&str>, query: &str) -> Vec<Entry> {
        let Some(_prefix) = prefix else {
            return vec![];
        };

        let Some(ctx) = self.ctx.as_mut() else {
            eprintln!("[calculator] no rink context");
            return vec![];
        };

        let result = if query.is_empty() {
            Ok("type to calculate".into())
        } else {
            one_line(ctx, query)
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
            e = e.icon(icon);
        }

        vec![e.score(1.0)]
    }
}
