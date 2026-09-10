# Writing a huffi provider

A provider is a Rust struct that implements the `Provider` trait. It acts as
a data source for the launcher — every keystroke, the engine calls every
registered provider's `query()` method and blends the returned entries into
a ranked list.

## The trait

```rust
pub trait Provider: Send {
    fn id(&self) -> &str;
    fn meta(&self) -> ProviderMeta;
    fn init(&mut self, ctx: InitContext) -> ProviderResult;
    fn query(&mut self, ctx: QueryContext) -> Vec<Entry>;
}
```

### `id()`

A short, unique name for this provider (e.g. `"desktop"`, `"calculator"`).
Used in logs, select dispatch, and config lookup. This is the **stable**
identity — it is never overridden by user config.

### `meta()`

Returns a [`ProviderMeta`] with the display name, trigger prefixes, enabled
flag, and a `prefix_only` flag. All fields are overwritable by user config
under `[engine.provider.builtin.<id>]`.

[`ProviderMeta`] implements [`Default`], so a provider only spells out what
differs from the defaults (empty name — resolved to the provider `id()` at
registration — no prefixes, enabled, queried for every input):

```rust
fn meta(&self) -> ProviderMeta {
    ProviderMeta { ..Default::default() }
}
```

**`prefix_only`** — when `true`, the engine never calls your `query()`
unless the user's input matched one of your prefixes; the provider is
skipped entirely otherwise. Prefix-triggered providers can declare this
instead of returning `vec![]` for unprefixed input, saving a call per
keystroke. [`CalculatorProvider`] and [`MetaProvider`] do this.

Prefixes are resolved once per query by the engine: the **longest** declared
prefix that the user's input starts with becomes the *global prefix* for that
query — there is at most one active prefix per query. When the user's input
starts with that prefix, `query()` is called with `prefix: Some("...")` and
the text *after* the prefix as `query`. If a provider's prefixes do **not**
contain the global prefix, it is called with `prefix: None` and the original
full query — the same as for providers with no prefixes at all.

Entries are fuzzy-matched against that `query` parameter, so a matching
provider's prefix is left out of scoring: typing `"= 2 + 2"` with prefix `"="`
fuzzy-matches the provider's entries against `" 2 + 2"`, not the full input.
Providers whose prefix did not match score against the full input instead. A
`.score()`-based entry isn't affected by any query either way.

### `init(ctx)`

Called once at startup, before any queries are served. The `InitContext`
carries:

- `data_dir` — this provider's **own** data folder:
  `<data dir>/providers/<provider id>/`, created by the engine before
  `init()` runs (unless running in `--dry-run`). Use it to keep
  per-provider state (caches, indices, logs) without colliding with other
  providers — huffi never needs to know what you put there, and you never
  need to locate or create the folder yourself.
- `extra` — optional arbitrary config from
  `[engine.provider.builtin.<id>.extra]`. Not schema checked — your
  provider is responsible for interpreting it (e.g. via
  `serde_json::from_value`).

Use this for expensive one-time work:

- Scanning `.desktop` files
- Initialising a parser context (e.g. loading unit definitions for a
  calculator)
- Building an index, opening your own storage under `data_dir`

Return `ProviderResult::Config { msg, critical }` when the `extra` config
is present but invalid: a non-critical error logs a warning and the
provider continues with its defaults, a critical one disables the provider.
`ProviderResult::Unsupported` disables the provider when the platform can't
support it (e.g. a missing binary).

Since `init()` is only called once, keep `query()` as lightweight as
possible. Your provider holds arbitrary state — store everything you need
in your struct fields during `init()`.

### `query(ctx)`

Called on **every keystroke** while the user types. Return the entries your
provider wants to offer for the current input.

**Performance matters** — this runs for every keystroke. If the user types
quickly, `query()` is called rapidly. Avoid I/O, allocations in hot paths,
or expensive computation here. Cache everything in `init()`.

The `QueryContext` fields:

- `prefix` — `Some("...")` when the provider's declared prefix matched,
  `None` otherwise.
