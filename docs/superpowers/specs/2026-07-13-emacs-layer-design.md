# Emacs layer for herdr — design spec

**Date:** 2026-07-13
**Status:** Approved design, pre-implementation
**Repo posture:** `paulodder/herdr` fork of `ogulcancelik/herdr`; all work on the `emacs` branch, rebased onto `upstream/master` per upstream release.

## 1. Context and goal

herdr is a Rust terminal multiplexer for AI coding agents (workspaces → tabs → panes, client/server, own PTYs, ghostty-vt terminal state, ratatui rendering). Its keyboard idiom is tmux-style (prefix `ctrl+b`, vim-flavored copy mode, mouse-first UI).

The goal of this fork: make herdr **keyboard-native in the Emacs idiom** — tab/pane management on `C-x` chords, and a deep simulation of Emacs interaction semantics over pane scrollback and the app itself.

The predecessor project `~/projects/operator` (Go, charmbracelet) proved the concept: a TEXT mode with `C-x [`, mark/region, Emacs motions, `M-w` to clipboard. Operator is the **design donor only** — this fork is a fresh, deeper implementation in Rust. Where this spec and operator disagree, this spec wins; where this spec is silent on a key's behavior, real Emacs behavior wins.

## 2. Decisions already made

| Decision | Choice |
|---|---|
| Relationship to operator | Fork becomes the project; operator retired as standalone |
| Emacs depth | Full idiom: kill/mark rings, minibuffer + completion, isearch + occur, prefix args + kmacros |
| Fork posture | Personal build tracking upstream; surgical, rebasable diff |
| License context | Upstream is AGPL-3.0 dual-licensed; personal, non-distributed fork — no obligations triggered |

## 3. Architecture

### 3.1 One module, three seams

All new code lives in a self-contained `src/emacs/` module. It touches upstream code at exactly three seams, each a small, reviewable patch:

1. **Input interception** — a hook early in the client key-dispatch path (`src/input/`). When the Emacs layer is active for the focused context, it consumes the key event; otherwise the event flows to herdr untouched. This single hook powers the keymap engine, prefix args, and kmacro recording.
2. **Render overlay** — one overlay surface in the ratatui draw path for the minibuffer/echo area (bottom line) and for TEXT-mode/isearch highlights on the pane grid.
3. **Pane text/scrollback API** — read access to per-pane terminal state. Upstream already exposes `visible_text()` / `recent_unwrapped_text()` (`src/pane/terminal.rs`) and selection structures (`src/selection.rs`); we extend read-side access (line-addressable scrollback, match positions), not the data structures themselves.

Rebase strategy: upstream moves fast (multiple commits/day). Keeping the diff to `src/emacs/` + three thin seams is the survival mechanism. Any feature that would require broad edits across upstream files must be redesigned or dropped.

### 3.2 Module internals

```
src/emacs/
  mod.rs        — activation, seam glue
  keymap.rs     — chord parser (C-x, M-, C-u), keymap stack, dispatch
  commands.rs   — command table: name → impl (M-x namespace)
  text_mode.rs  — TEXT mode: point/mark, region, motions over scrollback
  rings.rs      — kill ring, mark ring, search ring
  isearch.rs    — incremental search state machine (+ regexp variant)
  occur.rs      — match-list view over scrollback
  minibuffer.rs — prompt/echo area, editable line, completion engine
  kmacro.rs     — key-event recorder/replayer
```

Everything is keyed off a single config switch: `[emacs] enabled = true` (the "one command"). With it off, the fork behaves as stock herdr.

### 3.3 The command table is the spine

Every capability is a **named command** (`other-window`, `isearch-forward`, `kill-ring-save`, …). Keymaps bind chords to command names; M-x invokes them by name; kmacros replay the keys that invoke them; prefix args are passed as an argument to every command. This mirrors Emacs's own architecture and means each new feature is: implement command, add default binding, done — M-x, macros, and C-u support come for free.

## 4. Feature specification

### Phase 0 — C-x management layer (config + activation)

- `prefix = "ctrl+x"`; when the Emacs layer is enabled the layer owns dispatch, so C-x chords are parsed natively rather than via herdr's prefix mode.
- Bindings: `C-x 2` split down, `C-x 3` split right, `C-x o` other-pane, `C-x 0` close pane, `C-x 1` zoom, `C-x b` switch tab (minibuffer-completed in Phase 3; herdr goto picker until then), `C-x c` new tab, `C-x n`/`C-x p` next/prev tab, `C-x k` close tab, `C-x w` workspace picker, `C-x [` TEXT mode, `C-q` quoted-insert (sends the next key chord literally to the pane — the escape hatch for a raw `C-x`, `C-s`, etc.).
- Fork operational guards: disable `herdr update` self-replacement and background version check in the fork build.
- **Trade-off (accepted):** panes never receive raw `C-x` (or other layer-owned chords) except via `C-q`.

### Phase 1 — TEXT mode + kill/mark rings

