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
  enable = true;
  daemon.enable = true;  # optional: always-warm systemd service
};
```

### From source

```console
git clone https://github.com/suiluj482/huffi
cd huffi
cargo build --release
# binaries in target/release/: huffi-daemon, huffi, huffi-ui
```

No root or manual daemon setup is needed — the CLI and UI auto-spawn the
daemon as a child process on first use.

---

## Usage

### `huffi-ui` — Wayland layershell GUI

A floating overlay that appears with keyboard focus. Start typing to fuzzy
search, use arrow keys or tab to navigate, and press enter to launch.

```console
huffi-ui                        # launch empty
huffi-ui --query fire           # start with an initial query
huffi-ui -q "= 2 + 2"           # shorthand (note: quote for spaces)
```

| Key | Action |
|---|---|
| Type | Fuzzy search |
| `↓` / `↑` | Select next / previous |
| `Tab` | Select next (wraps) |
| `Enter` | Launch selection |
| `Esc` | Dismiss |
| Mouse wheel | Scroll results |
| `+` / `−` button | Boost / delete association |

### `huffi-daemon` — scoring engine

```console
# Run explicitly (normally auto-spawned)
huffi-daemon
huffi-daemon --dry-run        # ephemeral, no disk writes
huffi-daemon --socket /tmp/mysock --data /tmp/history.json
```

### `huffi` — CLI client

```console
# Search for apps
huffi query fire

# Record a launch (trains the model)
huffi select fire firefox.desktop

# Boost an app at the exact typed prefix (no fan-out)
huffi boost fi finder.desktop

# Delete an association at the exact prefix
huffi delete fi firefox.desktop

# Inspect raw scores for a prefix
huffi history f

# List providers and their trigger prefixes
huffi providers
```

---

## Features

- **Adaptive ranking** — fuzzy text match blended with time-decayed usage
  history. The more you type, the more the model defers to exact-prefix
  memory rather than top-level defaults. See [docs/ALGORITHM.md](docs/ALGORITHM.md) for
  the full design.
- **Calculator** — type `=` followed by an expression to evaluate via
  [`rink-core`](https://github.com/tiffany352/rink-rs). Results are copied
  to the clipboard with `wl-copy`.
- **Desktop entries** — parses `.desktop` files via
  [`freedesktop-desktop-entry`](https://crates.io/crates/freedesktop-desktop-entry),
  matching against name, keywords, generic name, and comment with field
  weighting.
- **Boost / Delete** — correct the model in the moment. Boost is a
  synthetic 10x launch, scoped to the exact prefix. Delete removes the
  association entirely. Both are available in the CLI, UI, and protocol.
- **Prefix resolution** — each query is preprocessed once to resolve the
  *global prefix*: the longest declared provider prefix that the input
  starts with. It is returned with every query result so the UI can mark it
  (badge next to the input), and providers whose prefixes don't match are
  still queried with the full input.
- **Auto-spawning daemon** — no init script or manual setup. The client
  spawns the daemon as a detached child if it isn't running.
- **Icon support** — SVG and PNG icons loaded from the desktop entry's
  icon path, displayed in the UI via iced.
- **JSON-over-Unix-socket protocol** — NDJSON framing over a Unix socket.
  The protocol boundary means alternative frontends (e.g. a dmenu-compatible
  CLI shim, a Quickshell widget, or a third-party GUI) can drive the same
  daemon.
- **Persistence** — history stored in `$XDG_DATA_HOME/huffi/history.json`
  with atomic writes (`.json.tmp` + rename). Two-week half-life exponential
  decay. No background jobs, no unbounded logs.
- **Nix flake** — reproducible builds for `x86_64-linux` and
  `aarch64-linux`, dev shell with all Wayland dependencies, and a Home
  Manager module with optional `systemd --user` daemon service.

---

## Architecture

The project is a Rust workspace with four crates:

| Crate | Binary | Role |
|---|---|---|
| `huffi` | `huffi-daemon` | Scoring engine, history store, provider trait + implementations |
| `huffi-protocol` | — | Shared NDJSON-over-Unix-socket message types and daemon auto-spawn |
| `huffi-cli` | `huffi` | CLI client for scripting and testing |
| `huffi-ui` | `huffi-ui` | Wayland layershell GUI built with [`iced_layershell`](https://github.com/Heufneutje/iced_layershell) |

### Providers

| Provider | Trigger | Source |
|---|---|---|
| `DesktopEntryProvider` | (always active) | `freedesktop-desktop-entry` — reads `.desktop` files |
| `CalculatorProvider` | `=` prefix | `rink-core` — evaluates math expressions, copies to clipboard |

The [`Provider`] trait lets you plug arbitrary data sources into the
launcher. See **[docs/WRITING_A_PROVIDER.md](docs/WRITING_A_PROVIDER.md)**
for the full guide with trait docs, builder API reference, and examples.

[`Provider`]: https://docs.rs/huffi/latest/huffi/provider/trait.Provider.html

Ranking state lives in a long-running daemon process. The daemon binds its
Unix socket before doing slow work, so a racing client doesn't time out. On
first use, the client spawns the daemon as a detached child — zero config.

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

**Early-stage but functional.** All four crates compile and run. The core
loop (type → fuzzy match → blend with history → select → learn) works
end-to-end. ~30 passing tests.

### Implemented

- Fuzzy matching via `nucleo` with field-weighted scoring
- History store with exponential decay (2-week half-life), confidence-weighted
  blending, and fan-out on write
- Desktop entry provider (reads real `.desktop` files, filters `NoDisplay`
  and non-`Application` types)
- Calculator provider (`=` prefix)
- Boost and delete (protocol, daemon, CLI, UI)
- JSON persistence with atomic writes
- Daemon auto-spawn (client spawns daemon on first use)
- Wayland layershell GUI (keyboard navigation, pagination, icons, scrollbar,
  boost/delete buttons, Catppuccin Mocha theme)
- Nix flake + Home Manager module with optional systemd service
- Protocol-level pagination (`offset`, `length` in queries)
- Integration tests (spawn a real daemon, run queries)

### Planned / in discussion

- Config file
- Rethinking the weighted sum formula (making extra match fields less
  dominant)
- Possible simplification: merge daemon/UI into a single process
- `xdg-desktop-portal` integration (icon lookup improvements)

---

## Tests

```console
cargo test
```

Runs ~30 unit tests across all crates plus integration tests that spawn a
real daemon.

---

## License

GNU General Public License v3.0 only. See [LICENSE](/LICENSE).
