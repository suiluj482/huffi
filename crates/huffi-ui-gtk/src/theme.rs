use gtk4::{self, gdk};

pub const ICON_SIZE: i32 = 24;

pub const MAUVE: &str = "#cba6f7";

pub fn rgb(hex: &str) -> (f64, f64, f64) {
    let hex = hex.trim_start_matches('#');
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    (channel(0), channel(2), channel(4))
}

const DEFAULT_CSS: &str = include_str!("../data/style.css");

pub fn load_css(display: &gdk::Display) {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(DEFAULT_CSS);
    gtk4::style_context_add_provider_for_display(
        display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    if let Some(config_dir) = dirs::config_dir() {
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
