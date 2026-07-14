# Phase 0+1 follow-ups (from final whole-branch review, 2026-07-14)

Verdict at `3d1a5ac`: **Ready to merge — Yes.** These are the non-blocking items the
final reviewer triaged as follow-up tickets or pre-Phase-2 work.

## Manual gate before calling Phase 1 done (morning checklist)

1. Ghostty visual pass: reversed+bold point legibility; host cursor hides on `C-x [`
   and returns on `q`/`Esc`; smooth viewport follow on `C-v`/`M-v`; no key echo into
   the pane during TEXT mode; region shading boundary cases (mark==point, multi-row,
   viewport edges).
2. Wayland clipboard round-trip: `M-w` a region, paste elsewhere (`wl-paste`); with
   `clipboard_sync = true`, copy externally then `C-y` into a pane.
3. Live `C-y`/`M-y` into a real agent pane (Claude Code) — including the documented
   multi-line `M-y` backspace limitation.
4. `cargo clippy` (not installed in the overnight sandbox; install via rustup first).

## Follow-up tickets (batched)

**TEXT-mode lifecycle polish** (Minors 3–7 from final review):
- Disable-mid-TEXT-mode skips the scroll restore (`apply_config` drops text_mode directly).
- `mark_rings` never prunes closed panes (memory-trivial; dangling-state wart).
- `[emacs.keys]` errors only reach `tracing::warn!` — route through the config
  diagnostics/toast pipeline like upstream `[keys]` errors.
- Bind arrows/Home/End/PageUp/PageDown in the TEXT keymap (Emacs binds them all).
- Stale echo persists across mode changes (only cleared on next Terminal-mode press).

**Robustness/coverage:**
- `EmacsCommand::name()` → exhaustive match (current round-trip test iterates the
  table, so it cannot catch a missing variant).
- Override test for the Yank/YankPop dual-bind path in `build_keymaps`.
- Test for `emacs_region_text` end.col==0 branch (region ending at column 0).
- `emacs_send_key_to_focused_pane` discards `try_send_bytes` Result (upstream-consistent;
  add a debug log at minimum).
- Grapheme-vs-scalar backspace count in `M-y` erase — fold into the documented
  limitation note.

**Known upstream flakiness (not ours):** 14 env-race tests (detect::manifest*,
detect::manifest_update*, server::headless keybinding pair, settings_save_toast) fail
intermittently under full parallel load; all pass in isolation. Consider reporting
upstream.

## Phase 2 design note (budget before building isearch)

Scrollback pruning shifts absolute row addressing: if a pane keeps producing output and
ghostty prunes the oldest rows, TEXT-mode point/mark/mark-ring rows silently refer to
different content. Same class as the wide-char limitation, but it **compounds for
isearch** if match positions are stored as absolute rows — resolve the addressing
strategy before Phase 2 stores match positions.

## Cosmetic deviations from Emacs (accepted for Phase 1)

- Region rows between endpoints highlight the full pane width instead of stopping at EOL.
- `C-y` after `M-y` types the ring head, not the last-popped entry (Emacs rotates).
- Rebound `keyboard-quit` won't cancel a pending chord in live mode (mid-chord cancel is
  hardcoded to literal `C-g` — deliberate safety choice; documented asymmetry).
- Only key presses run the stale-TEXT-mode check; a mouse-focus change followed only by
  mouse activity leaves the point overlay visible until the next keypress (self-healing).
