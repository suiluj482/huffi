//! Theme resolution and loading.
//!
//! A theme is a directory with the same shape as the embedded default
//! [`DEFAULT_THEME_DIR`]:
//!
//! ```text
//! <theme>/
//!   style.css            # global stylesheet
//!   entry.ui             # default GTK Builder row template
//!   providers/<id>/
//!     style.css          # optional, scoped to that provider's rows
//!     entry.ui           # optional, custom row layout for that provider
//! ```
//!
//! The user's theme lives at `$XDG_CONFIG_HOME/huffi/themes/<name>/` and is
//! selected with `[ui] theme = "<name>"`. Files in the user theme overlay the
//! embedded default file by file; CSS layers are registered at APPLICATION
//! priority for the embedded default and USER priority for the user's
//! override. The legacy `~/.config/huffi/style.css` keeps being loaded on top
//! for backwards compatibility.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use gtk4::prelude::StyleContextExt;
use gtk4::{self, gdk};

use crate::config;

/// The default theme, compiled into the binary from `data/themes/default/`.
// This path is relative to `$CARGO_MANIFEST_DIR`, so the package ships a
// self-contained binary with no runtime data directory.
const DEFAULT_THEME_DIR: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/data/themes/default");

/// Resolve the accent color from the stylesheet (`@define-color
/// huffi_mauve_color`), falling back to the default value if the theme
/// doesn't define it.
pub fn mauve(context: &gtk4::StyleContext) -> (f64, f64, f64) {
    if let Some(color) = context.lookup_color("huffi_mauve_color") {
        return (
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
        );
    }
    (
        0xcb as f64 / 255.0,
        0xa6 as f64 / 255.0,
        0xf7 as f64 / 255.0,
    )
}

/// A resolved theme: the embedded default plus an optional user overlay.
pub struct Theme {
    /// `config_dir/huffi/themes/<name>/` when that directory exists.
    user_root: Option<PathBuf>,
    /// Cache of resolved entry templates per provider id. Interior mutability
    /// so [`Theme::entry_template`] can be called through `&self` from row
    /// building.
    templates: RefCell<HashMap<String, String>>,
}

impl Theme {
    /// Resolve the selected theme. `default` is always available (embedded);
    /// any other name falls back to the embedded default when the user has no
    /// such theme directory.
    pub fn new(name: impl Into<String>) -> Self {
        let user_root =
            config::config_dir().map(|dir| dir.join("huffi").join("themes").join(name.into()));
        Self {
            user_root: user_root.filter(|root| root.is_dir()),
            templates: RefCell::new(HashMap::new()),
        }
    }

    /// Construct with an explicit theme root (tests only). `None` disables the
    /// user overlay entirely.
    #[cfg(test)]
    fn with_root(user_root: Option<PathBuf>) -> Self {
        Self {
            user_root,
            templates: RefCell::new(HashMap::new()),
        }
    }

    /// Resolve a file inside the theme: the user overlay wins when present,
    /// otherwise the embedded default. `None` when neither has it.
    fn resource(&self, rel: &str) -> Option<String> {
        if let Some(root) = &self.user_root {
            let path = root.join(rel);
            if path.is_file() {
                return std::fs::read_to_string(&path).ok();
            }
        }
        DEFAULT_THEME_DIR
            .get_file(rel)
            .and_then(|file| file.contents_utf8())
            .map(str::to_owned)
    }

    /// Read `rel` from the user overlay only (no embedded fallback).
    fn user_resource(&self, rel: &str) -> Option<String> {
        let path = self.user_root.as_ref()?.join(rel);
        path.is_file()
            .then(|| std::fs::read_to_string(&path).ok())
            .flatten()
    }

    /// The entry-row GTK Builder template for a provider. A provider-specific
    /// `providers/<id>/entry.ui` wins over the theme's default `entry.ui`.
    pub fn entry_template(&self, provider_id: Option<&str>) -> String {
        let key = provider_id.unwrap_or("");
        if let Some(cached) = self.templates.borrow().get(key) {
            return cached.clone();
        }
        let template = provider_id
            .and_then(|id| self.resource(&format!("providers/{id}/entry.ui")))
            .or_else(|| self.resource("entry.ui"))
            .unwrap_or_default();
        self.templates
            .borrow_mut()
            .insert(key.to_owned(), template.clone());
        template
    }
}

