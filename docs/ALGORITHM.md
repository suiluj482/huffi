# Algorithm

## Motivating analogy

Huffman coding assigns short codes to frequent symbols and long codes to rare
ones, and it's provably optimal: expected code length equals the entropy of
the source. Swap "bits" for "keystrokes" and "symbol" for "app," and that's
exactly the property we want — frequent apps should need only a character or
two, rare ones need more. The name `huffi` is a nod to this: not because
we're literally building a Huffman tree
(our codes should fuzzy match the applications), but
because the target *shape* of the system — short identifiers for frequent
targets, emerging adaptively from a live, non-stationary frequency
distribution — is the same problem adaptive Huffman coding solves in a
different domain.

## Architecture overview

Three things happen on every keystroke:

1. **Fuzzy match** every candidate app against the typed query → `text_score`
2. **Look up history** for the *exact* typed string in a prefix trie → `history_score` + `confidence`
3. **Blend** the two into a final ranking

Nothing is a hard filter except "does this even fuzzy-match at all." Ranking
is one continuous scoring function, not filter-then-sort.

## The prefix trie

Every node in the trie corresponds to a string the user has typed (or a
prefix of one). Each node stores, per app, a decayed score:

```
Node("fi")
  ├── Firefox:      { score: 12.4, last_update: t0 }
  ├── Finder:        { score: 3.1,  last_update: t1 }
  └── FirmwareTool:   { score: 0.2,  last_update: t2 }
```

The root node (`""`) is the degenerate case: pure global frequency, no
conditioning on typed text. This is what a launcher like rofi gives you
today — huffi treats it as the *least* informative node, not the primary
signal.

### Fan-out on write

When you type `fire` and launch Firefox, **every prefix** of what you typed
gets updated, not just the exact string:

```
"f", "fi", "fir", "fire"  → all get a Firefox score bump
```

This is the mechanism that lets the model learn "fi" without ever being
typed as an exact 2-character query on its own — it accumulates evidence any
time a longer string starting with "fi" resolves to Firefox. If you start
typing "fire" for a while during a period where "fi" is ambiguous, "fi"
quietly catches up in the background, and you can go back to typing less.

### Decay

Scores decay exponentially rather than being stored as raw historical
counts. Instead of keeping a log and resumming on every query, each
`(prefix, app)` pair stores a single running score plus the timestamp of its
last update, and decay is applied lazily:

```
score_new = score_stored * exp(-λ * (t_now - t_last_update)) + 1   // on launch
score_effective = score_stored * exp(-λ * (t_now - t_last_update)) // on query, no launch
```

`λ` is derived from a chosen half-life (e.g. 2 weeks: `λ = ln(2) / 14 days`).
This is the same trick used by frecency implementations like Mozilla's
`moz_inputhistory` and various frecency-based fuzzy finders — O(1) per
update, no background jobs, no unbounded history log.

## Confidence-weighted blending

This is the key piece that avoids naive prefix backoff (see "Why not
backoff" below). For a given typed query, the trie node may have little or
no data. Confidence in that node's history score should scale with how much
data it has:

```
confidence = n / (n + k)
combined_score = confidence * history_score + (1 - confidence) * text_score
```

- `n` = number of launches recorded at this *exact* node
- `k` = smoothing constant — how much evidence is needed before history is
  trusted over text relevance. Current choice: `k = 3`.

Behavior at the extremes:

- **Well-trained node** (`n` large, e.g. "f" → Firefox dozens of times):
  confidence → 1, history dominates, text match barely matters.
- **Untrained node** (`n` = 0, e.g. first time typing "fi"): confidence = 0,
  ranking is pure fuzzy text score. This is what lets "fi" surface something
  *other* than Firefox the first time you type it — nothing is borrowed from
  the "f" node's bias.
- **In between:** blend shifts gradually as the exact node accumulates its
  own evidence (via direct typing or fan-out from longer queries).

### Why not backoff to a parent node

An earlier design considered "if there's no data for the exact prefix,
borrow the nearest ancestor's top-ranked result instead" (i.e. classic
n-gram backoff). This is wrong for this use case: it reintroduces exactly the
bias the user typed more characters to escape. If "f" always means Firefox,
backing off to "f"'s top pick when "fi" is untrained just shows Firefox
anyway — defeating the purpose of typing "fi" at all. huffi uses a two-level
fallback instead of an N-level climb: use the exact node if it has data, else
fall through straight to the neutral text-score-only floor.

## Fuzzy text scoring

`text_score` is computed via the [`nucleo`](https://github.com/helix-editor/nucleo)
crate (the matcher built for the Helix editor), which implements fzf-style
consecutive-character and word-boundary bonus scoring with gap penalties.

## Adaptation / migration behavior

If a new app starts overtaking Firefox at the "f" node, no special-cased
"reduce confidence on backoff" rule is needed — decay plus fan-out already
produce the right adaptive behavior:

1. Every launch of the new app updates "f", so "f"'s ranking naturally
   flips once the new app's decayed score exceeds Firefox's.
2. The user starts typing "fi" (or "fire") to reliably get Firefox back.
3. Those events fan out to "fi" independently of what's happening at "f".
4. Once "fi" accumulates enough of its own evidence, confidence rises there
   and it reliably surfaces Firefox on its own — "fi" becomes Firefox's new
   home rather than a temporary workaround.

## Manual intervention: boost and remove

Correcting the model should be possible in the moment a wrong suggestion is
seen, not via a separate tool. Two operations, both scoped to the *exact*
currently-typed string only — no fan-out to shorter prefixes, since a
deliberate correction at "fi" shouldn't silently push an opinion onto "f":

- **Delete** — clears the association between the current string and the
  highlighted app.
- **Boost** — one-shot, not a repeatable nudge. Modeled as a synthetic
  launch with a large weight (10x a normal launch) applied to *both*
  the score and the sample count `n`, so confidence rises the same way it
  would from organic use, rather than being a special-cased override with
  different semantics from the rest of the model. Fan-out is skipped.

Both are available in the CLI, protocol, and UI.

## Open questions

- Tune `k` (or its KT/Laplace-derived equivalent) against real usage data
  rather than guessing up front.
- Consider logging "top suggestion shown but not selected" as a mild
  negative signal to speed up migration — deferred for v1, only worth
  adding if plain decay+fan-out proves too slow in practice.
- Boost weight (10x) is a guess — untested against how it actually feels
  in practice.

## Rejected design ideas

- **Time-of-day / sequence-based signals** — considered and explicitly
  rejected. The ranking should be predictable from the typed text alone,
  not from context the user can't see or control. Adding implicit context
  would make the model's behavior harder to reason about without clear
  benefit for the core use case.

- **Locking a string against future learning.** Two
different features hide under that name — freezing all future writes vs.
pinning the *displayed* result while still recording organic data
underneath — and the second requires maintaining and reconciling two
parallel beliefs about the same string. Given boost already uses a large
synthetic weight, decay means a boosted result should naturally stay
dominant unless real usage genuinely shifts enough to outweigh it — which
arguably is correct behavior (see the migration scenario above). Locking is
deferred until repeated re-boosting of the same string shows it's actually
needed, rather than built preemptively.