use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use crate::engine::provider::{
    Entry, InitContext, Provider, ProviderMeta, ProviderResult, QueryContext, entry,
    parse_extra_config, split_command,
};
use crate::engine::scoring::MatchField;

/// History namespace for desktop entries: every entry's history key is
/// `desktop-<id>`, so a desktop id can never collide with a key from another
/// provider.
const HISTORY_PREFIX: &str = "desktop";

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
        match parse_extra_config::<DesktopConfig>(&ctx.extra) {
            Err(result) => return result,
            Ok(Some(weights)) => self.weights = weights,
            Ok(None) => {}
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
    let cwd = desktop.path().map(Path::new).map(Path::to_path_buf);
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

    if let Some(cwd) = cwd {
        e = e.cwd(cwd);
    }

    e = e.history_key(format!("{HISTORY_PREFIX}-{id}"));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::provider::{Action, ExecMode};

    fn read(content: &str, name: &str) -> Option<Entry> {
        let path =
            std::env::temp_dir().join(format!("huffi-{name}-{}.desktop", std::process::id()));
        std::fs::write(&path, content).expect("write temp desktop file");
        let result = read_desktop_entry(&path, DesktopConfig::default());
        std::fs::remove_file(&path).ok();
        result
    }

    #[test]
    fn the_path_key_becomes_the_entry_cwd() {
        let entry = read(
            "[Desktop Entry]\nType=Application\nName=Files\nExec=thunar\nTerminal=true\nPath=/tmp\n",
            "desktop-path",
        )
        .expect("entry");
        match entry.entry.action {
            Action::Exec {
                mode,
                cwd: Some(ref cwd),
                ..
            } => {
                assert_eq!(mode, ExecMode::Terminal);
                assert_eq!(cwd, Path::new("/tmp"));
            }
            other => panic!("expected a terminal exec running in /tmp, got {other:?}"),
        }
    }

    #[test]
    fn the_history_key_is_namespaced_by_the_provider_id() {
        let entry = read(
            "[Desktop Entry]\nType=Application\nName=Files\nExec=thunar\n",
            "desktop-history",
        )
        .expect("entry");
        assert_eq!(
            entry.history_key.as_deref(),
            Some(format!("{HISTORY_PREFIX}-{}", entry.entry.id).as_str())
        );
    }

    #[test]
    fn an_entry_without_a_path_key_has_no_own_cwd() {
        let entry = read(
            "[Desktop Entry]\nType=Application\nName=Files\nExec=thunar\n",
            "desktop-no-path",
        )
        .expect("entry");
        match entry.entry.action {
            Action::Exec { cwd: None, .. } => {}
            other => panic!("expected an exec without cwd, got {other:?}"),
        }
    }
}
