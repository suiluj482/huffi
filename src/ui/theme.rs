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
//!     <variant>/
//!       entry.ui         # optional, row layout for one layout variant
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
use std::rc::Rc;

use gtk4::prelude::StyleContextExt;
use gtk4::{self, gdk};

use crate::config;

/// The default theme, compiled into the binary from `data/themes/default/`.
// This path is relative to `$CARGO_MANIFEST_DIR`, so the package ships a
// self-contained binary with no runtime data directory.
const DEFAULT_THEME_DIR: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/data/themes/default");

/// Cache key for a resolved row template: the entry's provider id and its
/// layout variant, either of which may be unset.
type TemplateKey = (Option<String>, Option<String>);

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
    /// Cache of resolved entry templates, keyed by `(provider id, variant)` —
    /// `None` for either means "not set". Shared as `Rc<str>` so building a row
    /// costs an `Rc` bump rather than a fresh copy of the whole XML document.
    /// Interior mutability so [`Theme::entry_template`] can be called through
    /// `&self` from row building.
    templates: RefCell<HashMap<TemplateKey, Rc<str>>>,
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

    /// The entry-row GTK Builder template for a provider, resolved by layout
    /// variant:
    ///
    /// ```text
    /// providers/<id>/<variant>/entry.ui  →  providers/<id>/entry.ui  →  entry.ui
    /// ```
    ///
    /// A user file in any of those positions wins over the embedded default at
    /// the same position, and a missing position falls through to the next one
    /// (see [`Theme::resource`]). Variants only ever add a more specific
    /// layout; they never remove the fallbacks.
    ///
    /// The returned `Rc<str>` is shared, not reallocated, so the per-row cost is
    /// one reference bump. The XML itself still has to be parsed per row:
    /// `GtkBuilder` instantiates a single object graph, so there is no way to
    /// re-run the same definitions for a second row.
    pub fn entry_template(&self, provider_id: Option<&str>, variant: Option<&str>) -> Rc<str> {
        let key = (provider_id.map(str::to_owned), variant.map(str::to_owned));
        if let Some(cached) = self.templates.borrow().get(&key) {
            return Rc::clone(cached);
        }
        let scoped = provider_id.map(|id| format!("providers/{id}"));
        let template = variant
            .zip(scoped.as_deref())
            .and_then(|(v, dir)| self.resource(&format!("{dir}/{v}/entry.ui")))
            .or_else(|| {
                scoped
                    .as_deref()
                    .and_then(|dir| self.resource(&format!("{dir}/entry.ui")))
            })
            .or_else(|| self.resource("entry.ui"))
            .unwrap_or_default();
        let template: Rc<str> = Rc::from(template);
        self.templates
            .borrow_mut()
            .insert(key, Rc::clone(&template));
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
        let template = theme.entry_template(None, None);
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
        let template = theme.entry_template(Some("desktop"), None);
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

        let calc = theme.entry_template(Some("calculator"), None);
        assert!(calc.contains("GtkBox\" id=\"row\""), "{calc}");
        let generic = theme.entry_template(Some("desktop"), None);
        assert!(
            generic.contains("title"),
            "fallback template for other providers"
        );
    }

    /// Write a user theme containing a `providers/<id>/<variant>/entry.ui`
    /// alongside the provider-level and theme-wide templates, so the
    /// resolution chain can be asserted end to end. `tag` must be unique per
    /// test: [`temp_theme_dir`] clears the directory it returns, and the test
    /// binary runs tests on parallel threads.
    fn variant_theme(tag: &str) -> PathBuf {
        let dir = temp_theme_dir(tag);
        for (rel, body) in [
            ("entry.ui", "global"),
            ("providers/calculator/entry.ui", "provider"),
            ("providers/calculator/date/entry.ui", "variant"),
        ] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("<interface>{body}</interface>")).unwrap();
        }
        dir
    }

    #[test]
    fn variant_template_wins_over_provider_template() {
        let theme = Theme::with_root(Some(variant_theme("variant-wins")));
        let date = theme.entry_template(Some("calculator"), Some("date"));
        assert!(date.contains("variant"), "{date}");
    }

    #[test]
    fn unknown_variant_falls_back_to_provider_template() {
        let theme = Theme::with_root(Some(variant_theme("variant-unknown")));
        let number = theme.entry_template(Some("calculator"), Some("number"));
        assert!(number.contains("provider"), "{number}");
    }

    #[test]
    fn unknown_provider_and_variant_fall_back_to_global_template() {
        let theme = Theme::with_root(Some(variant_theme("variant-no-provider")));
        let other = theme.entry_template(Some("desktop"), Some("date"));
        assert!(other.contains("global"), "{other}");
    }

    #[test]
    fn variant_without_provider_id_uses_global_template() {
        let theme = Theme::with_root(Some(variant_theme("variant-unscoped")));
        let unowned = theme.entry_template(None, Some("date"));
        assert!(unowned.contains("global"), "{unowned}");
    }

    #[test]
    fn each_variant_is_cached_separately() {
        let theme = Theme::with_root(Some(variant_theme("variant-cache")));
        let date = theme.entry_template(Some("calculator"), Some("date"));
        let number = theme.entry_template(Some("calculator"), Some("number"));
        assert!(!Rc::ptr_eq(&date, &number));
        assert!(Rc::ptr_eq(
            &date,
            &theme.entry_template(Some("calculator"), Some("date"))
        ));
        assert!(Rc::ptr_eq(
            &number,
            &theme.entry_template(Some("calculator"), Some("number"))
        ));
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
        let a = theme.entry_template(Some("desktop"), None);
        // Pointer equality, not just equal contents: the second call must hand
        // back the very same allocation rather than re-resolving the theme.
        let b = theme.entry_template(Some("desktop"), None);
        assert!(Rc::ptr_eq(&a, &b), "template was reallocated, not cached");
    }

    #[test]
    fn default_template_is_cached_separately_from_provider_templates() {
        let dir = temp_theme_dir("cache-keys");
        let theme = Theme::with_root(Some(dir));
        let default = theme.entry_template(None, None);
        let desktop = theme.entry_template(Some("desktop"), None);
        assert!(!Rc::ptr_eq(&default, &desktop));
        // Both are cached, and each keeps its own identity across calls.
        assert!(Rc::ptr_eq(&default, &theme.entry_template(None, None)));
        assert!(Rc::ptr_eq(
            &desktop,
            &theme.entry_template(Some("desktop"), None)
        ));
    }
}
