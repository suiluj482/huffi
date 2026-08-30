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

[engine.provider.desktop]
# Fuzzy-match field weights for the desktop-entry provider.
weight_name         = 1.0
weight_keyword      = 0.8
weight_generic_name = 0.7
weight_comment      = 0.5

[engine.external]
# For `Terminal=true` desktop entries: the argv items to prepend to the
# entry's command — the terminal binary plus whatever flags it expects before
# a command. Terminals differ here: kitty/gnome-terminal use ["kitty", "--"],
# alacritty/xterm use ["alacritty", "-e"], foot takes none. The final argv
# is `terminal… <entry command>…`.
terminal = ["kitty", "--"]
# Clipboard tool used by the calculator and meta providers.
clipboard = "wl-copy"
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
      engine.provider.desktop.weight_comment = 0.9;
      engine.external.terminal = [ "foot", "--" ];
    };
    # …or point at a checked-in file instead:
    # configFile = ./huffi.toml;
  };
}
```

The file is installed to `~/.config/huffi/config.toml`.

## Other configuration

- **Styling** — `$XDG_CONFIG_HOME/huffi/style.css` is loaded with user
  priority on top of the default stylesheet (`data/style.css`); the accent
  color for the scroll rail is read from its `huffi_mauve_color`
  `@define-color`.
- The `--data` and `--socket` flags still override the corresponding
  `[paths]` entries per invocation; the config file only supplies the
  defaults the flags would otherwise use.