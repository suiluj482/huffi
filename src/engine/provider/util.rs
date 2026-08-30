use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::engine::config::ExternalConfig;
use crate::engine::scoring::{MatchField, Rank};

use super::{Entry, EntryMeta, Icon};

#[derive(Debug, Clone)]
pub enum Action {
    Exec {
        args: Vec<String>,
        terminal: bool,
    },
    /// Copy `value` to the clipboard on selection. The clipboard binary is
    /// resolved from config when the action is performed.
    Clipboard {
        value: String,
    },
    NoOp,
}

impl Action {
    /// Perform the action, resolving external binaries (terminal wrapper,
    /// clipboard tool) from `external`.
    pub fn perform(&self, external: &ExternalConfig) {
        let args: Vec<String> = match self {
            Action::Exec {
                args,
                terminal: false,
            } => args.clone(),
            Action::Exec {
                args,
                terminal: true,
            } => {
                let mut cmd = external.terminal.clone();
                cmd.extend(args.iter().cloned());
                cmd
            }
            Action::Clipboard { value } => {
                vec![external.clipboard.clone(), value.clone()]
            }
            Action::NoOp => return,
        };
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
    icon: Option<Icon>,
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

    /// Set the icon to a themed icon name (default `impl Into<Icon>` maps
    /// strings to [`Icon::Name`]).
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Set the icon to a themed icon name (freedesktop icon theme).
    pub fn icon_name(mut self, name: impl Into<String>) -> Self {
        self.icon = Some(Icon::Name(name.into()));
        self
    }

    /// Set the icon to an explicit file path (PNG or SVG).
    pub fn icon_path(mut self, path: impl AsRef<Path>) -> Self {
        self.icon = Some(Icon::Path(path.as_ref().to_owned()));
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

    /// Set the action to copy `value` to the clipboard on selection. The
    /// clipboard binary comes from config at perform time.
    pub fn clipboard(mut self, value: impl Into<String>) -> Self {
        self.action = Some(Action::Clipboard {
            value: value.into(),
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
