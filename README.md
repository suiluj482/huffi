# huffi

[![License: GPL-3.0-only](https://img.shields.io/badge/license-GPL--3.0--only-blue)](/LICENSE)

A launcher that learns *what you meant*, not just *what you use*.

Most launchers rank results by global frequency or recency. This gets
inefficient when applications share a prefix. For example, you always launch
Firefox by typing `f`. To launch your file manager with a frequency-only
launcher, you will need to type `fil` until Firefox no longer matches.

Huffi's core idea: **the more precisely you type, the more you're trying to
escape the default.** It understands that you want something else when typing
more than usual — launching Firefox with `f` and your file manager with `fi`.

---

## Quick start

### From Nix

```console
nix run github:suiluj482/huffi
```

Or add `huffi` to your flake inputs and use the Home Manager module:

```nix
inputs = {
  ...
  
  huffi = {
    url = "/mnt/storage/JS/documents/projects/software/huffi";
    inputs.nixpkgs.follows = "nixpkgs";
    inputs.home-manager.follows = "home-manager";
  };
};

# in system config
nixpkgs.overlays = [ inputs.huffi.overlays.default ];

# in home manager
imports = [ inputs.huffi.homeManagerModules.huffi ];

programs.huffi = {
  enable = true;  # installs huffi and preloads it via systemd --user
  settings = {    # optionally write ~/.config/huffi/config.toml
    ui.width = 700;
    engine.scoring.boost_weight = 4.0;
  };
  # …or configFile = ./huffi.toml; for a checked-in config instead.
};
```

### From source

```console
git clone https://github.com/suiluj482/huffi
cd huffi
nix develop               # provides GTK4 and the Rust toolchain
cargo build --release
# binary in target/release/: huffi
```

---

## Usage

`huffi` is a GTK4 overlay that appears with keyboard focus.
Start typing to fuzzy search, use arrow keys to navigate, press tab to use the
selected entry's query suggestion (if defined), and press enter to launch.

The first instance stays **warm**: dismissing hides the window instead of
exiting. Every other invocation talks to it over a control socket
(`$XDG_RUNTIME_DIR/huffi.sock`) and returns immediately — so every launch
after the first has no startup cost. With no subcommand, `huffi` behaves like
`show`.

```console
huffi                          # show: wake the resident, or start one
huffi preload                  # ensure a hidden resident is running (for systemd)
huffi show -q fire             # wake the resident and show query "fire"
huffi hide                     # hide the window if it is visible
huffi toggle -q "= 2 + 2"      # show if hidden, hide if visible
huffi quit                     # quit the resident instance
huffi status                   # is it running? is the window visible?
huffi show --reload -q fire    # restart the resident and take over its socket
huffi show --socket /custom.sock   # talk to an instance on a different socket
```

| Key | Action |
|---|---|
| Type | Fuzzy search |
| `↓` / `↑` | Select next / previous |
| `Tab` | Apply the selected entry's query suggestion (if defined) |
| `Enter` | Launch selection |
| `Esc` | Dismiss |
| Mouse wheel | Scroll results |
| `+` / `−` button | Boost / delete association |

### Subcommands

| Subcommand | Action |
|---|---|
| `show` | Ensure a resident is running, then show the window (the default) |
| `preload` | Start a hidden resident if none is running; exit 0 if one already is |
| `hide` | Hide the window if it is visible |
| `toggle` | Show the window if hidden, hide it if visible |
| `quit` | Quit the resident instance |
| `status` | Print `running` / `visible` state (exit 1 if not running) |

### Options

| Flag | Scope | Meaning |
|---|---|---|
| `-q, --query <QUERY>` | `show`, `toggle` | Query to show on wake |
| `--data <PATH>` | `show`, `preload` | Override the data folder (default `$XDG_DATA_HOME/huffi`) |
| `--dry-run` | `show`, `preload` | Don't record history or execute actions; adds in-memory test providers |
| `--reload` | `show`, `preload` | Quit any running instance, unlink its socket, and take it over |
| `--socket <PATH>` | all | Override the control socket path |
| `--config <PATH>` | all | Override the config file path |

### Environment variables

A few options can instead be supplied through the environment, useful for a
systemd service or shell profile. A flag on the command line always wins over
an environment variable, which wins over the config file.

| Variable | Equivalent flag | Meaning |
|---|---|---|
| `HUFFI_SOCKET` | `--socket` | Override the control socket path |
| `HUFFI_CONFIG` | `--config` | Override the config file path |
| `HUFFI_DATA` | `--data` | Override the data folder |
| `HUFFI_DRY_RUN` | `--dry-run` | Don't record history or execute actions |

### Config file

Options live in `$XDG_CONFIG_HOME/huffi/config.toml` (default
`~/.config/huffi/config.toml`), overridable per invocation with flags.
Precedence: **flags > config file > defaults**. See
**[docs/CONFIG.md](docs/CONFIG.md)** for the full reference. Example:

```toml
# ~/.config/huffi/config.toml
[ui]
width     = 700
page_size = 15

[engine.scoring]
boost_weight = 4.0        # less aggressive + / − boosts
half_life_days = 7        # faster decay

[engine.provider.desktop]
weight_comment = 0.9      # comments match a bit harder
```

---

## Features

- **Adaptive ranking** — fuzzy text match blended with time-decayed usage
  history. The more you type, the more the model defers to exact-prefix
  memory rather than top-level defaults. See [docs/ALGORITHM.md](docs/ALGORITHM.md) for
  the full design.
- **Calculator** — type `=` followed by an expression to evaluate via
  [`rink-core`](https://github.com/tiffany352/rink-rs). Results are copied
  to the clipboard with `wl-copy`, and `Tab` uses its `= <result>` query
  suggestion to chain calculations.
- **Desktop entries** — parses `.desktop` files via
  [`freedesktop-desktop-entry`](https://crates.io/crates/freedesktop-desktop-entry),
  matching against name, keywords, generic name, and comment with field
  weighting.
- **Boost / Delete** — correct the model in the moment. Both are scoped to the
  exact prefix you typed and are available on result rows with a history key:
  boost is a synthetic 10x launch, delete clears the prefix's association.
- **Prefix resolution** — each query is preprocessed once to resolve the
  *global prefix*: the longest declared provider prefix that the input
  starts with. It is returned with every query result so the UI can mark it
  (badge next to the input), and providers whose prefixes don't match are
  still queried with the full input.
- **Warm resident** — the process stays resident and hidden between uses;
  every invocation is an instant wake over the control socket (`huffi` or
  `huffi show`). `huffi preload` starts it hidden so a `systemd --user`
  service can keep it warm on login, `huffi quit` stops it, and `huffi
  status` reports it.
- **Icon support** — themed freedesktop icon names or explicit PNG/SVG
  paths, resolved and rendered by the UI against the active icon theme.
- **Persistence** — history stored in `$XDG_DATA_HOME/huffi/history.json`
  (default; the data folder is configurable) with atomic writes
  (`.json.tmp` + rename). Each provider keeps its state in
  `<data_dir>/providers/<provider id>/`. Two-week half-life exponential decay. No
  background jobs, no unbounded logs.
- **Config file** — TOML at `$XDG_CONFIG_HOME/huffi/config.toml` controlling
  paths and UI size plus `[engine.*]` tables for scoring constants, provider
  settings, and external binaries; flags always override it. See
  [docs/CONFIG.md](docs/CONFIG.md).
- **Nix flake** — reproducible builds for `x86_64-linux` and
  `aarch64-linux`, dev shell with all Wayland/GTK dependencies, and a Home
  Manager module that installs `huffi` and keeps it warm on login.

---

## Architecture

The project is a Rust crate that compiles to the binary `huffi`. The scoring
engine lives in the library (`src/engine/`) and the GTK4 frontend in the
binary's `src/ui/`. Background work (scoring, history, launching) runs on
worker threads and hops back to the main loop via
`glib::spawn_future_local`.

```
src/
  lib.rs        # pub mod engine   (GTK-free, unit-testable)
  main.rs       # bin: clap args, control socket, GTK init, engine bootstrap
  engine/       # providers + scoring + history (the model)
  ui/           # GTK4 window, control socket, background tasks, CSS theme
data/style.css  # default stylesheet
tests/          # in-process integration tests against the engine
```

### Providers

| Provider | Trigger | Source |
|---|---|---|
| `DesktopEntryProvider` | (always active) | `freedesktop-desktop-entry` — reads `.desktop` files |
| `CalculatorProvider` | `=` prefix | `rink-core` — evaluates math expressions, copies to clipboard, `Tab` applies its query suggestion |
| `MetaProvider` | `@` prefix | launcher state — uptime, control socket path, pid, version (select copies the value) |

The `Provider` trait lets you plug arbitrary data sources into the
launcher. See **[docs/WRITING_A_PROVIDER.md](docs/WRITING_A_PROVIDER.md)**
for the full guide with trait docs, builder API reference, and examples.

The engine ranks the full result set on every query; the UI cuts its
`page_size`-sized window out of it around the current selection.

### Data model

History is a flat `HashMap<String, HashMap<HistoryKey, HistoryRecord>>` (not
a pointer-linked tree — exact-string lookup is the only access pattern):

```
""  → { firefox.desktop: { score: 12.4, n: 47, last_update: … }, … }
"f" → { firefox.desktop: { score:  8.2, n: 30, last_update: … }, … }
```

Fan-out on write is a substring loop, not a tree walk. Persistence is JSON
with synchronous write-back on every mutation.

See [docs/ALGORITHM.md](docs/ALGORITHM.md) for the full scoring design (confidence
weighting, exponential decay, the prefix trie, and adaptation behavior).

---

## Project status

**Early-stage but functional.** The binary compiles and runs. The core
loop (type → fuzzy match → blend with history → select → learn) works
end-to-end.

### Planned / in discussion

- Rethinking the weighted sum formula (making extra match fields less
  dominant)

---

## Tests

```console
nix develop --command cargo test
```

Runs the engine's unit tests plus in-process integration tests that build an
engine with test providers and page through queries.

---

## License

GNU General Public License v3.0 only. See [LICENSE](/LICENSE).