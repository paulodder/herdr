# Emacs conformance corpus

`emacs_conformance.json` is the shared input and observation format for the
Herdr TEXT-mode differential tests. Each case is executed twice:

1. `scripts/emacs_conformance.el` runs the key sequence in GNU Emacs started
   with `-Q`, `fundamental-mode`, a read-only buffer, and
   `transient-mark-mode`.
2. The Rust test `emacs_conformance_corpus_matches` runs the same key sequence
   through Herdr's production Emacs keymap and command dispatcher.

Run both sides with:

```bash
just emacs-conformance
```

The normal Rust test suite checks Herdr against the committed GNU Emacs
snapshots without requiring Emacs to be installed. To deliberately refresh
the reference snapshots using the pinned Emacs version:

```bash
just emacs-conformance-update
git diff -- tests/fixtures/emacs_conformance.json
just emacs-conformance
```

## Adding a case

- Use canonical Emacs key notation accepted by both `kbd` and Herdr's chord
  parser.
- Keep fixture lines under the 40-column terminal width and use ten lines.
  That makes the Emacs buffer and Herdr's test terminal grid have identical
  boundaries.
- Use `"comparison": "exact"` when Herdr must equal the `emacs` snapshot.
- For a deliberate, bounded incompatibility, use
  `"comparison": "known-deviation"`, explain it in `reason`, and add the
  current `herdr` snapshot. Both snapshots remain asserted, so neither side
  can drift silently.
- Compare observable behavior, not Emacs implementation details or messages
  whose wording can change between releases.

The corpus currently observes point, mark, region activation, and the
kill-ring head. Extend the snapshot schema when isearch, minibuffer, prefix
arguments, or keyboard macros land.
