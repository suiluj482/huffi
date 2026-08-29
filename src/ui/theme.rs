use std::path::PathBuf;

use gtk4::prelude::StyleContextExt;
use gtk4::{self, gdk};

pub const ICON_SIZE: i32 = 24;

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
    (0xcb as f64 / 255.0, 0xa6 as f64 / 255.0, 0xf7 as f64 / 255.0)
}

const DEFAULT_CSS: &str = include_str!("../../data/style.css");

pub fn load_css(display: &gdk::Display) {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(DEFAULT_CSS);
    gtk4::style_context_add_provider_for_display(
        display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    if let Some(config_dir) = config_dir() {
        let user_css = config_dir.join("huffi").join("style.css");
        if user_css.exists() {
            let user_provider = gtk4::CssProvider::new();
            user_provider.load_from_path(&user_css);
            gtk4::style_context_add_provider_for_display(
                display,
                &user_provider,
                gtk4::STYLE_PROVIDER_PRIORITY_USER,
            );
        }
    }
}

/// The user config directory (`$XDG_CONFIG_HOME`, defaulting to
/// `$HOME/.config`), following the same convention `dirs::config_dir()` used
/// to provide.
fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
}
