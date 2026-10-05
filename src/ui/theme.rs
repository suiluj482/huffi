//! Theme resolution and loading.
//!
//! A theme is a directory with the same shape as any other theme:
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
//! Themes come from two places, and a selected name is looked for in both.
//! **Builtin** themes ship in the binary, compiled from `data/themes/<name>/`
//! by [`BUILTIN_THEMES`]; every directory there is a theme, so adding one needs
//! no registration. `default` is a builtin theme and doubles as the *base*
//! theme: the layer every other theme falls back to for a file it doesn't have.
//! A **user** theme lives at `$XDG_CONFIG_HOME/huffi/themes/<name>/` and is
//! selected with `[ui] theme = "<name>"`.
//!
//! Resolution is per file, lowest layer first:
//!
//! ```text
//! base builtin theme  →  selected builtin theme  →  user theme
//! ```
//!
//! So the layers compose rather than compete: a builtin theme that ships only
//! `style.css` still gets every row template from the base theme, and a user
//! theme that ships only `style.css` still gets a stylesheet and a layout. A
//! name matching neither a builtin nor a user directory warns and renders the
//! base theme.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::prelude::StyleContextExt;
use gtk4::{self, gdk};

use crate::config;

/// Every theme shipped in the binary, keyed by directory name under
/// `data/themes/`.
// This path is relative to `$CARGO_MANIFEST_DIR`, so the package ships a
// self-contained binary with no runtime data directory. Embedding the parent
// rather than one theme is what makes the set open-ended: a new directory is a
// theme, with nothing to register here and nothing to update in Rust.
static BUILTIN_THEMES: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/data/themes");

/// The builtin theme every other theme falls back to, and what an unknown theme
/// name renders as.
///
/// Everything the loader guarantees exists — a stylesheet, a root `entry.ui` —
/// comes from here, so a builtin theme can be as small as one file.
const BASE_THEME: &str = "default";

/// The built-in themes, for error messages and tests. Order is the embedded
/// directory's, so it is not meaningful.
pub fn builtin_themes() -> impl Iterator<Item = &'static str> {
    BUILTIN_THEMES.dirs().map(theme_name)
}

/// The builtin theme named `name`, as the name `[ui] theme` selects it by.
/// `None` if there is no such builtin theme.
///
/// The name is borrowed from the embedded tree rather than copied out of the
/// caller's string, so resolving a theme allocates nothing.
fn builtin_theme(name: &str) -> Option<&'static str> {
    BUILTIN_THEMES.get_dir(name).map(theme_name)
}

/// The name `[ui] theme` selects a builtin theme by, which is just its
/// directory under `data/themes/`. `include_dir` exposes a directory only as its
/// path relative to the embedded root, and for a theme the two are the same.
fn theme_name(dir: &'static include_dir::Dir<'static>) -> &'static str {
    dir.path()
        .to_str()
        .expect("builtin theme name is not UTF-8")
}

/// A file in the builtin theme `name`, as text. `None` if that theme doesn't
/// have it, or it isn't UTF-8.
///
/// Looked up on the embedded root rather than on the theme's own subdirectory,
/// because `include_dir` resolves a lookup path against the root even when it
/// is called on a subdirectory — `default`.`get_file("style.css")` misses, and
/// `data/themes`.`get_file("default/style.css")` hits.
fn builtin_file(name: &str, rel: &str) -> Option<String> {
    BUILTIN_THEMES
        .get_file(format!("{name}/{rel}"))
        .and_then(|file| file.contents_utf8())
        .map(str::to_owned)
}

/// Cache key for a resolved row template: the entry's provider id and its
/// layout variant, either of which may be unset.
type TemplateKey = (Option<String>, Option<String>);

/// Resolve the accent colour from the stylesheet (`@define-color
/// huffi_accent_color`), falling back to the base theme's own accent if the
/// selected theme doesn't define it.
///
/// The accent is read here rather than styled because the scroll rail is drawn,
/// not a widget, so no CSS rule can reach it.
///
/// `huffi_mauve_color` is still accepted as a legacy spelling. The base theme
/// used to *be* the Catppuccin mauve palette, so a theme written against that
/// release names its accent after it; renaming the variable without reading the
/// old name would silently drop such a theme's rail colour, which is the one
/// part of a theme with no CSS rule to notice it missing.
pub fn accent(context: &gtk4::StyleContext) -> (f64, f64, f64) {
    for name in ["huffi_accent_color", "huffi_mauve_color"] {
        if let Some(color) = context.lookup_color(name) {
            return (
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
            );
        }
    }
    (
        0x8a as f64 / 255.0,
        0xa9 as f64 / 255.0,
        0xc8 as f64 / 255.0,
    )
}

