use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry, split_command,
};
use crate::engine::scoring::MatchField;

/// Fuzzy-match field weights for this provider. When provided via
/// `[engine.provider.builtin.desktop.extra]`, the fields are parsed from
/// the arbitrary extra config.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(default)]
pub struct DesktopConfig {
    pub weight_name: f32,
    pub weight_keyword: f32,
    pub weight_generic_name: f32,
    pub weight_comment: f32,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            weight_name: 1.0,
            weight_keyword: 0.8,
            weight_generic_name: 0.7,
            weight_comment: 0.5,
        }
    }
}

pub struct DesktopEntryProvider {
    dirs: Vec<PathBuf>,
    weights: DesktopConfig,
    entries: Arc<[Entry]>,
}

impl DesktopEntryProvider {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            weights: DesktopConfig::default(),
            entries: Arc::from([]),
        }
    }
}

impl Default for DesktopEntryProvider {
    fn default() -> Self {
        Self::new(freedesktop_desktop_entry::default_paths().collect())
    }
}

impl Provider for DesktopEntryProvider {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta::builder("desktop").build()
    }

    fn init(&mut self, ctx: InitContext) -> ProviderResult {
        // Parse extra config into DesktopConfig if provided.
        if let Some(ref extra) = ctx.extra {
            match serde_json::from_value::<DesktopConfig>(extra.clone()) {
                Ok(weights) => self.weights = weights,
                Err(e) => {
                    return ProviderResult::Config {
                        msg: format!("invalid extra config: {e}"),
                        critical: false,
                    }
                }
            }
        }

        let weights = self.weights;
        self.entries = Arc::from(
            freedesktop_desktop_entry::Iter::new(self.dirs.clone().into_iter())
                .filter_map(|path| read_desktop_entry(&path, weights))
                .collect::<Vec<_>>(),
        );
        ProviderResult::Ok
    }

    fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
        self.entries.to_vec()
    }
}

fn read_desktop_entry(path: &Path, weights: DesktopConfig) -> Option<Entry> {
    let input = std::fs::read_to_string(path).ok()?;
    let desktop =
        freedesktop_desktop_entry::DesktopEntry::from_str(path, &input, None::<&[&str]>).ok()?;

    if desktop.no_display() {
        return None;
    }

    if desktop.type_() != Some("Application") {
        return None;
    }

    let name = desktop.name::<&str>(&[])?.into_owned();
    let exec = desktop.exec().map(|s| s.to_string())?;
    let terminal = desktop.terminal();
    let generic_name = desktop.generic_name::<&str>(&[]).map(|s| s.into_owned());
    let comment = desktop.comment::<&str>(&[]).map(|c| c.into_owned());
    let icon = desktop.icon().map(|s| s.to_owned());
    let id = desktop.id().to_string();

    let mut match_fields = vec![MatchField {
        text: name.clone(),
        weight: weights.weight_name,
    }];

    if let Some(ref c) = comment {
        match_fields.push(MatchField {
            text: c.clone(),
            weight: weights.weight_comment,
        });
    }

    if let Some(ref g) = generic_name {
        match_fields.push(MatchField {
            text: g.clone(),
            weight: weights.weight_generic_name,
        });
    }

    if let Some(kw) = desktop.keywords::<&str>(&[]) {
        for word in kw {
            let word = word.trim();
            if !word.is_empty() {
                match_fields.push(MatchField {
                    text: word.to_string(),
                    weight: weights.weight_keyword,
                });
            }
        }
    }

    let exec_args: Vec<String> = split_command(&exec)
        .into_iter()
        .filter(|arg| {
            !(arg.starts_with('%') && arg.len() == 2 && arg.as_bytes()[1].is_ascii_alphabetic())
        })
        .collect();

    let mut e = if terminal {
        entry(&id, &name).terminal_exec(exec_args)
    } else {
        entry(&id, &name).exec(exec_args)
    };

    e = e.history_key(&id);
    if let Some(c) = comment {
        e = e.comment(c);
    }
    if let Some(g) = generic_name {
        e = e.subtitle(g);
    }
    if let Some(i) = icon {
        e = e.icon_name(i);
    }

    Some(e.match_fields(match_fields))
}
