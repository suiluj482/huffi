# Theming

A theme is a directory of files that change how huffi looks. There are two kinds
of file, and they answer different questions:

| File        | Format                    | Controls                                          |
|-------------|---------------------------|---------------------------------------------------|
| `style.css` | CSS                       | how things look: colours, fonts, spacing          |
| `entry.ui`  | [GTK Builder](https://docs.gtk.org/gtk4/class.Builder.html) XML | what is in a row: which widgets, and in what order |

Start with the stylesheet. Recolouring huffi, resizing text, or restyling the
search box needs no XML at all — only the row layouts below need templates, and
only if you want to change *which* widgets a row has.

## Where themes come from

Themes ship in the binary, compiled in from `data/themes/<name>/`, and are also
read from `~/.config/huffi/themes/<name>/`. Both are selected the same way, with
`[ui] theme = "<name>"` in `~/.config/huffi/config.toml`.

Four themes ship today:

| Name                    | Palette                                          |
|-------------------------|--------------------------------------------------|
| `default`               | neutral greys with a slate accent — used when `theme` is unset |
| `catppuccin-mocha-mauve` | [Catppuccin](https://catppuccin.com/palette) Mocha with the mauve accent |
| `nord`                  | [Nord](https://www.nordtheme.com/) — arctic blue accent |
| `tokyo-night-storm`     | [Tokyo Night](https://github.com/enkia/tokyo-night-vscode-theme) Storm variant, with the blue accent |

The three coloured themes are huffi's own, **based on** the palettes named above.
Only the colours come from those palettes. The layout — the sizes, the spacing, what
gets a highlight and what gets left alone — is huffi's, arranged for a panel this
small, so nothing here is a faithful port and none of it follows the palette it came
from. If a row reads badly or a contrast looks off, that's the arrangement rather
than the palette, and the fix belongs in `style.css` next to the rules below.

Builtin themes are **embedded, not installed**, so start your own theme by
creating a directory and writing only the files you want to change:

```sh
mkdir -p ~/.config/huffi/themes/mine
```

then select it:

```toml
# ~/.config/huffi/config.toml
[ui]
theme = "mine"
```

If you have a checkout of huffi, `data/themes/default/` is the reference for
what a theme may contain and the cheapest way to start — copy a stylesheet out of
it and delete what you do not need.

## The layers

A theme is layered over the others file by file, lowest first:

```text
default (builtin)  →  selected builtin theme  →  your theme
```

Each layer wins **per file**: a file present in yours replaces the matching file
below it, and a position you leave out falls back to the layer beneath. So a
theme directory containing nothing but a two-line `style.css` is valid, and so is
one that overrides `entry.ui` and leaves the stylesheet alone.

`default` is the base layer rather than merely a theme you can select: every
position that isn't supplied by a more specific layer resolves there, which is
what lets each alternative ship a single file. It is also what a name
matching nothing at all renders as — huffi warns on stderr and says which builtin
themes exist, since a typo is otherwise indistinguishable from a stylesheet that
isn't applying.

Two details, because they don't generalize the same way:

- **Between builtin layers, a stylesheet replaces rather than layers.** Only one
  `style.css` is registered per theme, so a builtin theme that ships one is
  responsible for all of it. Copy `data/themes/default/style.css` and edit, as
  `nord` does.
- **Your stylesheet does layer**, at GTK's `USER` priority over the builtin one
  at `APPLICATION`. That's the layering you can do partially, and it's what makes
  a two-rule stylesheet a complete recolour of one part of the UI.

A user theme whose name matches a builtin one overrides that builtin theme's
files, and still inherits from `default` for the positions it says nothing about.

## Theme directories

```text
data/themes/<name>/
  style.css                 # the one stylesheet, for every row
  entry.ui                  # default GTK Builder row template
  variants/<name>/
    entry.ui                # optional, row layout for one layout variant,
                             #   for every provider reporting it
  providers/<id>/
    entry.ui                # optional, custom row layout for that provider
    <variant>/
      entry.ui              # optional, row layout for one layout variant of
                             #   this provider
```

The tree has one stylesheet position and four template positions, and the two
compose differently, because CSS and GTK Builder files are not the same kind of
thing:

- **The stylesheet layers**, as described above.
- **Row templates are selected, not merged.** Exactly one `entry.ui` wins per
  row, by the resolution chain below. There is no such thing as overriding one
  label inside another theme's row template — you take the whole template and
  write it out.

The two halves are covered separately: [Stylesheets](#stylesheets) first, then
[Row templates](#row-templates).

## Stylesheets

### How your stylesheet combines with the builtin one

The builtin `style.css` is always registered too, at a lower priority than yours.
That means a two-rule stylesheet is a complete recolour of one part of the UI,
and everything else keeps its default appearance:

```css
/* ~/.config/huffi/themes/mine/style.css */
.title {
    font-family: monospace;
}
.detail {
    color: #888;
}
```

The flip side is that a default rule cannot be *removed*, only overridden — a
stylesheet here is an addition, not a replacement.

There is no per-provider stylesheet. GTK applies every registered sheet to every
widget in the display, so splitting CSS across `providers/<id>/style.css` would
gain nothing: the rules would still all apply to all rows, and would interact
with each other by priority. Scoping is done with classes instead.

### Classes you can target

Because the one stylesheet applies to everything, these classes are how you
scope a rule to the rows you mean. Rows carry:

| Class                        | On every row? | Meaning                                 |
|------------------------------|---------------|-----------------------------------------|
| `row` / `row-selected`       | yes           | the row / the selected row              |
| `title` / `title-selected`   | yes           | the title label / when selected         |
| `subtitle` / `subtitle-selected` | when present | the subtitle label                  |
| `comment` / `comment-selected`   | when present | the comment label, for prose           |
| `score` / `score-selected`   | when declared | both score labels share these two       |
| `detail`                     | when present  | each filled-in `detail-<key>` label     |
| `provider-<id>`              | provider rows | which provider produced it              |
| `provider-<id>-<variant>`    | variant rows  | this provider's layout variant          |
| `variant-<variant>`          | variant rows  | the layout variant, provider-independent |

Each `-selected` class **coexists** with the class it qualifies rather than
replacing it: a selected row's title carries both `title` and `title-selected`.
The pair therefore names a role and a state, so `.title { … }` styles every
title and `.title-selected { … }` styles only the state — and a state rule needs
to state only what differs:

```css
.title             { font-size: 15px; color: @huffi_text_color; }
.title-selected    { color: @huffi_accent_color; }
```

It also means a state can be reached from the row instead of the widget, which is
how `.row-selected .detail` recolours every detail in a selected row at once —
the renderer never touches the detail's classes, so any template inherits it.

`.provider-calculator .title` cannot match a desktop row, because a desktop row
does not carry `provider-calculator`. That is the whole of the isolation
mechanism:

```css
.provider-calculator .title { color: @accent_color; }
.provider-desktop .row { border-bottom: none; }
.variant-info .detail { font-style: italic; }
.row-selected .detail { color: @huffi_accent_color; }
```

Because class tokens are matched whole, `.provider-calculator-info` selects
exactly that class and nothing else — no escaping is involved. The practical
advice is only that you write the **longest** class name you mean: `.provider-my`
matches all of a provider's rows *including its variants*, which is often not
what you want.

Of the two variant classes, `variant-<variant>` is the one that scales:
`.variant-info .title` styles every provider's info rows with one rule, where
`.provider-calculator-info` is specific to one provider.

### Sizing one provider differently

Pairing a base class with a provider class is enough to restyle a single
provider, and the base stylesheet uses it to give calculator rows a larger
type size — a result is read rather than scanned, and is often the only row on
screen:

```css
.provider-calculator .title   { font-size: 17px; }
.provider-calculator .detail  { font-size: 12px; }
```

One rule per widget covers both the selected and unselected states, because the
role class survives selection and the state rules don't set a size.

There is one consequence worth knowing, and it runs the other way from the last.
A provider-scoped rule is *more* specific than a state rule, so
`.provider-my .row { background-color: … }` outranks `.row-selected` and paints
over the selection highlight — and `.provider-my .title { color: … }` likewise
outranks `.title-selected`. That is ordinary CSS specificity, and it is usually
what you want: a provider that sets a row's background has claimed it. When it
isn't, restate the state rule afterwards — they tie on specificity, so source
order decides:

```css
.provider-my .row  { background-color: #1e1e2e; }
.row-selected .row { background-color: @huffi_surface1_color; }
```

Beyond that, **put provider-scoped rules at the end of the stylesheet**, so they
beat the state rules above them on equal specificity. A base rule written below
one of them silently takes precedence again, which is exactly the kind of
regression that survives a test suite.

### Styling the rest of the panel

The overlay around the rows is classed too, for anyone restyling more than the
results:

| Class         | Widget                                             |
|---------------|----------------------------------------------------|
| `huffi-entry` | the search box (with `:focus-within`, `> text`, `> placeholder`, `> selection` for its parts) |
| `panel`       | the panel behind everything, with its padding       |
| `footer`      | the hint line under the results                    |
| `footer-active` | the providers answering the current query, which the footer shows separately on the left (the rest, on the right, keep the plain `footer` colour) |
| `badge`       | the suggestion badges                              |
| `flat-btn`    | the borderless "+" / "−" buttons, with `:hover`   |

The base theme also `@define-color`s `huffi_base_color`, `huffi_surface0_color`,
`huffi_surface1_color`, `huffi_text_color`, `huffi_subtext0_color` and
`huffi_accent_color` as named colours, so a theme can build on the default palette
by name instead of copying hex values out of it. Reference them as
`@huffi_subtext0_color`.

One of them reaches further than CSS: `huffi_accent_color` is read out of the
style context to tint the scroll rail, which is drawn rather than styled.
Redefine it and the rail follows; delete it and the rail falls back to the
default accent.

## Row templates

`entry.ui` is a GTK Builder interface containing one root object. Templates are
only needed when you want to change *which widgets a row has* or how they are
arranged — a rule like `.title { font-family: monospace }` is a stylesheet
concern and needs no template at all.

### Which template a row uses

Every row picks the most specific template available for its provider and layout
variant:

```text
providers/<id>/<variant>/entry.ui  →  providers/<id>/entry.ui
    →  variants/<variant>/entry.ui  →  entry.ui
```

Each of those four positions is resolved **independently**: huffi asks your theme
for the file, and if your theme does not have it, asks the next layer down. So
the chain above is really "first position that exists in any layer wins",
falling through one file at a time.

That has one consequence worth knowing. If you override
`providers/calculator/entry.ui` but leave `providers/calculator/info/entry.ui`
alone, substance and unit rows render with the **base theme's** info template,
not yours. To give one provider a single layout across all of its variants,
shadow those variant templates with copies of your own file.

If no template is found at all the row is empty rather than an error. In practice
`entry.ui` is always present, so the chain has a guaranteed fallback.

There are four positions rather than one because templates answer "what widgets
does this row have?", and that genuinely differs: a calculator substance row
carries a prose `comment` under the title, where a number row has nothing to say
there and would be left with a blank line. There is no way to merge two layouts,
so each arrangement is its own file.

The two variant positions differ only in scope:

| Position                   | Whose opinion is it?                     |
|----------------------------|------------------------------------------|
| `providers/<id>/<variant>` | this provider's, for its own rows        |
| `variants/<variant>`       | the theme's, for *every* provider's rows |

A shared `variants/list/entry.ui` therefore lets one file serve any number of
providers, with no per-provider duplication. The cost is that a shared template
can only populate widget ids that mean something everywhere — `title`,
`subtitle`, `comment`, and `detail-<key>` for keys that are generic. It cannot
reference a particular provider's key, because a label for one provider's detail
means nothing to a provider that has no such detail.

The shared position comes **after** the provider's own template. That ordering
is deliberate: a theme that customises one provider keeps winning for it, and
only providers with no file of their own fall through to the shared one.

### Widget ids

The renderer instantiates the template per row and binds the entry's fields onto
these ids:

| Widget id       | Type        | Bound to                                              |
|-----------------|-------------|-------------------------------------------------------|
| `row`           | `GtkBox`    | the row container (effectively required)              |
| `clickable`     | `GtkBox`    | click target: selects on click, submits on double-click |
| `icon`          | `GtkImage`  | entry icon (pixel size comes from `[ui].icon_size`)   |
| `title-area`    | `GtkBox`    | container for the title and subtitle                  |
| `title`         | `GtkLabel`  | entry title                                           |
| `subtitle`      | `GtkLabel`  | entry subtitle (shown only when the entry has one)    |
| `comment`       | `GtkLabel`  | entry comment: free-form prose, shown only when the entry has one |
| `scores`        | `GtkBox`    | container for the score labels                        |
| `score-base`    | `GtkLabel`  | base (fuzzy) score                                    |
| `score-history` | `GtkLabel`  | history score (shown only when present)               |
| `boost`         | `GtkButton` | "+" history button (only for entries with a history key) |
| `delete`        | `GtkButton` | "−" history button (only for entries with a history key) |
| `detail-<key>`  | `GtkLabel`  | one named detail (see below)                          |

Every one is optional, with one caveat: a template without `row` still produces
a row, but the widgets you declared are then never attached to anything, so the
row renders as an empty box. Anything you add beyond the known ids — a
`GtkImage` decoration, an extra separator — is left untouched, so templates can
carry arbitrary widgets.

`title`, `subtitle`, and `comment` are three lines of one entry rather than three
unrelated fields: the headline, a short qualifier, and prose. A theme that
declares all three decides the arrangement; a theme that declares only some is
fine, since each is filled only when the entry has it.

Three details about the renderer that are easy to get wrong. A template supplies
the *label*, not its text, so `title` must be declared without a `label`
property — otherwise the renderer sets the text but you have pinned a competing
one, and the row renders blank. An optional widget such as `subtitle` or
`comment` should be declared `visible=false`, since the renderer reveals it only
when the entry actually has that field. And `boost` / `delete` are the one pair
the renderer *hides* as well as reveals, setting visibility from the presence of
a history key — so `visible=false` on those is belt-and-braces rather than
load-bearing, unlike `subtitle`.

### Named details

An entry can carry arbitrary key/value display fields — a unit's quantity and
dimensionality, a date's humanized relative time, a substance's properties. A
template opts in one at a time by declaring a `detail-<key>` label:

```xml
<child>
  <object class="GtkLabel" id="detail-quantity">
    <property name="visible">false</property>
  </object>
</child>
```

Keys are chosen by the provider and must match `[a-z0-9-]+`, since they become
part of a GTK object id. `EntryBuilder::detail` enforces that with a
`debug_assert!`, so a provider that breaks the rule fails loudly in development
and in the test suite rather than silently shipping an invisible field. A
release build doesn't panic on it — the renderer skips a key that fails the
check, since one bad key in a third-party provider shouldn't take the launcher
down on every keystroke.

The renderer fills in every declared widget the entry actually carries and gives
it the `detail` class; a detail with no matching widget is not shown, and a
widget with no matching detail stays hidden.

There is deliberately **no catch-all widget** — a template renders exactly the
details it declares, so a user can drop any field they do not care about, and a
provider can add a field without every existing theme suddenly showing it.

Because details are opt-in and `detail`-classed, styling them needs no renderer
changes:

```css
.detail { font-size: 11px; color: @huffi_subtext0_color; }
.row-selected .detail { color: @huffi_accent_color; }
```

Long values are the template author's problem, not the renderer's: give the
label `ellipsize=end` with a `max-width-chars`, or `wrap=true` and let it take
several lines. The default calculator template ellipsizes most details this way.

### Layout variants

A variant names a *layout*. It selects a more specific template via the chain
above, and adds two CSS classes, so a variant can be restyled without writing
any XML at all.

Variant names are shared vocabulary, not private labels, because a theme can
ship `variants/<name>/entry.ui` once and have every provider reporting `<name>`
pick it up. That only pays off if the name means the same thing everywhere, so
prefer names describing the layout a row wants and that another provider could
plausibly also want — `list` for a row with a tall wrapping detail, rather than
`unit-list` for one provider's result type. If a layout really is yours alone,
`providers/<id>/` already expresses that, and an unshared variant name is better
expressed by not using a variant at all.

## Worked example: the calculator

The bundled calculator provider is the theme feature with the most going on. It
evaluates `=`-prefixed expressions with [rink] and hands back a structured
result, which it splits across a row's three text slots plus its detail fields:

| Query          | `title`                  | `subtitle`          | `comment` | details                    |
|----------------|--------------------------|---------------------|-----------|----------------------------|
| `=1.609 km`    | `1.609 kilometer`        | —                   | —         | `quantity`, `dimensions`   |
| `=42`          | `42`                     | —                   | —         | `quantity`                 |
| `=now`         | `2026-10-03 00:00:00 …`  | `in 3 days`         | —         | —                          |
| `=water`       | `water`                  | —                   | —         | `properties`               |
| `=pascal`      | `pascal`                 | `1 pascal`          | *SI derived unit …* | `def`           |
| `=5 min`       | `5 minute, 0 second (time)` | —              | —         | —                          |

Three things are worth reading off that table.

**The title is the smallest canonical form of one fact, not a sentence.** rink's
own rendering of `=water` is `water: <doc> {…}` and of `=pascal` is
`Definition: pascal = … . SI derived unit …`. Neither belongs in a list row, and
neither is anything you would paste. So the title takes just the name or the
value, and the rest is distributed — which also means the title doubles as the
clipboard value and as the expression re-evaluated when the row is tab-selected,
both of which stay meaningful.

**A result only carries what is actually there.** `=42` is dimensionless, so
there is no `dimensions` to report; `=1.609 km` has a `length` quantity to
mention. Details a result does not have are simply not emitted, so a template
that declares them costs nothing.

**Prose goes in the comment, not in a detail.** A unit's documentation can be a
sentence or two, which no `detail-<key>` label is shaped for. That is what
`providers/calculator/info/entry.ui` arranges, and it is the only calculator
layout that isn't the stock one:

```text
providers/calculator/entry.ui        # title, subtitle, quantity, dimensions
providers/calculator/info/entry.ui   # adds a comment, for substance and unit rows
```

### Variants name layouts, not result kinds

The calculator emits three variant names, and they are three *arrangements*
rather than ten kinds of answer:

| Variant  | Selected for                                    | Shape                                    |
|----------|-------------------------------------------------|------------------------------------------|
| `number` | numbers and conversions                         | title, optional quantity/dimensions      |
| `info`   | substances and unit definitions                 | title, value, prose, properties          |
| `list`   | durations, factorizations, unit lists, searches | one line of text, nothing else           |

A date has **no** variant at all: a title plus a subtitle is exactly the stock
row, so naming one would buy nothing. A conversion likewise has no variant of
its own, because rink renders it as a bare value — it *is* a `number` row.

That collapsing is the point. Naming a variant after a result kind puts the
provider's data model in your theme's directory names, and every new kind
becomes a new template that mostly looks like the last one. Naming it after the
arrangement means a theme writes one file per distinct *look*.

The full detail key set is `calculator::DETAIL_KEYS` in
[`src/engine/provider/builtin/calculator.rs`][calculator], and a test asserts the
shipped templates declare only ids from that list — so a field can never appear
in a row that no template asked for, and a key the calculator drops can't leave a
widget stranded. Declare ids from that list and your theme gets exactly the
fields you named.

[rink]: https://github.com/tiffany352/rink-rs
[calculator]: ../src/engine/provider/builtin/calculator.rs
