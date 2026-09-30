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
# Theme name. Themes live in $XDG_CONFIG_HOME/huffi/themes/<name>/ and
# overlay the embedded default theme (data/themes/default/) file by file.
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
      engine.provider.builtin.desktop.extra.weight_comment = 0.9;
      engine.provider.builtin.nix.extra.weight_desc = 0.6;
      engine.external.terminal = [ "foot", "--" ];
    };
    # …or point at a checked-in file instead:
    # configFile = ./huffi.toml;
  };
}
```

The file is installed to `~/.config/huffi/config.toml`.

## Other configuration

- **Themes** — a theme is a directory of stylesheets and per-provider row
  templates. The default theme ships embedded at `data/themes/default/`:

  ```text
  data/themes/default/
    style.css                 # global stylesheet
    entry.ui                  # default GTK Builder row template
    providers/<id>/
      style.css               # optional, scoped to that provider's rows
      entry.ui                # optional, custom row layout for that provider
      <variant>/
        entry.ui              # optional, row layout for one layout variant
  ```

  To customize, copy `data/themes/default` to
  `~/.config/huffi/themes/<name>/`, keep only the files you want to override
  (you can start from nothing), and select it with `[ui] theme = "<name>"`
  (default: the embedded `default` theme). A file present in your theme
  replaces the matching file in the embedded default; the embedded CSS is
  still loaded below yours, so you only need to restate the rules you change.

  Every entry row carries a `provider-<id>` CSS class (e.g.
  `.provider-desktop`, `.provider-calculator`), so providers can be styled
  without an XML template. When the entry also names a layout variant, the row
  gets a `provider-<id>-<variant>` class as well:

  ```css
  /* ~/.config/huffi/themes/minimal/style.css */
  .provider-calculator .title { color: @accent_color; }
  .provider-calculator .row { background: transparent; }
  .provider-calculator-date .title { font-size: 15px; }
  ```

  A provider can also ship its own `entry.ui` GTK Builder template with a
  completely different widget layout — see below for the widget ids the row
  renderer binds to. An entry may additionally select a *variant*, which picks
  a more specific template; resolution falls through at each level, so a user
  file at one level does not have to restate the ones below it:

  ```text
  providers/<id>/<variant>/entry.ui  →  providers/<id>/entry.ui  →  entry.ui
  ```

  The bundled `default` theme uses both: the calculator sets a variant per kind
  of result, so `providers/calculator/entry.ui` renders ordinary numbers and
  `providers/calculator/date/entry.ui` renders dates with their humanized
  relative time.

  - **Row template widget ids** (`entry.ui`) — the renderer binds the entry
    fields onto these named widgets; any of them can be skipped:

    | Widget id      | Type       | Bound to                                  |
    |----------------|------------|-------------------------------------------|
    | `row`          | `GtkBox`   | the row container (required)              |
    | `clickable`    | `GtkBox`   | click-to-select target                    |
    | `icon`         | `GtkImage` | entry icon (pixel size from `icon_size`)  |
    | `title-area`   | `GtkBox`   | title (+ subtitle) area                   |
    | `title`        | `GtkLabel` | entry title                               |
    | `subtitle`     | `GtkLabel` | entry subtitle (shown only if set)        |
    | `scores`       | `GtkBox`   | score labels container                    |
    | `score-base`   | `GtkLabel` | base (fuzzy) score                        |
    | `score-history`| `GtkLabel` | history score (shown only if present)     |
    | `boost`        | `GtkButton`| "+" history button (only with history key)|
    | `delete`       | `GtkButton`| "−" history button (only with history key)|

    Unnamed widgets are left alone, so a template can add decorations freely.

  - **Named details** — an entry can carry arbitrary key/value display fields,
    and a template opts in to the ones it wants by declaring a
    `detail-<key>` label:

    ```xml
    <object class="GtkLabel" id="detail-quantity">
      <property name="visible">false</property>
    </object>
    ```

    Keys are chosen by the provider and must match `[a-z0-9-]+` (they become
    part of a GTK object id). The renderer fills in each declared widget that
    the entry actually carries and gives it the `detail` CSS class; a detail
    with no matching widget is not shown, and a widget with no matching detail
    stays hidden — so a template shows exactly the fields it declares, and
    there is no catch-all "render everything" widget. Selection styling is
    plain CSS, so any template can restyle details without renderer changes:

    ```css
    .detail { font-size: 11px; color: @huffi_subtext0_color; }
    .row-selected .detail { color: @huffi_mauve_color; }
    ```

    The calculator is the worked example. It evaluates expressions with
    [`rink-core`] and derives both a layout variant and a set of named details
    from the kind of result it got back, so this:

    ```text
    =1/3 m/s  →  variant number,  details quantity, exact, dimensions
    =now      →  variant date,    details human, absolute
    =water    →  variant substance, details doc, properties
    =lightyear → variant def,     details def, value, doc
    ```

    resolves to a different template per row (`entry.ui`,
    `date/entry.ui`, and the provider-level fallback for the rest), and each row
    renders only the details its result actually has. The full key set is
    `calculator::DETAIL_KEYS` in
    [`src/engine/provider/builtin/calculator.rs`][calculator], which is what the
    shipped templates are tested against — if you write your own template for
    the calculator, declare ids from that list and your theme will never show a
    field that doesn't exist or hide one you asked for.

    - **Legacy styling** — `$XDG_CONFIG_HOME/huffi/style.css` is still loaded
      with user priority on top of the active theme; the accent color for the
      scroll rail is read from its `huffi_mauve_color` `@define-color`.
  - The `--data` and `--socket` flags still override the corresponding
    `[paths]` entries per invocation; the config file only supplies the
    defaults the flags would otherwise use.

[`rink-core`]: https://github.com/tiffany352/rink-rs
[calculator]: ../src/engine/provider/builtin/calculator.rs