- `query` — the text after the prefix when a prefix matched, otherwise
  the full typed text.
- `original` — the full query as typed, including the prefix.

```
User types         prefix     query
──────────         ──────     ─────
""                 None       ""
"f"                None       "f"
"= 2 + 2"          Some("=")  " 2 + 2"
```

The global prefix is the longest matching prefix across **all** providers. So
if another provider declared the prefix `"=="`, typing `"== 2"` would activate
*that* prefix; a provider only declaring `"="` would then be called with
`prefix: None` and the full `"== 2"` query instead.

A prefix-triggered provider typically returns an empty `Vec` when `prefix`
is `None`, since it has nothing to offer for un-prefixed input. This is
exactly what [`CalculatorProvider`] does — it returns `vec![]` at the top
of `query()` when no prefix matched, staying performant and out of the way for normal app
searching.

## Entries and `EntryBuilder`

Create entries with the [`entry()`] builder function:

```rust
entry("my-entry-id", "Display Name")
    // optional fields…
    .build()
```

### Builder methods

| Method | Type | Purpose |
|---|---|---|
| `.subtitle(s)` | `String` | Secondary text shown beside the title in the UI |
| `.comment(s)` | `String` | Longer description (used as a fallback subtitle) |
| `.icon(name\|path)` | `String` / path | Icon to show. A string maps to a themed freedesktop icon name (see `.icon_name`); a `Path` to an explicit icon file (see `.icon_path`) |
| `.extra(json)` | `serde_json::Value` | Arbitrary metadata attached to the entry, surfaced on the query hit |
| `.exec(args)` | `Vec<String>` | Shell command to run on selection (no terminal) |
| `.terminal_exec(args)` | `Vec<String>` | Shell command to run in a terminal |
| `.clipboard(value)` | `String` | Copy `value` to the clipboard on selection (configurable default wl-copy) |
| `.history_key(key)` | `String` | Enable history tracking under this stable key |
| `.set_query(query)` | `String` | Query suggestion applied when this entry is tab-selected |
| `.score(s)` | `f32` | Static score (no fuzzy matching) |
| `.match_fields(fields)` | `Vec<MatchField>` | Fuzzy-match these weighted text fields |
| `.match_field(text)` | `String` | Shortcut for a single fuzzy-match field at weight 1.0 |

Only one of `.score()` or `.match_fields()` may be used on a single entry.
If neither is called, the entry gets a default score of `1.0`.

### Icons

`.icon()` describes the entry's icon without resolving it to an image — the
UI turns it into a widget. There are two sources:

- **`.icon_name(name)`** — a freedesktop icon theme name (e.g.
  `"firefox"`, `"accessories-calculator"`). The UI resolves it against the
  active icon theme, so theme fallbacks, SVG/PNG, and dark/symbolic variants
  all work for free. Passing a `&str`/`String` to `.icon()` is shorthand for
  this.
- **`.icon_path(path)`** — an explicit path to a PNG or SVG file, rendered
  verbatim. Passing a `Path`/`PathBuf` to `.icon()` is shorthand for this.

### Scoring modes

**`.match_fields(fields)`** — the standard choice. The entry is fuzzy-matched
against the user's query using [`nucleo`]. Each field has a weight that
controls its importance:

```rust
entry("firefox.desktop", "Firefox")
    .match_fields(vec![
        MatchField { text: "Firefox".into(),            weight: 1.0 },
        MatchField { text: "Browse the Web".into(),     weight: 0.5 },
        MatchField { text: "web browser".into(),        weight: 0.8 },
    ])
```

Fields are scored independently and combined as a weighted average. A match
in a higher-weighted field boosts the result more.

**`.score(s)`** — a static score bypasses fuzzy matching entirely. The entry
gets a fixed base score that the history-blending step operates on. Useful
for entries that are always relevant regardless of query (e.g. a calculator
result), or for entries whose scoring is handled externally.

The value must lie in `0.0..=1.0`. Scores outside this range are passed
through as-is (never normalized), so a score above `1.0` would outrank the
best fuzzy match in the batch and a score below `0.0` would rank below an
empty-query baseline of `0.8`.