/// A resolved theme: one builtin theme — the base one, or the selected one —
/// plus an optional user overlay above it.
pub struct Theme {
    /// Name of the selected builtin theme, when `data/themes/<name>/` exists.
    /// `None` for a user-only theme, which still resolves against the base
    /// theme underneath it.
    builtin: Option<&'static str>,
    /// `config_dir/huffi/themes/<name>/` when that directory exists.
    user_root: Option<PathBuf>,
    /// Cache of resolved entry templates, keyed by `(provider id, variant)` —
    /// `None` for either means "not set". Shared as `Rc<str>` so building a row
    /// costs an `Rc` bump rather than a fresh copy of the whole XML document.
    /// Interior mutability so [`Theme::entry_template`] can be called through
    /// `&self` from row building.
    templates: RefCell<HashMap<TemplateKey, Rc<str>>>,
}

/// Where a named theme's user overlay lives: `<config_dir>/huffi/themes/<name>/`.
fn theme_root(config_dir: &std::path::Path, name: &str) -> PathBuf {
    config_dir.join("huffi").join("themes").join(name)
}

/// Resolve the overlay root for a theme name, and report whether the name
/// matched neither a builtin theme nor a user directory.
fn user_theme_root(config_dir: Option<&std::path::Path>, name: &str) -> (Option<PathBuf>, bool) {
    let Some(config_dir) = config_dir else {
        // Nowhere to look for a user theme at all, which is not a
        // misconfiguration and must stay quiet.
        return (None, false);
    };
    let root = theme_root(config_dir, name);
    if root.is_dir() {
        return (Some(root), false);
    }
    // A builtin theme needs no directory to be found in, so a name that matches
    // one is a normal outcome rather than a misconfiguration.
    (None, builtin_theme(name).is_none())
}

impl Theme {
    /// Resolve the selected theme. Every builtin theme is always available;
    /// any other name resolves against the user overlay on top of the base
    /// theme, and warns when there is no such directory.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        // The name is borrowed from the embedded tree rather than copied out of
        // `name`, so the theme string lives as long as the binary does and
        // selecting a theme allocates nothing.
        let builtin = builtin_theme(&name);
        let config_dir = config::config_dir();
        let (user_root, missing) = user_theme_root(config_dir.as_deref(), &name);
        // A misspelled theme name is otherwise indistinguishable from a
        // stylesheet that isn't applying: the overlay simply goes away and the
        // stock base theme renders. Worth a line on stderr — and worth naming the
        // builtin themes there, since a typo in one of those is the likelier
        // mistake and the list is otherwise only in the source tree.
        //
        // `missing` implies a config dir, since `user_theme_root` reports a
        // missing named theme only when it had somewhere to look.
        if let Some(dir) = config_dir.filter(|_| missing) {
            eprintln!(
                "huffi: unknown theme {name:?} — no {} directory. Falling back to the \
                 embedded {BASE_THEME:?} theme; the builtin themes are {}.",
                theme_root(&dir, &name).display(),
                builtin_themes().collect::<Vec<_>>().join(", "),
            );
        }
        Self {
            builtin,
            user_root,
            templates: RefCell::new(HashMap::new()),
        }
    }

    /// Construct with an explicit user overlay root and no builtin theme of its
    /// own (tests only). `None` disables the overlay entirely.
    #[cfg(test)]
    fn with_root(user_root: Option<PathBuf>) -> Self {
        Self::with_layers(None, user_root)
    }

    /// Construct with an explicit builtin theme and user overlay root (tests
    /// only).
    #[cfg(test)]
    fn with_layers(builtin: Option<&'static str>, user_root: Option<PathBuf>) -> Self {
        Self {
            builtin,
            user_root,
            templates: RefCell::new(HashMap::new()),
        }
    }

    /// Resolve a file inside the theme: the user overlay wins when present,
    /// then the selected builtin theme, then the base theme. `None` when none
    /// of them has it.
    fn resource(&self, rel: &str) -> Option<String> {
        self.user_resource(rel)
            .or_else(|| self.builtin_resource(rel))
    }

    /// Read `rel` from the builtin layers only: the selected builtin theme's own
    /// copy, falling back to the base theme's.
    fn builtin_resource(&self, rel: &str) -> Option<String> {
        self.builtin
            .and_then(|name| builtin_file(name, rel))
            .or_else(|| builtin_file(BASE_THEME, rel))
    }

    /// Read `rel` from the user overlay only (no builtin fallback).
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
    /// A file in any position wins over the base theme's file at the same
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

