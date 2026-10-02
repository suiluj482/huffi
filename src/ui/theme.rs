//! Theme resolution and loading.
//!
//! A theme is a directory with the same shape as the embedded default
//! [`DEFAULT_THEME_DIR`]:
//!
//! ```text
//! <theme>/
//!   style.css            # the one stylesheet
//!   entry.ui             # default GTK Builder row template
//!   variants/<name>/
//!     entry.ui           # optional, row layout for one layout variant,
//!                        #   for every provider that reports it
//!   providers/<id>/
//!     entry.ui           # optional, custom row layout for that provider
//!     <variant>/
//!       entry.ui         # optional, row layout for one layout variant of
//!                        #   this provider
//! ```
//!
//! The user's theme lives at `$XDG_CONFIG_HOME/huffi/themes/<name>/` and is
//! selected with `[ui] theme = "<name>"`. Files in the user theme overlay the
//! embedded default file by file; the single `style.css` layers at APPLICATION
//! priority for the embedded default and USER priority for the user's override.

use std::cell::RefCell;
use std::collections::HashMap;
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

/// Resolve the overlay root for a theme name, and report whether a *named*
/// theme was asked for but not found.
///
/// `default` is embedded and so never has a directory to find; that is a
/// normal outcome, not a misconfiguration, so it is not reported.
fn user_theme_root(config_dir: Option<&std::path::Path>, name: &str) -> (Option<PathBuf>, bool) {
    let Some(config_dir) = config_dir else {
        // Nowhere to look for a user theme at all, which is not a
        // misconfiguration and must stay quiet.
        return (None, false);
    };
    let root = config_dir.join("huffi").join("themes").join(name);
    if root.is_dir() {
        (Some(root), false)
    } else {
        (None, name != "default")
    }
}

