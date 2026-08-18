use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::provider::{Entry, Provider, entry, lookup_icon, split_command};
use crate::scoring::MatchField;

pub const WEIGHT_NAME: f32 = 1.0;
pub const WEIGHT_KEYWORD: f32 = 0.8;
pub const WEIGHT_GENERIC_NAME: f32 = 0.7;
pub const WEIGHT_COMMENT: f32 = 0.5;

pub struct DesktopEntryProvider {
    id: String,
    dirs: Vec<PathBuf>,
    entries: Arc<[Entry]>,
}

impl DesktopEntryProvider {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            id: "desktop".into(),
            dirs,
            entries: Arc::from([]),
        }
    }
}

impl Default for DesktopEntryProvider {
    fn default() -> Self {
        Self::new(freedesktop_desktop_entry::default_paths())
    }
}

impl Provider for DesktopEntryProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn prefixes(&self) -> &[&str] {
        &[]
    }

    fn init(&mut self) {
        self.entries = Arc::from(
            freedesktop_desktop_entry::Iter::new(self.dirs.clone())
                .filter_map(|path| read_desktop_entry(&path))
                .collect::<Vec<_>>(),
        );
    }

    fn query(&mut self, _prefix: Option<&str>, _query: &str) -> Vec<Entry> {
        self.entries.to_vec()
    }
}

fn read_desktop_entry(path: &Path) -> Option<Entry> {
    let input = std::fs::read_to_string(path).ok()?;
    let desktop = freedesktop_desktop_entry::DesktopEntry::decode(path, &input).ok()?;

    if desktop.no_display() {
        return None;
    }

    if desktop.type_() != Some("Application") {
        return None;
    }

    let name = desktop.name(None)?.into_owned();
    let exec = desktop.exec().map(|s| s.to_string())?;
    let terminal = desktop.terminal();
    let generic_name = desktop.generic_name(None).map(|s| s.into_owned());
    let comment = desktop.comment(None).map(|c| c.into_owned());
    let icon = desktop.icon().and_then(lookup_icon);
    let id = desktop.id().to_string();

    let mut match_fields = vec![MatchField {
        text: name.clone(),
        weight: WEIGHT_NAME,
    }];

    if let Some(ref c) = comment {
        match_fields.push(MatchField {
            text: c.clone(),
            weight: WEIGHT_COMMENT,
        });
    }

    if let Some(ref g) = generic_name {
        match_fields.push(MatchField {
            text: g.clone(),
            weight: WEIGHT_GENERIC_NAME,
        });
    }

    if let Some(kw) = desktop.keywords() {
        for word in kw.split(';') {
            let word = word.trim();
            if !word.is_empty() {
                match_fields.push(MatchField {
                    text: word.to_string(),
                    weight: WEIGHT_KEYWORD,
                });
            }
        }
    }

    let exec_args: Vec<String> = split_command(&exec)
        .into_iter()
        .filter(|arg| !(arg.starts_with('%') && arg.len() == 2 && arg.as_bytes()[1].is_ascii_alphabetic()))
        .collect();

    let mut e = if terminal {
        entry(&id, &name).terminal_exec(exec_args)
    } else {
        entry(&id, &name).exec(exec_args)
    };

    e = e.history_key(&id);
    if let Some(c) = comment { e = e.comment(c); }
    if let Some(g) = generic_name { e = e.subtitle(g); }
    if let Some(i) = icon { e = e.icon(i); }

    Some(e.match_fields(match_fields))
}