/// Register the global `style.css` stylesheet: the selected builtin theme's —
/// the base theme's when it has none of its own — at GTK's `APPLICATION`
/// priority, then the user's stylesheet over it at `USER`.
///
/// Within the builtin layers this is a *replacement* rather than a cascade, so a
/// builtin theme's stylesheet has to be complete on its own. There is no
/// priority between two `APPLICATION` providers that can be relied on, and
/// inventing one would mean a third layer for a rule a theme author can simply
/// write down. The user layer still layers, because that is a layering a user
/// is expected to do partially.
///
/// A theme has exactly one stylesheet. `providers/<id>/style.css` is not a
/// thing: GTK registers a sheet for the whole display and cannot attach one to
/// a subtree, so per-provider sheets would all be live for all rows and would
/// resolve ties by provider registration order rather than by anything a theme
/// author chose. Rows carry `provider-<id>`, and that class is the whole
/// scoping mechanism — see `docs/THEMING.md`.
pub fn load_css(display: &gdk::Display, theme: &Theme) {
    if let Some(css) = theme.builtin_resource("style.css") {
        add_css(display, &css, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    if let Some(css) = theme.user_resource("style.css") {
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

    /// A temp theme directory that removes itself when the test's binding drops,
    /// so a failing or passing run leaves nothing behind in `$TMPDIR`. `tag` must be
    /// unique per test: the path is keyed by process id, and the test binary runs
    /// tests on parallel threads.
    struct TempTheme(PathBuf);

    impl TempTheme {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("huffi-theme-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }

        /// The directory as a `PathBuf`, for handing to `Theme::with_root`.
        fn owned(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for TempTheme {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Structural ids the shipped templates add for themselves, on top of the
    /// ids the renderer binds. A theme may add as many as it likes; the shipped
    /// ones are listed so that introducing another is a reviewed edit rather
    /// than an unreviewed attribute.
    const AUTHOR_IDS: &[&str] = &["details-area", "title-line"];

    /// Every `.ui` template in every builtin theme, as (path, contents).
    ///
    /// [`include_dir::Dir::files`] only lists a directory's immediate children,
    /// so `providers/` and below need walking by hand. Paths are already
    /// relative to the embedded root, so they carry their own theme name and a
    /// bad template says which theme it came from.
    fn shipped_templates() -> Vec<(String, String)> {
        fn collect(dir: &include_dir::Dir<'static>, out: &mut Vec<(String, String)>) {
            for file in dir.files() {
                let path = file.path().display().to_string();
                if file.path().extension().is_some_and(|ext| ext == "ui") {
                    let body = file.contents_utf8().expect("shipped template is UTF-8");
                    out.push((path, strip_xml_comments(body)));
                }
            }
            for sub in dir.dirs() {
                collect(sub, out);
            }
        }
        let mut out = Vec::new();
        collect(&BUILTIN_THEMES, &mut out);
        assert!(!out.is_empty(), "no templates found in the builtin themes");
        out
    }

    /// Drop `<!-- … -->` so prose in a template's comment isn't mistaken for
    /// markup. Good enough for the plain, comment-free XML GTK Builder takes.
    fn strip_xml_comments(xml: &str) -> String {
        let mut out = String::with_capacity(xml.len());
        let mut rest = xml;
        while let Some(start) = rest.find("<!--") {
            out.push_str(&rest[..start]);
            match rest[start..].find("-->") {
                Some(end) => rest = &rest[start + end + 3..],
                // Unterminated comment: keep what follows it rather than
                // silently dropping the rest of the file.
                None => {
                    out.push_str(&rest[start..]);
                    return out;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Every `id="…"` value in a template, in document order.
    fn widget_ids(xml: &str) -> Vec<&str> {
        xml.match_indices("id=\"")
            .map(|(at, matched)| {
                let rest = &xml[at + matched.len()..];
                let end = rest.find('"').expect("unterminated id attribute");
                &rest[..end]
            })
            .collect()
    }

    #[test]
    fn only_a_missing_named_theme_is_reported() {
        let config_dir = TempTheme::new("named");
        // `user_theme_root` takes the config dir and appends `huffi/themes`,
        // so build a real theme directory underneath it.
        let theme_dir = theme_root(config_dir.path(), "mine");
        std::fs::create_dir_all(&theme_dir).unwrap();

        // A builtin theme needs no directory, so it never has one and is not a
        // misconfiguration worth reporting. That holds for the base theme and
        // for the alternatives beside it alike.
        for builtin in builtin_themes() {
            assert_eq!(
                user_theme_root(Some(config_dir.path()), builtin),
                (None, false),
                "{builtin:?} is builtin but was reported as missing"
            );
        }
        // An overlay that exists is not missing.
        assert_eq!(
            user_theme_root(Some(config_dir.path()), "mine"),
            (Some(theme_dir), false)
        );
        // A named theme with no builtin and no directory disables the overlay
        // and is reported, which is what `Theme::new` turns into the stderr
        // warning.
        assert_eq!(
            user_theme_root(Some(config_dir.path()), "typo"),
            (None, true)
        );
        // No config dir at all cannot be a typo, and must stay quiet.
        assert_eq!(user_theme_root(None, "mine"), (None, false));
    }

    /// The base theme has to exist for the loader's guarantees to mean anything,
    /// and the alternatives have to be reachable by name — both are silent
    /// failures otherwise, since an unknown builtin name just renders the base
    /// theme.
    #[test]
    fn the_base_theme_and_the_shipped_alternatives_are_builtin() {
        let names: Vec<&str> = builtin_themes().collect();
        assert!(
            names.contains(&BASE_THEME),
            "the base theme {BASE_THEME:?} is not in data/themes/"
        );
        assert!(
            names.contains(&"catppuccin-mocha-mauve"),
            "the Catppuccin theme moved out of `default` but isn't builtin; got {names:?}"
        );
    }

    /// A builtin theme's stylesheet *replaces* the base theme's rather than
    /// layering over it, so a builtin theme that ships none would render with no
    /// colours at all rather than an obviously broken one.
    #[test]
    fn every_builtin_theme_ships_a_stylesheet() {
        for name in builtin_themes() {
            assert!(
                builtin_file(name, "style.css").is_some(),
                "{name} has no style.css, and a builtin stylesheet does not inherit"
            );
        }
    }

    /// The whole point of moving the Catppuccin palette out: `default` is now
    /// neutral, and Catppuccin is one alternative among others rather than the
    /// look everyone gets.
    #[test]
    fn the_base_theme_is_neutral_and_the_alternative_is_catppuccin() {
        let base = Theme::with_layers(Some(BASE_THEME), None);
        let base_css = base.resource("style.css").expect("base style.css");
        assert!(
            base_css.contains("#8aa9c8"),
            "base accent is not the slate one"
        );
        for rgb in ["#1e1e2e", "#313244", "#cdd6f4", "#cba6f7"] {
            assert!(
                !base_css.contains(rgb),
                "base theme still carries the Catppuccin value {rgb}"
            );
        }

        let catppuccin = Theme::with_layers(Some("catppuccin-mocha-mauve"), None);
        let css = catppuccin
            .resource("style.css")
            .expect("catppuccin style.css");
        assert!(css.contains("#cba6f7"), "Catppuccin mauve accent is gone");
    }

    /// The accent is read out of the style context by Rust rather than by CSS, so
    /// a rename that missed one stylesheet would ship a theme whose rail colour
    /// silently fell back. Both spellings have to be gone from the builtins.
    #[test]
    fn no_builtin_stylesheet_uses_the_legacy_accent_name() {
        for dir in BUILTIN_THEMES.dirs() {
            let name = theme_name(dir);
            let css = builtin_file(name, "style.css").expect("stylesheet");
            assert!(
                !css.contains("huffi_mauve_color"),
                "{name} still defines the legacy huffi_mauve_color"
            );
            assert!(
                css.contains("huffi_accent_color"),
                "{name} never defines huffi_accent_color"
            );
        }
    }

    /// A builtin theme that ships only a stylesheet still has to get every row
    /// template from the base theme underneath it. If this regresses the theme
    /// renders with real colours and no rows, which is a confusing way to fail.
    #[test]
    fn a_builtin_theme_inherits_row_templates_from_the_base_theme() {
        let theme = Theme::with_layers(Some("catppuccin-mocha-mauve"), None);
        assert!(
            theme.entry_template(None, None).contains("id=\"row\""),
            "no root template"
        );
        // The one layout the base theme ships that is not the stock row, so a
        // fallthrough to `entry.ui` wouldn't pass for it.
        let info = theme.entry_template(Some("calculator"), Some("info"));
        assert!(
            info.contains("id=\"comment\""),
            "the info layout variant did not come from the base theme"
        );
    }

    /// Layer order is base → selected builtin → user, and each layer has to be
    /// able to win over the one below it.
    #[test]
    fn a_user_theme_overrides_the_selected_builtin_theme() {
        let dir = TempTheme::new("override-builtin");
        std::fs::write(dir.path().join("style.css"), "/* user css */").unwrap();
        let theme = Theme::with_layers(Some("catppuccin-mocha-mauve"), Some(dir.owned()));
        assert_eq!(
            theme.resource("style.css").as_deref(),
            Some("/* user css */")
        );
        // The builtin layer underneath is still what supplies the templates.
        assert!(theme.entry_template(None, None).contains("id=\"row\""));
    }

    /// A user theme whose name collides with a builtin one is an *override* of
    /// it, not a replacement: it still gets the base theme's row templates for
    /// the positions it says nothing about.
    #[test]
    fn a_user_theme_named_after_a_builtin_still_inherits_from_the_base() {
        let dir = TempTheme::new("shadow-builtin");
        std::fs::write(dir.path().join("style.css"), "/* only css */").unwrap();
        let theme = Theme::with_layers(None, Some(dir.owned()));
        assert_eq!(
            theme.resource("style.css").as_deref(),
            Some("/* only css */")
        );
        assert!(
            theme
                .entry_template(Some("calculator"), Some("info"))
                .contains("id=\"comment\"")
        );
    }

    /// An unknown name resolves to the base theme rather than to nothing: a
    /// stylesheet that isn't applying is otherwise indistinguishable from a
    /// misspelling, which is why `Theme::new` warns on stderr.
    #[test]
    fn an_unknown_theme_name_renders_the_base_theme() {
        let theme = Theme::with_layers(Some("no-such-theme"), None);
        assert_eq!(
            theme.resource("style.css").as_deref(),
            builtin_file(BASE_THEME, "style.css").as_deref()
        );
    }

    #[test]
    fn builtin_base_resolves_when_user_file_absent() {
        let dir = TempTheme::new("missing");
        let theme = Theme::with_root(Some(dir.owned()));
        let css = theme.resource("style.css").expect("builtin style.css");
        assert!(css.contains("@define-color"));
        let template = theme.entry_template(None, None);
        assert!(template.contains("id=\"row\""));
    }

    #[test]
    fn user_file_wins_over_builtin() {
        let dir = TempTheme::new("override");
        std::fs::write(dir.path().join("style.css"), "/* user css */").unwrap();
        let theme = Theme::with_root(Some(dir.owned()));
        assert_eq!(
            theme.resource("style.css").as_deref(),
            Some("/* user css */")
        );
    }

    #[test]
    fn missing_user_resource_falls_back_to_the_base_theme() {
        let dir = TempTheme::new("fallback");
        std::fs::write(dir.path().join("style.css"), "/* user css */").unwrap();
        let theme = Theme::with_root(Some(dir.owned()));
        let template = theme.entry_template(Some("desktop"), None);
        assert!(
            template.contains("id=\"row\""),
            "a provider without its own entry.ui must use the base template"
        );
    }

    /// The templates shipped in the builtin themes are the only ones the project
    /// controls, so they're the ones worth holding to an exact id vocabulary.
    ///
    /// An id the renderer doesn't know is inert: a misspelled `title` or
    /// `detail-qantity` leaves a widget that never gets filled, and a row that
    /// renders half-empty with nothing in the log. Structural containers the
    /// templates add for themselves are allowed, but only the ones listed in
    /// [`AUTHOR_IDS`] — so introducing a new one is a deliberate edit here
    /// rather than a stray attribute nobody reviews.
    ///
    /// This is a text check, not a real parse: instantiating a template means a
    /// `GtkBuilder` off the main thread, which gtk4 forbids. XML
    /// well-formedness is still only checked by GTK at runtime.
    #[test]
    fn shipped_templates_only_use_known_widget_ids() {
        use crate::ui::app::KNOWN_WIDGET_IDS;
        use huffi::engine::provider::is_detail_key;

        for (path, xml) in shipped_templates() {
            for id in widget_ids(&xml) {
                if KNOWN_WIDGET_IDS.contains(&id)
                    || AUTHOR_IDS.contains(&id)
                    || id.strip_prefix("detail-").is_some_and(is_detail_key)
                {
                    continue;
                }
                panic!("{path} declares id {id:?}, which nothing binds");
            }
        }
    }

    /// `row` is load-bearing: without it the renderer falls back to a bare box,
    /// so a template that lost it degrades every row it serves in silence.
    /// `clickable` is the row's click target, so without it rows stop
    /// responding to clicks.
    #[test]
    fn shipped_templates_declare_the_ids_they_rely_on() {
        for (path, xml) in shipped_templates() {
            let ids = widget_ids(&xml);
            for required in ["row", "clickable"] {
                assert!(
                    ids.contains(&required),
                    "{path} declares no `{required}` object"
                );
            }
        }
    }

    /// The calculator's detail keys and the shipped calculator templates must
    /// agree in both directions: a `detail-<key>` widget the calculator never
    /// emits is dead markup, and a key it does emit that no template declares is
    /// a field nobody can see. Both are silent, so both are worth catching.
    #[test]
    fn shipped_calculator_templates_and_detail_keys_agree() {
        use huffi::engine::provider::builtin::calculator::DETAIL_KEYS;
        use huffi::engine::provider::is_detail_key;

        let mut declared: Vec<String> = Vec::new();
        for (path, xml) in shipped_templates() {
            if !path.contains("providers/calculator") {
                continue;
            }
            for id in widget_ids(&xml) {
                if let Some(key) = id.strip_prefix("detail-") {
                    // A key that can't be a widget id is silently dropped by the
                    // renderer, so one drifting out of the character set would
                    // make the field unreachable rather than fail loudly.
                    assert!(
                        is_detail_key(key),
                        "{path} declares detail-{key}, which is not a usable widget id"
                    );
                    assert!(
                        DETAIL_KEYS.contains(&key),
                        "{path} declares detail-{key}, which the calculator never emits"
                    );
                    declared.push(key.to_owned());
                }
            }
        }
        assert!(!declared.is_empty(), "no calculator detail widgets found");
        for key in DETAIL_KEYS {
            assert!(
                declared.iter().any(|d| d == key),
                "calculator emits detail-{key} but no shipped template declares it"
            );
        }
    }

    #[test]
    fn provider_template_wins_when_present() {
        let dir = TempTheme::new("provider");
        let provider_dir = dir.path().join("providers").join("calculator");
        std::fs::create_dir_all(&provider_dir).unwrap();
        std::fs::write(
            provider_dir.join("entry.ui"),
            "<interface><object class=\"GtkBox\" id=\"row\"/></interface>",
        )
        .unwrap();
        let theme = Theme::with_root(Some(dir.owned()));

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
    /// resolution chain can be asserted end to end.
    fn variant_theme(tag: &str) -> TempTheme {
        let dir = TempTheme::new(tag);
        for (rel, body) in [
            ("entry.ui", "global"),
            ("providers/calculator/entry.ui", "provider"),
            ("providers/calculator/date/entry.ui", "variant"),
        ] {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("<interface>{body}</interface>")).unwrap();
        }
        dir
    }

    #[test]
    fn variant_template_wins_over_provider_template() {
        let dir = variant_theme("variant-wins");
        let theme = Theme::with_root(Some(dir.owned()));
        let date = theme.entry_template(Some("calculator"), Some("date"));
        assert!(date.contains("variant"), "{date}");
    }

    /// A theme with a `variants/<name>/entry.ui` that no provider claims. The
    /// "prov" provider has no template of its own, so it should fall through
    /// the provider level to the shared variant.
    fn shared_variant_theme(tag: &str) -> TempTheme {
        let dir = TempTheme::new(tag);
        for (rel, body) in [
            ("entry.ui", "global"),
            ("providers/calc/entry.ui", "provider"),
            ("providers/calc/priv/entry.ui", "priv-variant"),
            ("variants/shared/entry.ui", "shared-variant"),
        ] {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("<interface>{body}</interface>")).unwrap();
        }
        dir
    }

    #[test]
    fn shared_variant_applies_to_a_provider_with_no_template_of_its_own() {
        let dir = shared_variant_theme("shared-applies");
        let theme = Theme::with_root(Some(dir.owned()));
        let row = theme.entry_template(Some("prov"), Some("shared"));
        assert!(row.contains("shared-variant"), "{row}");
    }

    #[test]
    fn shared_variant_applies_when_there_is_no_provider_at_all() {
        let dir = shared_variant_theme("shared-no-provider");
        let theme = Theme::with_root(Some(dir.owned()));
        let row = theme.entry_template(None, Some("shared"));
        assert!(row.contains("shared-variant"), "{row}");
    }

    #[test]
    fn provider_variant_wins_over_shared_variant() {
        let dir = shared_variant_theme("shared-vs-provider");
        let theme = Theme::with_root(Some(dir.owned()));
        let row = theme.entry_template(Some("calc"), Some("priv"));
        assert!(row.contains("priv-variant"), "{row}");
    }

    /// The point of putting the shared position *after* the provider template:
    /// a theme that customises one provider keeps winning for it, rather than
    /// having its layout replaced by the shared variant file.
    #[test]
    fn provider_template_wins_over_shared_variant_for_the_same_variant_name() {
        let dir = TempTheme::new("shared-after-provider");
        for (rel, body) in [
            ("entry.ui", "global"),
            ("providers/calc/entry.ui", "provider"),
            ("variants/shared/entry.ui", "shared-variant"),
        ] {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("<interface>{body}</interface>")).unwrap();
        }
        let theme = Theme::with_root(Some(dir.owned()));
        let row = theme.entry_template(Some("calc"), Some("shared"));
        assert!(row.contains("provider"), "{row}");
    }

    #[test]
    fn unknown_variant_falls_back_to_provider_template() {
        let dir = variant_theme("variant-unknown");
        let theme = Theme::with_root(Some(dir.owned()));
        let number = theme.entry_template(Some("calculator"), Some("number"));
        assert!(number.contains("provider"), "{number}");
    }

    #[test]
    fn unknown_provider_and_variant_fall_back_to_global_template() {
        let dir = variant_theme("variant-no-provider");
        let theme = Theme::with_root(Some(dir.owned()));
        let other = theme.entry_template(Some("desktop"), Some("date"));
        assert!(other.contains("global"), "{other}");
    }

    #[test]
    fn variant_without_provider_id_uses_global_template() {
        let dir = variant_theme("variant-unscoped");
        let theme = Theme::with_root(Some(dir.owned()));
        let unowned = theme.entry_template(None, Some("date"));
        assert!(unowned.contains("global"), "{unowned}");
    }

    #[test]
    fn each_variant_is_cached_separately() {
        let dir = variant_theme("variant-cache");
        let theme = Theme::with_root(Some(dir.owned()));
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
        let dir = TempTheme::new("user-only");
        std::fs::write(dir.path().join("style.css"), "/* u */").unwrap();
        let theme = Theme::with_root(Some(dir.owned()));
        assert_eq!(theme.user_resource("style.css").as_deref(), Some("/* u */"));

        let none = Theme::with_root(None);
        assert_eq!(none.user_resource("style.css"), None);
    }

    #[test]
    fn entry_template_is_cached() {
        let dir = TempTheme::new("cache");
        let theme = Theme::with_root(Some(dir.owned()));
        let a = theme.entry_template(Some("desktop"), None);
        // Pointer equality, not just equal contents: the second call must hand
        // back the very same allocation rather than re-resolving the theme.
        let b = theme.entry_template(Some("desktop"), None);
        assert!(Rc::ptr_eq(&a, &b), "template was reallocated, not cached");
    }

    #[test]
    fn default_template_is_cached_separately_from_provider_templates() {
        let dir = TempTheme::new("cache-keys");
        let theme = Theme::with_root(Some(dir.owned()));
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