impl Theme {
    /// Resolve the selected theme. `default` is always available (embedded);
    /// any other name falls back to the embedded default when the user has no
    /// such theme directory.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        let (user_root, missing) = user_theme_root(config::config_dir().as_deref(), &name);
        // A misspelled theme name is otherwise indistinguishable from a
        // stylesheet that isn't applying: the overlay simply goes away and the
        // stock default theme renders. Worth a line on stderr.
        if missing {
            let at = config::config_dir()
                .map(|dir| dir.join("huffi").join("themes").join(&name))
                .unwrap_or_default();
            eprintln!(
                "huffi: theme {name:?} not found at {} — using the embedded default theme",
                at.display()
            );
        }
        Self {
            user_root,
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

    /// The entry-row GTK Builder template for an entry, resolved by layout
    /// variant:
    ///
    /// ```text
    /// providers/<id>/<variant>/entry.ui  →  providers/<id>/entry.ui
    ///     →  variants/<variant>/entry.ui  →  entry.ui
    /// ```
    ///
    /// The two variant positions differ only in scope. `providers/<id>/<variant>`
    /// is one provider's opinion about one variant, while `variants/<variant>` is
    /// the theme's opinion about it for *every* provider — so a theme can ship
    /// one `variants/list/entry.ui` and have every provider that reports a `list`
    /// variant pick it up, with no per-provider file. A shared template is
    /// therefore only useful if it sticks to generic widget ids (`title`,
    /// `subtitle`, `detail-<key>`) rather than a particular provider's keys.
    ///
    /// The shared position comes *after* the provider's own template, so a theme
    /// that customises one provider doesn't silently lose a variant layout that
    /// the shared file would otherwise have supplied for it.
    ///
    /// A user file in any position wins over the embedded default at the same
    /// position, and a missing position falls through to the next one (see
    /// [`Theme::resource`]). Variants only ever add a more specific layout; they
    /// never remove the fallbacks.
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
            .or_else(|| variant.and_then(|v| self.resource(&format!("variants/{v}/entry.ui"))))
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
/// priority and user overlay at USER priority.
///
/// A theme has exactly one stylesheet. `providers/<id>/style.css` is not a
/// thing: GTK registers a sheet for the whole display and cannot attach one to
/// a subtree, so per-provider sheets would all be live for all rows and would
/// resolve ties by provider registration order rather than by anything a theme
/// author chose. Rows carry `provider-<id>`, and that class is the whole
/// scoping mechanism — see `docs/THEMING.md`.
pub fn load_css(display: &gdk::Display, theme: &Theme) {
    add_css_layer(display, theme, "style.css");
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
    fn only_a_missing_named_theme_is_reported() {
        let config_dir = temp_theme_dir("named");
        // `user_theme_root` takes the config dir and appends `huffi/themes`,
        // so build a real theme directory underneath it.
        let theme_dir = config_dir.join("huffi").join("themes").join("mine");
        std::fs::create_dir_all(&theme_dir).unwrap();

        // `default` is embedded, so it never has a directory and is not a
        // misconfiguration worth reporting.
        assert_eq!(
            user_theme_root(Some(config_dir.as_path()), "default"),
            (None, false)
        );
        // An overlay that exists is not missing.
        assert_eq!(
            user_theme_root(Some(config_dir.as_path()), "mine"),
            (Some(theme_dir), false)
        );
        // A named theme with no directory disables the overlay and is reported,
        // which is what `Theme::new` turns into the stderr warning.
        assert_eq!(
            user_theme_root(Some(config_dir.as_path()), "typo"),
            (None, true)
        );
        // No config dir at all cannot be a typo, and must stay quiet.
        assert_eq!(user_theme_root(None, "mine"), (None, false));
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

    /// The calculator templates shipped in the default theme are the only
    /// in-tree templates that declare `detail-*` widgets, so they're the ones
    /// worth holding to the provider's actual key set. A `detail-<key>` widget
    /// the calculator never emits is dead markup, and a key it does emit that
    /// no template declares is a field nobody can see — both are silent, so
    /// both are worth catching here.
    ///
    /// This is a text check, not a real parse: instantiating the template would
    /// mean a `GtkBuilder` off the main thread, which gtk4 forbids. So
    /// well-formedness is still only checked by GTK at runtime; what's checked
    /// here is that the structural ids and the detail keys line up.
    #[test]
    fn shipped_calculator_templates_declare_only_real_detail_keys() {
        use huffi::engine::provider::builtin::calculator::DETAIL_KEYS;
        use huffi::engine::provider::is_detail_key;

        let dir = temp_theme_dir("shipped-calc");
        let theme = Theme::with_root(Some(dir));

        // A key that can't be a widget id is silently dropped by the renderer,
        // so a key drifting out of the character set would make a field
        // unreachable rather than fail loudly.
        for key in DETAIL_KEYS {
            assert!(
                is_detail_key(key),
                "calculator detail key {key:?} is not a usable detail-<key> widget id"
            );
        }

        for variant in [None, Some("date")] {
            let xml = theme.entry_template(Some("calculator"), variant);
            let label = variant.unwrap_or("<provider-level>");

            // Without a `row` object the renderer falls back to a bare box, so
            // a template that lost it degrades every calculator row in silence.
            assert!(
                xml.contains("id=\"row\""),
                "{label} template declares no `row` object"
            );
            assert!(
                xml.contains("id=\"clickable\""),
                "{label} template declares no `clickable` object, so rows stop \
                 being click targets"
            );

            let declared: Vec<&str> = xml
                .match_indices("id=\"detail-")
                .map(|(at, matched)| {
                    let rest = &xml[at + matched.len()..];
                    let end = rest.find('"').expect("unterminated id attribute");
                    &rest[..end]
                })
                .collect();
            assert!(!declared.is_empty(), "{label} template declares no details");

            for key in declared {
                assert!(
                    DETAIL_KEYS.contains(&key),
                    "{label} template declares detail-{key}, which the calculator never emits"
                );
            }
        }

        // Every key the calculator can emit is reachable from some shipped
        // template, so no detail is unreachable from the default theme.
        let provider_level = theme.entry_template(Some("calculator"), None).to_string();
        let date = theme
            .entry_template(Some("calculator"), Some("date"))
            .to_string();
        for key in DETAIL_KEYS {
            let declared = format!("id=\"detail-{key}\"");
            assert!(
                provider_level.contains(&declared) || date.contains(&declared),
                "calculator emits detail-{key} but no shipped template declares it"
            );
        }
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

    /// A theme with a `variants/<name>/entry.ui` that no provider claims. The
    /// "prov" provider has no template of its own, so it should fall through
    /// the provider level to the shared variant.
    fn shared_variant_theme(tag: &str) -> PathBuf {
        let dir = temp_theme_dir(tag);
        for (rel, body) in [
            ("entry.ui", "global"),
            ("providers/calc/entry.ui", "provider"),
            ("providers/calc/priv/entry.ui", "priv-variant"),
            ("variants/shared/entry.ui", "shared-variant"),
        ] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("<interface>{body}</interface>")).unwrap();
        }
        dir
    }

    #[test]
    fn shared_variant_applies_to_a_provider_with_no_template_of_its_own() {
        let theme = Theme::with_root(Some(shared_variant_theme("shared-applies")));
        let row = theme.entry_template(Some("prov"), Some("shared"));
        assert!(row.contains("shared-variant"), "{row}");
    }

    #[test]
    fn shared_variant_applies_when_there_is_no_provider_at_all() {
        let theme = Theme::with_root(Some(shared_variant_theme("shared-no-provider")));
        let row = theme.entry_template(None, Some("shared"));
        assert!(row.contains("shared-variant"), "{row}");
    }

    #[test]
    fn provider_variant_wins_over_shared_variant() {
        let theme = Theme::with_root(Some(shared_variant_theme("shared-vs-provider")));
        let row = theme.entry_template(Some("calc"), Some("priv"));
        assert!(row.contains("priv-variant"), "{row}");
    }

    /// The point of putting the shared position *after* the provider template:
    /// a theme that customises one provider keeps winning for it, rather than
    /// having its layout replaced by the shared variant file.
    #[test]
    fn provider_template_wins_over_shared_variant_for_the_same_variant_name() {
        let dir = temp_theme_dir("shared-after-provider");
        for (rel, body) in [
            ("entry.ui", "global"),
            ("providers/calc/entry.ui", "provider"),
            ("variants/shared/entry.ui", "shared-variant"),
        ] {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("<interface>{body}</interface>")).unwrap();
        }
        let theme = Theme::with_root(Some(dir));
        let row = theme.entry_template(Some("calc"), Some("shared"));
        assert!(row.contains("provider"), "{row}");
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