- `C-x [` freezes the pane view into TEXT mode: point appears, scrollback is a navigable plain-text buffer (read-only).
- Motions: `C-f/b/n/p`, `M-f/b` (word), `C-a/e`, `C-v/M-v` (page), `M-<`/`M->` (buffer ends), `M-g g` (goto line). Sentence motions (`M-a/e`) are omitted — terminal output has no sentences.
- Region: `C-SPC` set-mark (transient-mark visual), `C-x C-x` exchange point/mark, `C-g` deactivate.
- **Kill ring:** `M-w` (kill-ring-save), `C-w` (kill-region — buffer is read-only, so region text is pushed and region deactivates), `M-y` after a yank cycles the ring. Ring depth 60; head synced bidirectionally with the system clipboard (Wayland). `C-y` in TEXT mode signals "buffer is read-only" in the echo area, like Emacs.
- **Yank into panes:** in live mode, `C-y` types the kill-ring head into the focused pane's PTY (the way you hand scrollback text to an agent), `M-y` immediately after replaces it with the previous ring entry.
- **Mark ring:** every mark set pushes; `C-u C-SPC` pops and moves point; per-pane, depth 16.
- `q`/`Esc` exits back to the live cursor.

### Phase 2 — isearch + occur

- `C-s`/`C-r` incremental search over the full scrollback from point; live match highlighting; repeat to advance; `Enter` sets point (pushes old point on mark ring, Emacs-style); `C-g` aborts to origin; `C-s C-s` reuses last search (search ring, `M-p/M-n` history).
- `C-M-s`/`C-M-r` regexp isearch.
- `M-s o` occur: match-list rendered as a temporary overlay buffer; `n/p` or `C-n/C-p` move, `Enter` jumps TEXT mode point to that line.
- Isearch works from live mode too: `C-s` on a live pane enters TEXT mode implicitly.

### Phase 3 — minibuffer + M-x + completion

- One-line minibuffer in the echo-area overlay; editable with Emacs keys (C-a/e, C-k, M-DEL, C-y yanks from kill ring).
- `M-x` — fuzzy completion over the full command table (including herdr actions and plugin actions, auto-imported).
- `C-x b` — switch-tab with completion across workspaces:tabs; `C-x C-b` list.
- Completion UI: vertical candidate list above the minibuffer (ivy/vertico-style), `C-n/C-p` select, `TAB` complete.

### Phase 4 — prefix args + keyboard macros

- `C-u` universal argument (chainable: `C-u C-u` = 16), `M-<digit>` numeric args; delivered to every command via the command-table calling convention (motions repeat, `C-u C-SPC` already specified).
- `F3`/`F4` record/replay (also `C-x (` / `C-x )` / `C-x e`); records raw key events at the dispatch hook, so macros span Emacs commands *and* keys typed into panes; `C-u 10 F4` replays 10×.

## 5. Config surface

```toml
[emacs]
enabled = true          # the one command
clipboard_sync = true   # kill-ring head <-> system clipboard
kill_ring_max = 60
mark_ring_max = 16

[emacs.keys]            # override any default binding
# "C-x c" = "new-tab"
# "M-s o" = "occur"
```

Emacs-style key syntax (`C-`, `M-`, chords as space-separated sequences) in this section only; herdr's native `[keys]` syntax remains valid for non-Emacs setups.

## 6. Testing

- **Unit:** keymap/chord parser, rings, isearch state machine, minibuffer completion — pure logic, plain `cargo test` in `src/emacs/`.
- **Integration:** drive a headless server + scripted client input; assert via the socket API (`pane.read`, cursor/selection state). Upstream's test setup (justfile) to be adopted where it exists.
- **Smoke:** manual script mirroring the success criteria below, run before every rebase-onto-upstream.

## 7. Risks

| Risk | Mitigation |
|---|---|
| Upstream velocity → rebase pain | Single-module + three-seam discipline; smoke test after every rebase; isearch/copy-mode-keymap potentially offered upstream to shrink the diff |
| Key dispatch seam churns upstream | The one seam most likely to conflict; keep the hook ≤ ~20 lines and well-commented |
| Terminal key ambiguity (`C-M-s`, `M-<` under some terminals) | Require kitty-keyboard-protocol terminal (Ghostty — already Paul's terminal); document fallbacks |
| Rust ramp-up (coming from Go) | Phases are ordered easy→hard; Phase 0–1 touch narrow, well-understood code; implementation delegated to coding agents with this spec |
| `herdr update` overwrites fork | Disabled in fork build (Phase 0) |

## 8. Success criteria

From a fresh build of the fork, in one sitting:

1. `C-x c` new tab; `C-x n/p/b` navigate tabs; `C-x 2/3/o/0` manage panes.
2. `C-x [` on a Claude Code pane → TEXT mode; `C-SPC`, `M->`, `M-w` → region lands on the Wayland clipboard.
3. `C-s` finds text deep in scrollback; `M-s o` lists all matches; `Enter` jumps to one.
4. `M-x occur` works identically to `M-s o`.
5. `F3 … F4`, then `C-u 5 F4` replays a mixed sequence (Emacs commands + literal pane input) five times.
6. `[emacs] enabled = false` → bit-for-bit stock herdr behavior.
