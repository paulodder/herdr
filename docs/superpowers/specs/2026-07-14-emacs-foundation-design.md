# Emacs layer foundation — design spec

**Date:** 2026-07-14
**Status:** Approved design, pre-implementation
**Supersedes:** the Phase 2–4 sketches in `2026-07-13-emacs-layer-design.md` (that spec's Phase 0–1 shipped and stands; this spec replaces its plan for what comes next)
**Branch:** `emacs` on `paulodder/herdr`

## 1. Why this exists

Phase 1 shipped and the layer works — but using it surfaced two failures that look
unrelated and are not:

1. `C-x 3` (and every other global binding) does nothing inside TEXT mode.
2. `C-h` and `M-h` do nothing anywhere.

The first is an architectural defect. `src/app/input/emacs.rs:98` selects **one** keymap:

```rust
let lookup = if text_active { keymaps.text.lookup(&seq) } else { keymaps.global.lookup(&seq) };
```

Emacs does not do this. Emacs *layers* keymaps — the major-mode map overlays the global
map, and a sequence not found locally **falls through** to global. By making the choice
exclusive we made every global command unreachable from TEXT mode. One design decision,
an entire class of broken keys.

The second is not a defect but an absence: the layer defines 25 commands, and herdr's own
`NavigateAction` enum has **46 variants** — we expose 10 of them. Every new key today
costs an enum variant, a dispatch arm, and a table entry. That is a treadmill, and it is
the thing this spec exists to end.

**Goal:** make the layer's foundation match Emacs's actual architecture, so that adding a
binding is a config line and never a code change, and so that a key that does nothing
tells you why.

## 2. The contract we never named

Emacs already solved the "terminal inside an editor" problem, in `term-mode`:

| Emacs | herdr Emacs layer | Who owns the keyboard |
|---|---|---|
| `term-char-mode` | **live mode** (pane focused) | The pane. The layer steals only its own chords; everything else reaches the agent. |
| `term-line-mode` | **TEXT mode** (`C-x [`) | Emacs. The buffer is an ordinary read-only Emacs buffer; the **full keymap stack** applies. |

This is the mental model, and it is now binding on the implementation:

- **In TEXT mode, every global command works.** It is an Emacs buffer. `C-x 3` splitting a
  window from a read-only buffer is not an edge case — it is the whole point of a global map.
- **In live mode, the pane wins by default.** The layer owns `C-x`, `C-q`, `C-y`, `M-y`,
  `M-x`, and `C-u`; every other key belongs to the agent. `C-h` reaching Claude Code in live
  mode is correct behavior, not a bug.

The keys that "don't work" in live mode are the keys we *chose* not to steal. Naming this
contract is what makes that answerable instead of arbitrary.

## 3. Architecture

### 3.1 Keymap stack (fixes the bug class)

`KeymapSet` stops being `{global, text}` and becomes an ordered stack of active maps.

```rust
/// Maps consulted highest-priority first. Later maps are fallthrough.
fn active_maps(&self) -> impl Iterator<Item = &Keymap<EmacsCommand>>
// TEXT mode active -> [text, global]
// live mode        -> [global]
```

Lookup semantics, matching Emacs:

- Scan active maps in priority order. The **first exact `Bound`** wins.
- If no map binds the sequence but **any** map reports `Prefix`, the result is `Prefix`.
  (Prefix-ness is a union across the stack: `C-x` must stay a live prefix in TEXT mode
  even though the text map only binds `C-x C-x`, because the global map binds `C-x 3`.)
- Otherwise `Unbound`.

This is a change to `lookup`, `active_maps`, and the one call site. It restores every
global command inside TEXT mode and it makes future maps (isearch, minibuffer, a `C-h`
help map) compose by construction instead of by special case.

### 3.2 Key normalization (makes missing keys testable)

One canonical `Chord` per physical key, whatever the terminal encoding. The layer already
has `Chord::from_key`; it gains an explicit equivalence table and total coverage:

| Key | Legacy encoding | Kitty (Ghostty) | Canonical chord |
|---|---|---|---|
| `C-i` | byte 9 — indistinguishable from TAB | `105;5u` → `C-i` | legacy: `TAB` · kitty: `C-i` |
| `C-m` | byte 13 — indistinguishable from RET | `109;5u` → `C-m` | legacy: `RET` · kitty: `C-m` |
| `C-[` | byte 27 — indistinguishable from ESC | `91;5u` → `C-[` | legacy: `ESC` · kitty: `C-[` |
| `C-]` | byte 29 | `93;5u` | `C-]` (unambiguous in both) |
| `C-SPC` | byte 0 | `32;5u` | `C-SPC` |
| `C-h` | byte 8 | `104;5u` | `C-h` (distinct from `DEL`) |
| `DEL` | byte 127 | `127u` | `DEL` |

**The fold is one-directional and lossy only where the terminal is.** A legacy byte 27
becomes `ESC` because the terminal cannot tell us anything more. A kitty CSI-u event that
explicitly reports `Char('[') + CTRL` stays `C-[` — we must not collapse a distinction the
terminal *did* give us, because doing so would silently destroy any binding on `C-[`,
`C-i`, or `C-m`. Ghostty with the kitty protocol is the supported terminal, so these three
chords are bindable; on a legacy terminal they are physically unavailable and the layer
says so rather than pretending otherwise.

Canonicalization runs **once**, at the normalization boundary — never at lookup time and
never in the binding table.

**Coverage test (this is the deliverable that ends the guessing):** a table-driven test
over all 26 `C-<letter>`, all 26 `M-<letter>`, digits, the named keys, and both encodings,
asserting `parse_chord(s) == Chord::from_key(encode(s))` round-trips. A key that cannot
reach the layer becomes a failing test, not a mystery.

Documented consequence: on a terminal without the kitty protocol, `C-i`/`C-m`/`C-[` are
physically indistinguishable from `TAB`/`RET`/`ESC`. We do not paper over this. Ghostty is
the supported terminal (already a stated requirement).

### 3.3 Undefined keys must speak

Silence is why this felt broken rather than incomplete. Every Emacs-owned context echoes,
exactly like Emacs:

- Unbound sequence in TEXT mode → `C-h b is undefined`
- Unbound **multi-chord** sequence in live mode → `C-x z is undefined`
- Unbound **single** key in live mode → passes to the pane, silently (the agent owns it)

The read-only message stays where it belongs: only for keys that would *insert* into the
buffer (self-inserting characters), not as the catch-all for every unbound key — the
current code says "Buffer is read-only" for any unbound single chord in TEXT mode, which
is wrong and actively misleading.

### 3.4 The command surface stops being hand-maintained

This is the structural fix for one-off-ness.

```rust
pub enum EmacsCommand {
    Builtin(EmacsBuiltin),   // layer-native: motions, rings, TEXT mode, help, M-x, C-u
    Herdr(NavigateAction),   // every one of herdr's 46 existing actions
}
```

Every `NavigateAction` variant gets an Emacs-style command name in one table, and an
**exhaustive `match` over `NavigateAction`** enforces it. When upstream adds an action,
the fork fails to compile until it is named. The treadmill is replaced by a compiler error
— that is the guarantee, and it is the single most important line in this spec.

Naming follows Emacs where a real equivalent exists (`split-window-right`,
`other-window`, `delete-window`), and herdr's own vocabulary where it does not
(`toggle-sidebar`, `open-navigator`, `detach`).

### 3.5 M-x and the minibuffer

Once every command has a name, `M-x` makes every command reachable **without a binding**.
That is what makes "adding a key" optional rather than mandatory.

- One-line minibuffer in the echo area; editable with Emacs keys (`C-a/C-e/C-k/C-w/C-y`,
  `M-DEL`).
- `M-x` fuzzy-completes over the full command table (builtins + all 46 herdr actions).
- Vertical candidate list above the minibuffer (vertico-style), `C-n`/`C-p` to select,
  `TAB` to complete, `RET` to run, `C-g` to abort.
- The minibuffer is a keymap on the stack (§3.1), not a special case in the dispatcher.
- Available from **both** live mode and TEXT mode — `M-x` is one of the chords the layer
  owns in char-mode.

### 3.6 Prefix arguments

`C-u` (chainable: `C-u C-u` = 16), and `M-<digit>` for numeric args. The dispatcher already
threads `Option<i64>` into `execute_emacs_command` — this fills it in for real. Every
command receives it; motions repeat; `C-u C-SPC` pops the mark ring (already specified in
Phase 1 and currently unreachable).

### 3.7 Discoverability: the `C-h` help prefix

The cure for "which keys even exist?" is to be able to ask.

- `C-h b` — **describe-bindings**: every binding in the active keymap stack, rendered in a
  scrollable overlay, grouped by map (text / global), showing chord → command name.
- `C-h k` — **describe-key**: reads the next chord sequence, echoes the command it runs
  (or that it is undefined).
- Bound in the **text** map (and thus available in TEXT mode). In live mode the pane owns
  `C-h`, so help is reached with `F1` — which is also bound in both maps.

`C-h` is a prefix key, which the keymap stack already supports for free once §3.1 lands.
This is the one feature that turns the layer from "a set of keys Paul has to remember"
into something explorable.

## 3.8 New default bindings: tab navigation and tab reordering

```
C-[   previous-tab      C-]   next-tab        (move between tabs)
M-[   move-tab-left     M-]   move-tab-right  (move tabs around)
```

Two notes that matter:

- **`move-tab-left` / `move-tab-right` are new commands, not exposed actions.** herdr's
  `tab.move` API exists but is reachable only by mouse drag (`move_tab_via_api`) — there is
  no `NavigateAction` for it. So these are `EmacsBuiltin` commands that call the API
  directly, clamping at the ends (no wraparound: Emacs's `C-x ^`-family commands do not
  wrap, and a tab silently teleporting from last to first is a worse surprise than a no-op).
- **All four require the kitty keyboard protocol.** `C-[` is byte 27 on a legacy terminal
  (i.e. `ESC`), and `M-[` is the CSI introducer itself — both are unusable without kitty.
  Ghostty is the supported terminal, so this is fine, but the layer must not pretend
  otherwise: on a terminal without the protocol these bindings simply never fire, and
  `C-h b` shows them as bound (they *are* bound — the terminal just can't deliver them).

These are defaults; `[emacs.keys]` overrides them like any other binding.

## 4. Config surface

Unchanged in shape, larger in reach — every command name in §3.4 is now bindable:

```toml
[emacs]
enabled = true
clipboard_sync = true

[emacs.keys]
"C-x 4" = "split-window-right"    # any command, no recompile
"M-h"   = "mark-paragraph"
"C-x t" = "toggle-sidebar"        # a herdr action, exposed by name
```

Binding errors route through herdr's config diagnostics pipeline (a toast), not
`tracing::warn!` — currently they are invisible, which is the same silence failure as §3.3.

## 5. What this explicitly does not include

isearch (`C-s`/`C-r`), occur, and keyboard macros. They were Phase 2/4 in the original
spec and remain future work — but they are *cheap* after this: isearch is a keymap on the
stack plus a command, and a macro recorder is a hook at the normalization boundary. That
is the point of doing the foundation first.

`mark-paragraph` (`M-h`) and the rest of the standard editing commands are likewise not
enumerated here: after §3.4 and §3.5, adding one is a command plus a table row, and the
user can bind it themselves without waiting for a release.

## 6. Testing

- **Keymap stack:** fallthrough, priority (local shadows global), union prefix detection,
  and specifically `C-x 3` dispatching **from inside TEXT mode** — the regression that
  motivated this spec.
- **Key normalization:** the full round-trip coverage table (§3.2), both encodings.
- **Command surface:** exhaustive-match test proving every `NavigateAction` has a name;
  round-trip name → command → name.
- **Minibuffer/M-x:** completion ranking, editing keys, abort, execution of both a builtin
  and a herdr action by name.
- **Prefix args:** `C-u 5 C-f` moves 5; `C-u C-u` = 16; `C-u C-SPC` pops the mark ring.
- **Help:** `C-h b` lists a binding from *each* active map; `C-h k` names a command.
- **Regression:** `[emacs] enabled = false` remains bit-for-bit stock herdr.

Existing suite stays green (65 emacs tests today), and the 14 known upstream env-race
flakes remain the documented baseline.

## 7. Success criteria

1. `C-x [`, then `C-x 3` — the pane splits, from inside TEXT mode.
2. `C-h b` lists every active binding; `C-h k C-x 3` answers `split-window-right`.
3. `M-x toggle-sidebar` runs a herdr action that has no binding at all.
4. `C-u 5 C-f` moves point five characters.
5. Binding `"M-h" = "mark-paragraph"` in `[emacs.keys]` works without rebuilding.
6. A new upstream `NavigateAction` variant fails the build until it is named.
7. `[emacs] enabled = false` → stock herdr.