### History tracking

If an entry has a `history_key`, the engine records launches and boosts
against that key. Over time the ranking becomes personalised — frequently
launched entries rise for their typed prefix.

Entries **without** a `history_key` still appear in results but are
invisible to the history model: they can't be boosted, deleted, or learned
from usage. This is appropriate for ephemeral or computed entries.

The `history_key` should be stable across restarts, same as `id`.
Often they are the same value.

### Query suggestions

Setting `.set_query(query)` gives the entry a query suggestion. When it is the
currently selected entry and the user presses `Tab`, the UI replaces the
input with the suggestion and re-runs the search. Pressing `Tab` on an entry 
without `set_query` does nothing.

The suggestion doesn't have to be a completion of what the user typed — it
can be a completely different query. This is useful for chaining entries
that produce a value. The [`CalculatorProvider`] sets its suggestion to the
prefix plus the computed result, so pressing `Tab` on the `= 2 + 2` result
switches the input to `= 4` and lets you keep calculating without retyping
the `=` prefix.

```rust
entry("my-calculator", "4")
    .set_query("= 4")
    .score(1.0)
```

### Action: what happens on selection

When the user selects an entry, its `Action` is performed:

- **`.exec(args)`** — spawns the command as a child process with stdout and
  stderr discarded. The first element of `args` is the program to run.
- **`.terminal_exec(args)`** — same, but the command is launched inside a
  terminal emulator (configurable default `kitty`) instead.

If no action is set, selection does nothing (`Action::NoOp`).

## Complete example: always-active provider

```rust
use huffi::engine::provider::{Entry, Provider, ProviderMeta, ProviderResult, InitContext, entry};
use huffi::engine::scoring::MatchField;

struct CustomDirProvider { entries: Vec<Entry> }

impl Provider for CustomDirProvider {
    fn id(&self) -> &str { "custom-dirs" }
    fn meta(&self) -> ProviderMeta {
        ProviderMeta { name: "Custom Dirs".into(), prefixes: vec![], enabled: true }
    }

    fn init(&mut self, ctx: InitContext) -> ProviderResult {
        self.entries = vec![
            entry("projects", "Projects")
                .exec(vec!["xdg-open".into(), "/home/me/projects".into()])
                .match_fields(vec![
                    MatchField { text: "Projects".into(), weight: 1.0 },
                ])
                .history_key("custom-projects")
        ];
        ProviderResult::Ok
    }

    fn query(&mut self, _ctx: QueryContext) -> Vec<Entry> {
        self.entries.clone()
    }
}
```

## Complete example: prefix-triggered provider

See [`CalculatorProvider`] in the source — it triggers on `=`, evaluates
expressions with [`rink-core`], and returns a single entry with the computed
result as its title and a `.set_query("= …")` query suggestion so `Tab`
chains calculations.

## Registering a provider

Register it on the [`Engine`] in `src/main.rs`, after `Engine::new`:

```rust
let mut engine = Engine::new(&data_dir, args.dry_run)?;
engine.add_provider(Box::new(MyProvider::default()));
```

`add_provider` creates the provider's `<data_dir>/providers/<id>/` folder (when not in
dry-run mode) and calls [`init(ctx)`][`init()`] before the provider is
queried. User-config overrides from `[engine.provider.builtin.<id>]` are
applied to the provider's [`meta()`] before init, and any `extra` config
is passed through `InitContext`. Built-ins are registered in
[`ProviderCollection::new_with_config()`] in `src/engine/provider/collection.rs`.

[`Engine`]: ../src/engine/mod.rs
[`ProviderCollection::new_with_config()`]: ../src/engine/provider/collection.rs
[`init()`]: #init
[`CalculatorProvider`]: ../src/engine/provider/builtin/calculator.rs
[`entry()`]: ../src/engine/provider/util.rs
[`nucleo`]: https://github.com/helix-editor/nucleo
[`rink-core`]: https://github.com/tiffany352/rink-rs