/// Register the global `style.css` stylesheet, default layer at APPLICATION
/// priority and user overlay (plus the legacy `~/.config/huffi/style.css`) at
/// USER priority.
pub fn load_css(display: &gdk::Display, theme: &Theme) {
    add_css_layer(display, theme, "style.css");

    if let Some(config_dir) = config::config_dir() {
        let user_css = config_dir.join("huffi").join("style.css");
        if user_css.exists() {
            add_css(
                display,
                &css_from_path(&user_css),
                gtk4::STYLE_PROVIDER_PRIORITY_USER,
            );
        }
    }
}

/// Register each provider's scoped `providers/<id>/style.css`, same default/
/// user layering as the global stylesheet. Called once the provider list is
/// known.
pub fn load_provider_css(display: &gdk::Display, theme: &Theme, provider_ids: &[String]) {
    for id in provider_ids {
        add_css_layer(display, theme, &format!("providers/{id}/style.css"));
    }
}

/// Add one stylesheet: the embedded default at APPLICATION priority, with the
/// user overlay (if any) layered on top at USER priority.
fn add_css_layer(display: &gdk::Display, theme: &Theme, rel: &str) {
    if let Some(css) = DEFAULT_THEME_DIR
        .get_file(rel)
        .and_then(|file| file.contents_utf8())
    {
        add_css(display, css, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    if let Some(css) = theme.user_resource(rel) {
        add_css(display, &css, gtk4::STYLE_PROVIDER_PRIORITY_USER);
    }
}

fn css_from_path(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn add_css(display: &gdk::Display, css: &str, priority: u32) {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(css);
    gtk4::style_context_add_provider_for_display(display, &provider, priority);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_theme_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("huffi-theme-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn embedded_default_resolves_when_user_file_absent() {
        let dir = temp_theme_dir("missing");
        let theme = Theme::with_root(Some(dir));
        let css = theme.resource("style.css").expect("embedded style.css");
        assert!(css.contains("@define-color"));
        let template = theme.entry_template(None);
        assert!(template.contains("id=\"row\""));
    }

    #[test]
    fn user_file_wins_over_embedded() {
        let dir = temp_theme_dir("override");
        std::fs::write(dir.join("style.css"), "/* user css */").unwrap();
        let theme = Theme::with_root(Some(dir));
        assert_eq!(
            theme.resource("style.css").as_deref(),
            Some("/* user css */")
        );
    }

    #[test]
    fn missing_user_resource_falls_back_to_embedded() {
        let dir = temp_theme_dir("fallback");
        std::fs::write(dir.join("style.css"), "/* user css */").unwrap();
        let theme = Theme::with_root(Some(dir));
        let template = theme.entry_template(Some("desktop"));
        assert!(
            template.contains("id=\"row\""),
            "a provider without its own entry.ui must use the default template"
        );
    }

    #[test]
    fn provider_template_wins_when_present() {
        let dir = temp_theme_dir("provider");
        let provider_dir = dir.join("providers").join("calculator");
        std::fs::create_dir_all(&provider_dir).unwrap();
        std::fs::write(
            provider_dir.join("entry.ui"),
            "<interface><object class=\"GtkBox\" id=\"row\"/></interface>",
        )
        .unwrap();
        let theme = Theme::with_root(Some(dir));

        let calc = theme.entry_template(Some("calculator"));
        assert!(calc.contains("GtkBox\" id=\"row\""), "{calc}");
        let generic = theme.entry_template(Some("desktop"));
        assert!(
            generic.contains("title"),
            "fallback template for other providers"
        );
    }

    #[test]
    fn user_resource_only_reads_user_layer() {
        let dir = temp_theme_dir("user-only");
        std::fs::write(dir.join("style.css"), "/* u */").unwrap();
        let theme = Theme::with_root(Some(dir));
        assert_eq!(theme.user_resource("style.css").as_deref(), Some("/* u */"));

        let none = Theme::with_root(None);
        assert_eq!(none.user_resource("style.css"), None);
    }

    #[test]
    fn entry_template_is_cached() {
        let dir = temp_theme_dir("cache");
        let theme = Theme::with_root(Some(dir));
        let a = theme.entry_template(Some("desktop"));
        // A second identical call returns the same cached value.
        let b = theme.entry_template(Some("desktop"));
        assert_eq!(a, b);
    }
}
