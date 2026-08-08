# Emacs conformance corpus

`emacs_conformance.json` is the shared input and observation format for the
Herdr TEXT-mode differential tests. Each case is executed twice:

1. `scripts/emacs_conformance.el` runs the key sequence in GNU Emacs started
   with `-Q`, `fundamental-mode`, a read-only buffer, and
   `transient-mark-mode`.
2. The Rust test `emacs_conformance_corpus_matches` runs the same key sequence
   through Herdr's production Emacs keymap and command dispatcher.

Cases may contain `steps`. Those assert the state after every completed Emacs
command, not only the final state. This catches transient differences such as
a mark being activated correctly and then deactivated by `M-w` even when the
final point happens to agree.

Run both sides with:

```bash
just emacs-conformance
```

## Record a real Emacs session

Put the pane excerpt in a plain-text fixture, then launch a clean GNU Emacs
with the recorder already armed:

```bash
just emacs-record tests/fixtures/emacs_trace_sample.txt /tmp/my-motion.json 0 0
```

The final two arguments are optional, zero-based row and column coordinates.
Use the buffer normally and press `C-c C-c` to save the trace and close that
dedicated Emacs. The JSON trace contains the exact source text plus every
completed command's canonical key sequence,
individual key event descriptions, command identity, and before/after state.
It is useful on its own when diagnosing a mismatch.

To turn a simple, non-minibuffer trace into a permanent parity test:

```bash
just emacs-conformance-import /tmp/my-motion.json my-motion
git diff -- tests/fixtures/emacs_conformance.json
just emacs-conformance
```

The import starts as an exact case on purpose. If it exposes a deliberate,
bounded Herdr difference, mark the affected step and final case as
`known-deviation`, add current `herdr` snapshots and precise `reason` fields,
then rerun the test. Nested minibuffer sessions are retained in diagnostic
traces but are not yet importable as flat corpus steps.

You can also load `scripts/emacs_trace.el` into an existing Emacs, visit any
buffer, and run `M-x herdr-emacs-trace-start`. This is convenient for an exact
piece of copied pane output; use a read-only buffer when the target is Herdr
TEXT mode.

## Recorded state model

The interactive trace stores a stable observable contract rather than private
redisplay objects:

- buffer identity: exact text, SHA-256, character/byte/line counts, major mode,
  read-only and modified flags;
- cursor identity: point as zero-based row/column plus character and byte
  offsets;
- selection identity: mark, effective transient-mark activity, ordered region,
  direction, and selected text;
- motion state: permanent and temporary goal columns;
- ring state: the complete kill ring, yank cursor, and buffer-local mark ring;
- buffer bounds: narrowing state and restriction endpoints;
- recursive UI state: minibuffer prompt/content/point and active isearch query,
  direction, and failure state;
- transition identity: canonical keys, constituent key event descriptions,
  Emacs command, nesting relationship, and complete before/after snapshots.

This deliberately excludes pixel-level redisplay, overlays, faces, and other
Emacs internals that Herdr TEXT mode neither exposes nor implements. Add an
observable field to the trace before implementing a new behavior so the GNU
Emacs result remains the oracle.

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
- Keep each fixture small enough for a deterministic unit test. The harness
  sizes its terminal to the excerpt so lines do not wrap and a trailing
  newline retains the same final empty line as the Emacs buffer.
- Use `"comparison": "exact"` when Herdr must equal the `emacs` snapshot.
- For a deliberate, bounded incompatibility, use
  `"comparison": "known-deviation"`, explain it in `reason`, and add the
  current `herdr` snapshot. Both snapshots remain asserted, so neither side
  can drift silently.
- Compare observable behavior, not Emacs implementation details or messages
  whose wording can change between releases.

The compact corpus currently asserts point, mark, region activation, and the
kill-ring head. The richer trace retains additional observations until Herdr
has corresponding state that can be asserted directly.
