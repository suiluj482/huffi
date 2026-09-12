//! Hot-path benchmark: the cost of a cold keystroke query through
//! `Engine::query`, against a synthetic desktop-style corpus.
//!
//! Queries are cycled so the engine's single-slot cache does not serve the
//! benchmark (each keystroke in real use changes the query); the cycle
//! mimics typed prefixes and also exercises the empty query.

use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use huffi::engine::Engine;
use huffi::engine::provider::{Entry, TestProvider, entry};
use huffi::engine::scoring::MatchField;

fn corpus(n: usize) -> Vec<Entry> {
    const PREFIXES: &[&str] = &[
        "firefox", "files", "calculator", "terminal", "code", "gimp",
        "nautilus", "ranger", "fish", "kitty", "foot", "wezterm", "alacritty",
        "obsidian", "zed", "neovim", "emacs", "libreoffice", "spotify",
        "discord", "signal", "nextcloud", "keepass", "vlc", "mpv", "gallery",
        "kdenlive", "blender", "inkscape", "gimp", "audacity", "pipewire",
        "pavucontrol", "hyprland", "waybar", "sway", "rofi", "wofi",
        "dunst", "mako", "vsftpd", "syncthing", "qbittorrent", "fuzzel",
    ];
    const WORDS: &[&str] = &[
        "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
        "hotel", "india", "juliett", "kilo", "lima", "mike", "november",
        "oscar", "papa", "quebec", "romeo", "sierra", "tango", "uniform",
        "victor", "whiskey", "xray", "yankee", "zulu",
    ];
    const COMMENTS: &[&str] = &[
        "Browse the world wide web",
        "Access and organize files",
        "Evaluate arithmetic expressions",
        "Open the command line shell",
        "Edit source code with syntax highlighting",
        "Render and export vector graphics",
        "Play audio and video streams",
        "Sync files between devices",
        "Manage window state at the compositor level",
        "Record audio input and output",
    ];
    const ACCENTED: &[&str] = &[
        "Éditeur de texte",
        "Gestionnaire de fichiers",
        "Navigateur web",
        "Terminal rapide",
        "Café e-commerce client",
        "Moteur de recherche",
    ];

    (0..n)
        .map(|i| {
            let prefix = PREFIXES[i % PREFIXES.len()];
            let word = WORDS[(i * 7 % 31) % WORDS.len()];
            let name = format!("{prefix}-{word}-{i}");
            let comment = COMMENTS[(i * 5 % 17) % COMMENTS.len()];
            let mut fields = vec![
                MatchField {
                    text: name.clone(),
                    weight: 1.0,
                },
                MatchField {
                    text: comment.to_string(),
                    weight: 0.5,
                },
            ];
            if i % 3 == 0 {
                fields.push(MatchField {
                    text: format!("{word}-resources-{i}"),
                    weight: 0.7,
                });
            }
            if i % 7 == 0 {
                fields.push(MatchField {
                    text: ACCENTED[i % ACCENTED.len()].to_string(),
                    weight: 0.8,
                });
            }

            entry(&name, &name)
                .comment(comment)
                .history_key(&name)
                .icon(prefix)
                .match_fields(fields)
        })
        .collect()
}

fn bench_query(c: &mut Criterion) {
    let dir = PathBuf::from(format!("/tmp/huffi-bench-{}", std::process::id()));
    let queries = ["", "f", "fi", "fire", "gl", "calc", "ter"];

    for n in [500_usize, 1000] {
        let mut engine = Engine::new(&dir, true).expect("engine");
        engine
            .add_provider(Box::new(TestProvider::new("desktop", corpus(n))))
            .expect("provider");

        c.bench_function(&format!("query_cold_{n}"), |b| {
            b.iter(|| {
                for q in &queries {
                    std::hint::black_box(engine.query(std::hint::black_box(q)));
                }
            });
        });
    }
}

criterion_group!(benches, bench_query);
criterion_main!(benches);