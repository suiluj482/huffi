use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::engine::provider::ICON_SIZE;
use crate::engine::scoring::{MatchField, Rank};

use super::{Entry, EntryMeta};

const TERMINAL: &str = "kitty";
const CLIPBOARD: &str = "wl-copy";

#[derive(Debug, Clone)]
pub enum Action {
    Exec { args: Vec<String>, terminal: bool },
    NoOp,
}

impl Action {
    pub fn perform(&self) {
        match self {
            Action::Exec { args, terminal } => {
                let mut args = args.clone();
                if *terminal {
                    args.splice(0..0, [TERMINAL.to_owned(), "--".to_owned()]);
                }
                let Some(program) = args.first() else {
                    return;
                };
                let result = Command::new(program)
                    .args(&args[1..])
                    .process_group(0)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                if let Err(e) = result {
                    eprintln!("failed to launch {program}: {e}");
                }
            }
            Action::NoOp => {}
        }
    }
}

pub fn entry(id: impl Into<String>, title: impl Into<String>) -> EntryBuilder {
    EntryBuilder {
        id: id.into(),
        title: title.into(),
        provider_id: None,
        subtitle: None,
        comment: None,
        icon: None,
        extra: None,
        action: None,
        rank: None,
        history_key: None,
        set_query: None,
    }
}

pub struct EntryBuilder {
    id: String,
    title: String,
    provider_id: Option<String>,
    subtitle: Option<String>,
    comment: Option<String>,
    icon: Option<String>,
    extra: Option<serde_json::Value>,
    action: Option<Action>,
    rank: Option<Rank>,
    history_key: Option<String>,
    set_query: Option<String>,
}

impl EntryBuilder {
    pub fn provider(mut self, id: impl Into<String>) -> Self {
        self.provider_id = Some(id.into());
        self
    }

    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.subtitle = Some(s.into());
        self
    }

    pub fn comment(mut self, s: impl Into<String>) -> Self {
        self.comment = Some(s.into());
        self
    }

    pub fn icon(mut self, s: impl Into<String>) -> Self {
        self.icon = Some(s.into());
        self
    }

    pub fn extra(mut self, v: serde_json::Value) -> Self {
        self.extra = Some(v);
        self
    }

    pub fn exec(mut self, args: Vec<String>) -> Self {
        self.action = Some(Action::Exec {
            args,
            terminal: false,
        });
        self
    }

    pub fn terminal_exec(mut self, args: Vec<String>) -> Self {
        self.action = Some(Action::Exec {
            args,
            terminal: true,
        });
        self
    }

    /// Set the action to copy `value` to the clipboard via `wl-copy`.
    pub fn clipboard(mut self, value: impl Into<String>) -> Self {
        self.action = Some(Action::Exec {
            args: vec![CLIPBOARD.to_owned(), value.into()],
            terminal: false,
        });
        self
    }

    pub fn history_key(mut self, key: impl Into<String>) -> Self {
        self.history_key = Some(key.into());
        self
    }

    /// Set the query suggestion the UI applies when this entry is tab-selected.
    pub fn set_query(mut self, query: impl Into<String>) -> Self {
        self.set_query = Some(query.into());
        self
    }

    pub fn score(mut self, score: f32) -> Entry {
        self.rank = Some(Rank::Score(score));
        self.build()
    }

    pub fn match_fields(mut self, fields: Vec<MatchField>) -> Entry {
        self.rank = Some(Rank::MatchFields(fields));
        self.build()
    }

    /// Convenience for a single fuzzy-match field at weight 1.0.
    pub fn match_field(mut self, text: impl Into<String>) -> Entry {
        self.rank = Some(Rank::MatchFields(vec![MatchField {
            text: text.into(),
            weight: 1.0,
        }]));
        self.build()
    }

    fn build(self) -> Entry {
        Entry {
            entry: EntryMeta {
                id: self.id,
                provider_id: self.provider_id,
                title: self.title,
                subtitle: self.subtitle,
                comment: self.comment,
                icon: self.icon,
                extra: self.extra,
                set_query: self.set_query,
                action: self.action.unwrap_or(Action::NoOp),
            },
            rank: self.rank.unwrap_or(Rank::Score(1.0)),
            history_key: self.history_key,
        }
    }
}

pub fn lookup_icon(name: &str) -> Option<String> {
    freedesktop_icons::lookup(name)
        .with_size(ICON_SIZE)
        .find()
        .map(|p| p.to_string_lossy().into_owned())
}

pub fn split_command(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in s.chars() {
        match ch {
            '"' if !in_quotes => {
                in_quotes = true;
            }
            '"' if in_quotes => {
                in_quotes = false;
            }
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        result.push(current);
    }

    result
}
