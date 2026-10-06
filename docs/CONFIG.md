# Configuration

Huffi reads a TOML config file from `$XDG_CONFIG_HOME/huffi/config.toml`
(default `~/.config/huffi/config.toml`), or from any path given with
`--config PATH`. A missing file is fine — every option falls back to a
default. A malformed file is an error so typos don't silently reset your
settings.

Precedence is **command line > config file > default**: `--socket`,
`--data`, and `--config` override the file, which overrides the built-ins.

```
huffi                          # reads ~/.config/huffi/config.toml if present
huffi show --config /etc/huffi.toml
huffi show --socket /custom.sock    # flag wins over [paths].socket
```

## Reference

Sections mirror huffi's module structure: `paths` and `ui` are core, while
everything the engine reads lives under `[engine.*]` (scoring, providers,
external binaries).

```toml
[paths]
# Huffi data folder. History lives at <data_dir>/history.json and each
# provider gets its own <data_dir>/providers/<provider id>/ folder.
# Default: $XDG_DATA_HOME/huffi (falls back to ~/.local/share/huffi).
# data_dir = "/home/me/.local/share/huffi"

# Control socket. Default: $XDG_RUNTIME_DIR/huffi.sock (falls back to /tmp).
# socket = "/run/user/1000/huffi.sock"

[ui]
# Overlay panel dimensions in pixels.
width     = 600
height    = 400
# Results shown per page.
page_size = 10
# Entry icon size in pixels.
icon_size = 24
# Theme name. Four ship in the binary: "default" (neutral greys),
# "catppuccin-mocha-mauve", "nord" and "tokyo-night-storm" — those three take their
# colours from the palettes of the same name; the layout is huffi's. A theme of your
# own lives in $XDG_CONFIG_HOME/huffi/themes/<name>/ and layers over whichever builtin
# theme you name, file by file.
# theme = "default"

[engine.scoring]
# Weight of a manual boost relative to a normal launch.
boost_weight     = 10.0
# Synthetic launch samples a boost counts as toward confidence. Boosts push
# ranking hard (boost_weight) without counting as that many normal launches
# of evidence.
boost_samples    = 5
# History half-life in days (exponential decay).
half_life_days   = 14.0
# Confidence smoothing constant k in n / (n + k). How much evidence a
# prefix needs before history is trusted over text score.
confidence_k     = 3.0
# Base score for results shown while the query is empty.
empty_query_score = 0.8

[engine.provider]
# Prefixes that own a query outright. When the resolved global prefix of a
# query is one of these, only providers that declare that prefix are queried —
# every other provider is skipped for that keystroke, including providers with
# no prefixes of their own (the desktop entries behind an unprefixed query).
# Exclusivity is judged against the *resolved* prefix (the longest one the
# input starts with), so listing "=" leaves `==` queries shared. Empty (the
# default) means every enabled provider shares every query.
# exclusive_prefixes = ["=", "!", ":"]

[engine.provider.builtin.desktop]
# Display name shown in the UI (defaults to the provider id).
# name    = "Applications"
# Override whether the provider participates in queries.
# enabled = true
# Override trigger prefixes (empty = always active).
# prefixes = []
# Only call the provider when a query matches one of its prefixes.
# prefix_only = false

[engine.provider.builtin.desktop.extra]
# Fuzzy-match field weights for the desktop-entry provider.
weight_name         = 1.0
weight_keyword      = 0.8
weight_generic_name = 0.7
weight_comment      = 0.5

[engine.provider.builtin.nix]
# Trigger prefix: `!firefox` runs `nix run nixpkgs#firefox`.
# prefixes = ["!"]
# Only call the provider when a query matches one of its prefixes.
# prefix_only = true

[engine.provider.builtin.nix.extra]
# Fuzzy-match field weights for the nix run provider. The package description
# is always shown as the subtitle; it is *not* fuzzy-matched against unless
# weight_desc is set above 0 (matching the description costs scoring time).
weight_attr         = 1.0
weight_pname        = 0.9
weight_desc         = 0.0
# Regenerate the cached nixpkgs index once it is this old (seconds).
# Default: 7 days.
cache_max_age_secs  = 604800

[engine.provider.builtin.runner]
# Trigger prefix: `>cargo test` runs `cargo test` in the terminal.
# prefixes = [">"]
# Only call the provider when a query matches one of its prefixes.
# prefix_only = true

[engine.provider.builtin.runner.extra]
# Shell history files to suggest from, in order (later files win when the
# same command appears in several). Default: $HISTFILE, ~/.bash_history and
# ~/.zsh_history, whichever exist.
# history_files = ["/home/user/.zsh_history"]
# Keep only the newest N distinct history commands; 0 = unlimited. Applied
# once at startup, never per query. Default: 5000.
max_history_lines  = 5000

[engine.provider.builtin.unicode]
# Trigger prefix: `:smile` copies 😄
# prefixes = [":"]
# Only call the provider when a query matches one of its prefixes.
# prefix_only = true
# Set `enabled = false` to skip the provider entirely

[engine.provider.builtin.unicode.extra]
# Fuzzy-match field weights for the Unicode provider: one field for the name
# (the CLDR name of an emoji, the Unicode name of any other character), one
# per shortcode, and one for the code point's hex digits.
weight_name         = 1.0
weight_shortcode    = 1.3
weight_codepoint    = 0.6

[engine.external]
# For `Terminal=true` desktop entries: the argv items to prepend to the
# entry's command — the terminal binary plus whatever flags it expects before
# a command. Terminals differ here: kitty/gnome-terminal use ["kitty", "--"],
# alacritty/xterm use ["alacritty", "-e"], foot takes none. The final argv
# is `terminal… <entry command>…`.
terminal = ["kitty", "--"]
# Same, for entries that should keep the terminal open after the command
# exits (the runner provider's `>` commands). Set this whenever you change
# `terminal`: kitty/konsole stay open with ["kitty", "--hold", "--"], and a
# terminal without a hold flag can be pointed at a wrapper script.
terminal_hold = ["kitty", "--hold", "--"]
# Clipboard tool used by the calculator and meta providers.
clipboard = "wl-copy"
# Working directory for spawned actions when the entry has none of its own
# (a desktop file's `Path=`, or a provider's `.cwd()`). Unset by default,
# which means the user's home directory; a leading `~/` expands.
# working_dir = "~/src"
```

## Nix (Home Manager)

The Home Manager module can write the config file for you. Both a raw file
and declarative settings are supported:

```nix
{ inputs, ... }:
{
  imports = [ inputs.huffi.homeManagerModules.huffi ];

  programs.huffi = {
    enable = true;
    settings = {
      ui.width = 700;
      ui.page_size = 15;
      paths.data_dir = "/home/me/.local/share/huffi";
      engine.scoring.boost_weight = 4.0;
      engine.provider.builtin.desktop.extra.weight_comment = 0.9;
      engine.provider.builtin.nix.extra.weight_desc = 0.6;
      engine.external.terminal = [ "foot", "--" ];
      engine.external.terminal_hold = [ "foot", "--hold", "--" ];
    };
    # …or point at a checked-in file instead:
    # configFile = ./huffi.toml;
  };
}
```

The file is installed to `~/.config/huffi/config.toml`.

## Other configuration

- **Themes** — a theme is a directory of one stylesheet and GTK Builder row
  templates, layered over `default` file by file, and selected with
  `[ui] theme = "<name>"` (default: `default`). Four ship in the binary:
  `default`, `catppuccin-mocha-mauve`, `nord` and `tokyo-night-storm`. Your own
  live in `$XDG_CONFIG_HOME/huffi/themes/<name>/`; the reference for their
  contents, the row-template resolution order, the widget ids a template may
  declare, and the classes rows carry is **[`THEMING.md`](THEMING.md)**.
- The `--data` and `--socket` flags still override the corresponding
  `[paths]` entries per invocation; the config file only supplies the
  defaults the flags would otherwise use.
