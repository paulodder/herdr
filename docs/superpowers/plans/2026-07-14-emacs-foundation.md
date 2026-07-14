# Emacs Layer Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the Emacs layer's foundation on Emacs's real architecture — a layered keymap stack, a canonical key-normalization boundary, a command surface that covers every herdr action by name, prefix arguments, a minibuffer with `M-x`, and a `C-h` help prefix — so that adding a binding is a config line, never a code change, and an unbound key says so.

**Architecture:** `KeymapSet` stops being an either/or `{global, text}` choice and becomes an ordered *stack* of active maps (`active_maps(ctx)`), looked up with first-`Bound`-wins / union-`Prefix` semantics (`stack_lookup`). `EmacsCommand` splits into `Builtin(EmacsBuiltin)` (layer-native) and `Herdr(NavigateAction)` (all 45 of herdr's existing actions), with a macro-generated **exhaustive match** over `NavigateAction` so a new upstream variant fails the build until it is named. Everything else — prefix args, the minibuffer, `M-x`, help — is a keymap on the stack plus a command, exactly as the spec promises.

**Tech Stack:** Rust 2021, crossterm 0.29, ratatui 0.30, serde/toml, tokio; vendored libghostty-vt built via zig 0.15.2.

## Deviations from the design spec (discovered against real source)

The plan follows the spec's suggested decomposition. Four points were resolved against the code:

1. **`NavigateAction` has 45 variants, not 46.** Counted mechanically:
   `awk '/^pub\(crate\) enum NavigateAction \{/,/^\}/' src/app/input/navigate.rs | grep -c '^    [A-Z]'` → `45`. Every "46" in the spec means "all of them"; the plan says 45 and pins the count in a test constant.
2. **Three variants carry a payload:** `SwitchWorkspace(usize)`, `SwitchTab(usize)`, `FocusAgent(usize)`. A name alone cannot express the index, so those three commands take the index **from the prefix argument** (`C-u 2 M-x switch-tab` → tab index 1; the prefix arg is 1-based, the herdr index is 0-based). Default with no prefix arg is index 0. This is why Task 4 (command surface) depends on Task 7's `Option<i64>` calling convention already existing — it does, from Phase 1.
3. **`NavigateAction` is not reachable from `src/emacs/`.** `src/app/mod.rs:15` is `mod input;` and `src/app/input/mod.rs:41` is `mod navigate;` — both private. Task 4 widens exactly two words to `pub(crate) mod`, marked as fork seams. This is the only new upstream seam in the whole plan.
4. **The keymap-stack logic is made generic and lives in `keymap.rs`** (`stack_lookup<T: Copy>`), not on `KeymapSet`. That lets Task 1's pure tests use `Keymap<u8>` and therefore survive Task 4's `EmacsCommand` restructure untouched. `KeymapSet::active_maps` / `KeymapSet::lookup` are thin wrappers over it. `active_maps` keeps exactly the name the spec gives it (§3.1), in Tasks 1, 9 and 10.
5. **The §3.2 fold is already performed by the parser, and it is one-directional.** `src/input/parse.rs` matches `"\t"`, `"\r"` and `"\x1b"` BEFORE `parse_legacy_ctrl_char`, so a legacy byte 9/13/27 already arrives as `KeyCode::Tab`/`Enter`/`Esc` — nothing to fold. A kitty `\x1b[105;5u` arrives as `Char('i') + CONTROL` and **must stay `C-i`**: collapsing it would destroy a real, deliverable binding (and §3.8 binds `C-[` to `previous-tab`). `canonical_chord` is therefore the *boundary* (which key codes are bindable, plus the documented one-directional table), not a collapse function. Task 2 asserts both directions explicitly.

Two spec ambiguities resolved (see also the report at the end):

- **§3.5 lists `C-w` among the minibuffer editing keys.** `kill-region` in a minibuffer with no mark is inert. The minibuffer map therefore binds **both** `C-w` and `M-DEL` to `backward-kill-word` (the readline habit), and does not bind `kill-region`. Documented in Task 11's bindings reference.
- **§3.7's `C-h b` "grouped by map (text / global)"** — with the stack there can be three maps (minibuffer / text / global). `describe_bindings_lines` groups by whatever `active_maps(ctx)` returns, so it is correct in all three contexts by construction.

## Global Constraints

- **Toolchain env — every shell command in every task must export these first** (the repo's `rust-toolchain.toml` pins 1.96.1 but only 1.88.0 is installed; rustup shims are absent):
  ```bash
  export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
  export ZIG="$HOME/.local/opt/zig-x86_64-linux-0.15.2/zig"
  cd /home/paul/projects/herdr
  ```
- **Test command:** `cargo test --locked --bin herdr <filter>` — NEVER `just test`. No cargo-nextest, no clippy installed.
- **Known-flaky baseline (NOT your bug):** 14 pre-existing upstream env-race tests fail intermittently under full parallel load (`detect::manifest*`, `detect::manifest_update*`, a `server::headless` keybinding pair, `settings_save_toast`). They pass in isolation. Re-run any failure in isolation before treating it as a regression.
- **`docs/superpowers` is gitignored upstream** — every commit touching it needs `git add -f`. So is all of `docs/` except `docs/next/` (see `.gitignore`: `/docs/*`, `!/docs/next/`), so Task 11's `docs/emacs-layer.md` also needs `git add -f`.
- **Surgical fork discipline:** all new logic lives in `src/emacs/`. Upstream files are touched only at the existing seams, each marked `// Emacs layer seam (fork):`. The diff must stay rebasable onto a fast-moving upstream.
- **Config reference:** any new `[emacs]` config key must be added to BOTH config reference JSON files that upstream keeps in sync (`docs/next/website/src/data/config-reference.json` and `website/src/data/config-reference.json`; `scripts/config_reference_check.py` fails on drift).
- **`[emacs] enabled = false` must remain bit-for-bit stock herdr** — the existing test `app::input::emacs::tests::disabled_layer_is_bit_for_bit_passthrough` asserts this; it must keep passing.
- **TDD is mandatory** for every task: failing test first, then implementation.
- Branch: `emacs`. Run `cargo fmt` before every commit.
- Hard cutover — no backward compatibility, no config migration.

## File Structure

New files:

| File | Responsibility |
|---|---|
| `src/emacs/minibuffer.rs` | `MinibufferState`, pure line-editing functions, fuzzy candidate filtering |
| `src/emacs/help.rs` | `HelpOverlay`, `describe_bindings_lines` (pure) |
| `docs/emacs-layer.md` | User-facing bindings + command reference |

Modified fork-owned files:

| File | Edit |
|---|---|
| `src/emacs/keymap.rs` | canonicalization at the boundary; `stack_lookup`; `Keymap::bindings()` |
| `src/emacs/commands.rs` | `MapContext`, `ActiveMap`, `active_maps`, `KeymapSet::lookup`, `minibuffer` map; `EmacsBuiltin` + `EmacsCommand::{Builtin,Herdr}`; the `herdr_command_table!` macro |
| `src/emacs/mod.rs` | new `EmacsState` fields (prefix reader, minibuffer, help, describe-key) |
| `src/emacs/render.rs` | minibuffer + candidate list in the echo area; `render_help_overlay` |
| `src/app/input/emacs.rs` | the dispatcher: reader stages, stack lookup, feedback rules, executor arms |

Modified upstream files (the whole new rebase surface):

| File | Edit |
|---|---|
| `src/app/mod.rs` | `pub(crate) mod input;` (1 word); emacs diagnostics in `apply_live_config` |
| `src/app/input/mod.rs` | `pub(crate) mod navigate;` (1 word) |
| `src/config.rs` | `.chain(self.emacs.binding_diagnostics())` in `collect_diagnostics` |
| `src/ui.rs` | 1 call: `render_help_overlay` |
| `src/main.rs` | `DEFAULT_CONFIG` `[emacs]` comment refresh |
| `docs/next/website/src/data/config-reference.json`, `website/src/data/config-reference.json` | `emacs.keys` description refresh |

---

### Task 1: The keymap stack

The bug that motivated the whole spec: `src/app/input/emacs.rs:98` picks **one** keymap, so no global command is reachable from TEXT mode. Replace it with an ordered stack.

**Files:**
- Modify: `src/emacs/keymap.rs` (add `stack_lookup` + `Keymap::bindings`)
- Modify: `src/emacs/commands.rs` (add `MapContext`, `ActiveMap`, `minibuffer` map, `active_maps`, `KeymapSet::lookup`)
- Modify: `src/emacs/mod.rs` (add `EmacsState::map_context`)
- Modify: `src/app/input/emacs.rs` (dispatcher lines ~82-134)
- Test: inline `#[cfg(test)]` in all three

**Interfaces:**
- Produces (used by Tasks 3, 7, 8, 9, 10):
  - `keymap::stack_lookup<'a, T: Copy + 'a>(maps: impl Iterator<Item = &'a Keymap<T>>, seq: &[Chord]) -> Lookup<T>`
  - `keymap::Keymap::<T>::bindings(&self) -> &[(Vec<Chord>, T)]`
  - `commands::MapContext { Live, Text, Minibuffer }`
  - `commands::ActiveMap<'a> { name: &'static str, map: &'a Keymap<EmacsCommand> }`
  - `commands::KeymapSet::active_maps(&self, ctx: MapContext) -> Vec<ActiveMap<'_>>`
  - `commands::KeymapSet::lookup(&self, ctx: MapContext, seq: &[Chord]) -> Lookup<EmacsCommand>`
  - `commands::KeymapSet { global, text, minibuffer }`
  - `EmacsState::map_context(&self) -> MapContext`

- [ ] **Step 1: Write the failing stack-lookup test** — append to the `mod tests` at the bottom of `src/emacs/keymap.rs`:

```rust
    #[test]
    fn stack_lookup_prefers_the_first_map_and_unions_prefixes() {
        // `local` shadows `global` for C-x C-x, but C-x 3 only exists in
        // `global` — the exact case that was broken in TEXT mode.
        let mut local: Keymap<u8> = Keymap::default();
        local.bind(parse_key_seq("C-x C-x").unwrap(), 1);
        local.bind(parse_key_seq("C-f").unwrap(), 2);

        let mut global: Keymap<u8> = Keymap::default();
        global.bind(parse_key_seq("C-x 3").unwrap(), 3);
        global.bind(parse_key_seq("C-f").unwrap(), 4);

        let stack = || [&local, &global].into_iter();

        // First exact Bound wins: local shadows global on C-f.
        assert_eq!(
            stack_lookup(stack(), &parse_key_seq("C-f").unwrap()),
            Lookup::Bound(2)
        );
        // Fallthrough: C-x 3 is only in global.
        assert_eq!(
            stack_lookup(stack(), &parse_key_seq("C-x 3").unwrap()),
            Lookup::Bound(3)
        );
        assert_eq!(
            stack_lookup(stack(), &parse_key_seq("C-x C-x").unwrap()),
            Lookup::Bound(1)
        );
        // Prefix is a UNION across the stack: C-x stays live even though the
        // local map alone would also report Prefix — and it must stay live
        // when only the global map has a longer binding.
        assert_eq!(
            stack_lookup([&local].into_iter(), &parse_key_seq("C-x").unwrap()),
            Lookup::Prefix
        );
        assert_eq!(
            stack_lookup([&global].into_iter(), &parse_key_seq("C-x").unwrap()),
            Lookup::Prefix
        );
        assert_eq!(
            stack_lookup(stack(), &parse_key_seq("C-x").unwrap()),
            Lookup::Prefix
        );
        // Nothing anywhere.
        assert_eq!(
            stack_lookup(stack(), &parse_key_seq("C-z").unwrap()),
            Lookup::Unbound
        );
        // A Bound in an earlier map beats a Prefix in a later one.
        let mut shadow: Keymap<u8> = Keymap::default();
        shadow.bind(parse_key_seq("C-x").unwrap(), 9);
        assert_eq!(
            stack_lookup([&shadow, &global].into_iter(), &parse_key_seq("C-x").unwrap()),
            Lookup::Bound(9)
        );
    }

    #[test]
    fn keymap_exposes_its_bindings_for_describe_bindings() {
        let mut map: Keymap<u8> = Keymap::default();
        map.bind(parse_key_seq("C-x 3").unwrap(), 7);
        let bindings = map.bindings();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].0, parse_key_seq("C-x 3").unwrap());
        assert_eq!(bindings[0].1, 7);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::keymap`
Expected: compile error — `cannot find function stack_lookup`, `no method named bindings`

- [ ] **Step 3: Implement `stack_lookup` and `bindings`** in `src/emacs/keymap.rs`. Add `bindings()` inside the existing `impl<T: Copy> Keymap<T>` block, directly after `lookup`:

```rust
    /// All bindings in insertion order. Used by `describe-bindings`.
    pub fn bindings(&self) -> &[(Vec<Chord>, T)] {
        &self.bindings
    }
```

Then add this free function directly below that `impl` block (above `#[cfg(test)]`):

```rust
/// Look a sequence up across an ordered stack of keymaps, with Emacs's
/// semantics (spec §3.1):
///
/// - the **first exact `Bound`** in priority order wins — an earlier map
///   shadows a later one;
/// - if nothing binds the sequence but **any** map reports `Prefix`, the
///   result is `Prefix` (prefix-ness is a union: `C-x` must stay a live
///   prefix in TEXT mode because the *global* map binds `C-x 3`, even
///   though the text map only binds `C-x C-x`);
/// - otherwise `Unbound`.
pub fn stack_lookup<'a, T: Copy + 'a>(
    maps: impl Iterator<Item = &'a Keymap<T>>,
    seq: &[Chord],
) -> Lookup<T> {
    let mut is_prefix = false;
    for map in maps {
        match map.lookup(seq) {
            Lookup::Bound(value) => return Lookup::Bound(value),
            Lookup::Prefix => is_prefix = true,
            Lookup::Unbound => {}
        }
    }
    if is_prefix {
        Lookup::Prefix
    } else {
        Lookup::Unbound
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::keymap`
Expected: `test result: ok.` (7 passed)

- [ ] **Step 5: Write the failing `active_maps` test** — append to `mod tests` at the bottom of `src/emacs/commands.rs`:

```rust
    #[test]
    fn active_maps_stack_by_context() {
        let (keymaps, _) = build_keymaps(&Default::default());
        let names = |ctx| {
            keymaps
                .active_maps(ctx)
                .into_iter()
                .map(|active| active.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(MapContext::Live), vec!["global"]);
        assert_eq!(names(MapContext::Text), vec!["text", "global"]);
        assert_eq!(names(MapContext::Minibuffer), vec!["minibuffer", "global"]);
    }

    #[test]
    fn global_bindings_fall_through_in_text_mode() {
        let (keymaps, _) = build_keymaps(&Default::default());
        // THE regression: C-x 3 must dispatch from inside TEXT mode.
        assert_eq!(
            keymaps.lookup(MapContext::Text, &parse_key_seq("C-x 3").unwrap()),
            Lookup::Bound(EmacsCommand::SplitWindowRight)
        );
        // ...while the text map still shadows on C-x C-x.
        assert_eq!(
            keymaps.lookup(MapContext::Text, &parse_key_seq("C-x C-x").unwrap()),
            Lookup::Bound(EmacsCommand::ExchangePointAndMark)
        );
        // C-x stays a live prefix in TEXT mode (union prefix).
        assert_eq!(
            keymaps.lookup(MapContext::Text, &parse_key_seq("C-x").unwrap()),
            Lookup::Prefix
        );
        // Text-only motions are not reachable in live mode.
        assert_eq!(
            keymaps.lookup(MapContext::Live, &parse_key_seq("C-f").unwrap()),
            Lookup::Unbound
        );
    }
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::commands`
Expected: compile error — `cannot find type MapContext`, `no method named active_maps`/`lookup`

- [ ] **Step 7: Implement the stack on `KeymapSet`** in `src/emacs/commands.rs`. Change the `use` line at the top from

```rust
use super::keymap::{parse_key_seq, Keymap};
```

to

```rust
use super::keymap::{parse_key_seq, stack_lookup, Chord, Keymap, Lookup};
```

Replace the whole `KeymapSet` struct declaration with:

```rust
/// Which keymaps are active. Derived from `EmacsState` (see
/// `EmacsState::map_context`), never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapContext {
    /// Pane focused, TEXT mode off: the pane owns every key we do not steal.
    Live,
    /// TEXT mode: an ordinary read-only Emacs buffer — the full stack applies.
    Text,
    /// A minibuffer prompt is open.
    Minibuffer,
}

/// One entry of the active keymap stack: a display name (for
/// `describe-bindings`) and the map itself.
#[derive(Debug, Clone, Copy)]
pub struct ActiveMap<'a> {
    pub name: &'static str,
    pub map: &'a Keymap<EmacsCommand>,
}

#[derive(Debug, Clone, Default)]
pub struct KeymapSet {
    /// Always active. In TEXT mode and in the minibuffer it is the
    /// fallthrough map, exactly like Emacs's `global-map`.
    pub global: Keymap<EmacsCommand>,
    /// Active while TEXT mode is on (shadows `global`).
    pub text: Keymap<EmacsCommand>,
    /// Active while a minibuffer prompt is open (shadows `global`).
    pub minibuffer: Keymap<EmacsCommand>,
}

impl KeymapSet {
    /// The active keymap stack, highest priority first (spec §3.1).
    /// Emacs's minibuffer does not inherit the previous buffer's local map,
    /// so `Minibuffer` is `[minibuffer, global]`, not `[minibuffer, text, global]`.
    pub fn active_maps(&self, ctx: MapContext) -> Vec<ActiveMap<'_>> {
        let global = ActiveMap {
            name: "global",
            map: &self.global,
        };
        match ctx {
            MapContext::Live => vec![global],
            MapContext::Text => vec![
                ActiveMap {
                    name: "text",
                    map: &self.text,
                },
                global,
            ],
            MapContext::Minibuffer => vec![
                ActiveMap {
                    name: "minibuffer",
                    map: &self.minibuffer,
                },
                global,
            ],
        }
    }

    /// Look `seq` up across the active stack.
    pub fn lookup(&self, ctx: MapContext, seq: &[Chord]) -> Lookup<EmacsCommand> {
        stack_lookup(
            self.active_maps(ctx).into_iter().map(|active| active.map),
            seq,
        )
    }
}
```

(The `minibuffer` map stays empty until Task 8 fills `DEFAULT_MINIBUFFER_BINDINGS`; `KeymapSet::default()` already gives it an empty `Keymap`.)

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::commands`
Expected: `test result: ok.` (8 passed)

- [ ] **Step 9: Write the failing dispatcher tests** — append to `mod tests` at the bottom of `src/app/input/emacs.rs`:

```rust
    /// THE regression from the spec (§7.1): C-x [ then C-x 3 must split the
    /// window from inside TEXT mode. Before the keymap stack, TEXT mode
    /// consulted only the text keymap, so every global command was dead.
    #[tokio::test]
    async fn c_x_3_splits_the_window_from_inside_text_mode() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        assert_eq!(app.state.workspaces[0].tabs[0].panes.len(), 1);
        enter_text_mode(&mut app);
        assert!(app.state.emacs.text_mode.is_some());

        app.route_client_input(vec![0x18, b'3']); // C-x 3

        assert_eq!(
            app.state.workspaces[0].tabs[0].panes.len(),
            2,
            "global split-window-right fell through from the text keymap"
        );
        assert_ne!(
            app.state.emacs.echo.as_deref(),
            Some("C-x 3 is undefined"),
            "the sequence must not be reported undefined"
        );
    }

    /// Fallthrough for a pure-state action (no PTY spawn): C-x b in TEXT
    /// mode opens the navigator, the same way it does in live mode.
    #[tokio::test]
    async fn c_x_b_opens_the_navigator_from_text_mode() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x18, b'b']); // C-x b
        assert_eq!(app.state.mode, Mode::Navigator);
        assert!(sent_bytes(&mut rx).is_empty());
    }

    /// The text map still shadows the global one on the same sequence.
    #[tokio::test]
    async fn text_map_shadows_global_on_c_x_c_x() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        app.route_client_input(vec![0x00]); // C-SPC
        app.route_client_input(vec![0x0e]); // C-n
        app.route_client_input(vec![0x18, 0x18]); // C-x C-x
        let text = app.state.emacs.text_mode.as_ref().expect("still in TEXT");
        assert_eq!((text.point.row, text.point.col), (0, 0), "point <-> mark");
    }

    /// C-q is a global command, so the stack now reaches it in TEXT mode.
    /// A read-only buffer cannot quote-insert: say so instead of pushing a
    /// literal byte into the PTY behind the frozen view.
    #[tokio::test]
    async fn quoted_insert_in_text_mode_is_read_only() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x11]); // C-q
        assert!(!app.state.emacs.quoted_insert);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Buffer is read-only"));
        app.route_client_input(vec![0x18]); // C-x: still a prefix, not a literal
        assert_eq!(app.state.emacs.pending.len(), 1);
        assert!(sent_bytes(&mut rx).is_empty());
    }

    /// C-x [ while TEXT mode is already on must not re-seed the session
    /// (it would clobber entry_offset_from_bottom and lose the point).
    #[tokio::test]
    async fn re_entering_text_mode_is_a_no_op() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(&fifty_lines());
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<: point to row 0
        let before = app.state.emacs.text_mode.as_ref().unwrap().clone_for_test();
        app.route_client_input(vec![0x18, b'[']); // C-x [ again
        let after = app.state.emacs.text_mode.as_ref().unwrap().clone_for_test();
        assert_eq!(after, before, "TEXT mode session untouched");
    }
```

`clone_for_test` does not exist yet. Add it to `TextModeState` in `src/emacs/text_mode.rs`, directly after the struct declaration (find `pub struct TextModeState`):

```rust
impl TextModeState {
    /// Test helper: a comparable snapshot of the session identity.
    #[cfg(test)]
    pub fn clone_for_test(&self) -> (crate::layout::PaneId, Pos, Option<Pos>, bool, usize) {
        (
            self.pane_id,
            self.point,
            self.mark,
            self.mark_active,
            self.entry_offset_from_bottom,
        )
    }
}
```

- [ ] **Step 10: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `c_x_3_splits_the_window_from_inside_text_mode` FAILS (`panes.len()` is 1 — the sequence was swallowed as undefined), `c_x_b_opens_the_navigator_from_text_mode` FAILS (mode is still `Terminal`), `quoted_insert_in_text_mode_is_read_only` FAILS (C-q unreachable in TEXT mode → "Buffer is read-only" comes from the old catch-all but `quoted_insert` stays false and the echo happens to match; if it passes, that is the old wrong catch-all — Task 3 removes it), `re_entering_text_mode_is_a_no_op` FAILS.

- [ ] **Step 11: Add `map_context` to `EmacsState`** in `src/emacs/mod.rs`. Add to the `use` block at the top:

```rust
use commands::{KeymapSet, MapContext};
```

(replacing `use commands::KeymapSet;`), and add this method inside `impl EmacsState`, directly after `apply_config`:

```rust
    /// Which keymap stack is active right now (spec §3.1).
    pub fn map_context(&self) -> MapContext {
        if self.text_mode.is_some() {
            MapContext::Text
        } else {
            MapContext::Live
        }
    }
```

(Task 8 adds the `Minibuffer` arm.)

- [ ] **Step 12: Rewrite the dispatcher core** in `src/app/input/emacs.rs`. Change the import line

```rust
use crate::emacs::keymap::{format_seq, Chord, Lookup};
```

to

```rust
use crate::emacs::commands::MapContext;
use crate::emacs::keymap::{format_seq, Chord, Lookup};
```

Replace lines 82-134 (from `let text_active = ...` through the closing brace of the `match lookup { ... }`) with:

```rust
        let ctx = self.state.emacs.map_context();
        let text_active = ctx == MapContext::Text;

        let Some(chord) = Chord::from_key(&key) else {
            return text_active;
        };

        // C-g always cancels an in-flight chord (and, in TEXT mode,
        // deactivates the mark). Delegates to KeyboardQuit so mid-chord quit
        // and bound quit behave identically.
        if !self.state.emacs.pending.is_empty() && chord == Chord::ctrl('g') {
            self.execute_emacs_command(EmacsCommand::KeyboardQuit, None);
            return true;
        }

        let mut seq = self.state.emacs.pending.clone();
        seq.push(chord);
        // Emacs layer seam (fork): the ordered keymap stack, not an
        // either/or choice — a sequence unbound in the local map falls
        // through to global. This is what makes C-x 3 work in TEXT mode.
        match self.state.emacs.keymaps.lookup(ctx, &seq) {
            Lookup::Bound(cmd) => {
                self.state.emacs.pending.clear();
                self.execute_emacs_command(cmd, None);
                true
            }
            Lookup::Prefix => {
                self.state.emacs.echo = Some(format!("{}-", format_seq(&seq)));
                self.state.emacs.pending = seq;
                true
            }
            Lookup::Unbound => {
                self.state.emacs.pending.clear();
                if text_active {
                    // Read-only buffer: swallow everything, explain like Emacs.
                    self.state.emacs.echo = if seq.len() > 1 {
                        Some(format!("{} is undefined", format_seq(&seq)))
                    } else {
                        Some("Buffer is read-only".to_string())
                    };
                    true
                } else if seq.len() == 1 {
                    // Plain unbound key in live mode: flows to the pane.
                    self.state.emacs.last_yank = None;
                    false
                } else {
                    self.state.emacs.last_yank = None;
                    self.state.emacs.echo = Some(format!("{} is undefined", format_seq(&seq)));
                    true
                }
            }
        }
```

(The `Unbound` arm is still the old, wrong catch-all. Task 3 fixes it — one task, one deliverable.)

Also update `emacs_would_consume` (same file) to use the stack, so repeat/release events agree with press events. Replace its body:

```rust
    /// Press-equivalent consume decision, used for repeat/release events.
    fn emacs_would_consume(&self, key: TerminalKey) -> bool {
        if self.state.emacs.text_mode.is_some() {
            return true;
        }
        let emacs = &self.state.emacs;
        if emacs.quoted_insert || !emacs.pending.is_empty() {
            return true;
        }
        match Chord::from_key(&key) {
            Some(chord) => !matches!(
                emacs.keymaps.lookup(emacs.map_context(), &[chord]),
                Lookup::Unbound
            ),
            None => false,
        }
    }
```

- [ ] **Step 13: Make `quoted-insert` and `text-mode` read-only-aware** in `execute_emacs_command` (same file). Replace the `EmacsCommand::QuotedInsert` arm:

```rust
            EmacsCommand::QuotedInsert => {
                if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                } else {
                    self.state.emacs.quoted_insert = true;
                    self.state.emacs.echo = Some("C-q-".to_string());
                }
            }
```

and replace the `EmacsCommand::TextMode` arm:

```rust
            EmacsCommand::TextMode => {
                if self.state.emacs.text_mode.is_none() {
                    self.emacs_enter_text_mode();
                }
            }
```

- [ ] **Step 14: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `test result: ok.` — all pre-existing emacs adapter tests plus the 5 new ones

Run: `cargo test --locked --bin herdr emacs::`
Expected: `test result: ok.`

- [ ] **Step 15: Commit**

```bash
cargo fmt
git add src/emacs/keymap.rs src/emacs/commands.rs src/emacs/mod.rs src/emacs/text_mode.rs src/app/input/emacs.rs
git commit -m "fix: layer emacs keymaps into an ordered stack so global commands work in TEXT mode"
```

---

### Task 2: Key normalization at the boundary

One canonical `Chord` per physical key, whatever the terminal encoding (spec §3.2). Normalization happens **once**, in `Chord::from_key` and `parse_chord` — never at lookup time, never in the binding table.

**The fold is one-directional, and the parser already performs it.** `src/input/parse.rs` matches `"\t"` / `"\r"` / `"\x1b"` *before* `parse_legacy_ctrl_char`, so a legacy byte 9/13/27 arrives as `KeyCode::Tab` / `Enter` / `Esc` — the terminal lost the distinction and so do we. A kitty `\x1b[105;5u` arrives as `Char('i') + CONTROL` and **stays `C-i`**: the terminal gave us the distinction and collapsing it would silently destroy a real binding (§3.8 binds `C-[`). So `canonical_chord` is the *boundary* — which key codes are bindable at all — and the documented home of the one-directional table. It must NOT collapse `C-i`/`C-m`/`C-[`.

**Files:**
- Modify: `src/emacs/keymap.rs` (`Chord::from_key`, `named_key`, `parse_chord`, `format_chord`)
- Test: inline `#[cfg(test)]` in `src/emacs/keymap.rs`

**Interfaces:**
- Consumes: `crate::input::parse_terminal_key_sequence(&str) -> Option<TerminalKey>` (`src/input/mod.rs`), the production byte→key decoder used by `src/raw_input.rs`.
- Produces:
  - `keymap::canonical_chord(ctrl: bool, meta: bool, code: KeyCode) -> Option<Chord>` — the single normalization boundary, used by both `Chord::from_key` and `parse_chord`
  - `Chord::is_self_insert(&self) -> bool`, `Chord::self_insert_char(&self) -> Option<char>`

- [ ] **Step 1: Write the failing coverage test** — append to `mod tests` in `src/emacs/keymap.rs`:

```rust
    use crate::input::parse_terminal_key_sequence;

    /// The one true assertion of §3.2: whatever the terminal sends for a
    /// key, `Chord::from_key` must produce the same chord that
    /// `parse_chord` produces for that key's Emacs name.
    fn assert_encodes_to(encoding: &str, chord_name: &str) {
        let key = parse_terminal_key_sequence(encoding)
            .unwrap_or_else(|| panic!("{encoding:?} must decode to a key"));
        let expected =
            parse_chord(chord_name).unwrap_or_else(|| panic!("{chord_name} must parse"));
        assert_eq!(
            Chord::from_key(&key),
            Some(expected),
            "{encoding:?} should normalize to {chord_name}"
        );
    }

    #[test]
    fn all_control_letters_normalize_in_both_encodings() {
        for c in 'a'..='z' {
            let name = format!("C-{c}");
            // Kitty (Ghostty): CSI <codepoint> ; 5 u  (5 = 1 + CONTROL).
            // The terminal DID give us the distinction: C-i stays C-i.
            assert_encodes_to(&format!("\x1b[{};5u", c as u32), &name);
            // Legacy: the C0 control byte. The terminal could NOT give us the
            // distinction for C-i / C-m — those bytes ARE TAB / RET.
            let byte = char::from_u32(c as u32 - 96).unwrap();
            let legacy = match c {
                'i' => "TAB".to_string(),
                'm' => "RET".to_string(),
                _ => name.clone(),
            };
            assert_encodes_to(&byte.to_string(), &legacy);
        }
    }

    #[test]
    fn all_meta_letters_normalize_in_both_encodings() {
        for c in 'a'..='z' {
            let name = format!("M-{c}");
            // Kitty: modifier 3 = 1 + ALT.
            assert_encodes_to(&format!("\x1b[{};3u", c as u32), &name);
            // Legacy: ESC prefix.
            assert_encodes_to(&format!("\x1b{c}"), &name);
        }
    }

    #[test]
    fn digits_normalize_plain_and_meta() {
        for c in '0'..='9' {
            assert_encodes_to(&c.to_string(), &c.to_string());
            assert_encodes_to(&format!("\x1b{c}"), &format!("M-{c}"));
            assert_encodes_to(&format!("\x1b[{};3u", c as u32), &format!("M-{c}"));
        }
    }

    /// Spec §3.2: **the fold is one-directional.** A legacy byte 27 becomes
    /// ESC because the terminal cannot tell us more. A kitty CSI-u event
    /// that explicitly reports `Char('[') + CTRL` STAYS `C-[` — collapsing
    /// it would destroy the §3.8 `C-[` = previous-tab binding.
    #[test]
    fn the_lossy_fold_is_one_directional() {
        // C-i vs TAB
        assert_encodes_to("\t", "TAB"); // legacy byte 9: lossy
        assert_encodes_to("\x1b[9u", "TAB"); // kitty TAB
        assert_encodes_to("\x1b[105;5u", "C-i"); // kitty C-i: PRESERVED
        assert_ne!(
            parse_chord("C-i"),
            parse_chord("TAB"),
            "C-i and TAB are different chords"
        );

        // C-m vs RET
        assert_encodes_to("\r", "RET"); // legacy byte 13: lossy
        assert_encodes_to("\x1b[13u", "RET"); // kitty RET
        assert_encodes_to("\x1b[109;5u", "C-m"); // kitty C-m: PRESERVED
        assert_ne!(parse_chord("C-m"), parse_chord("RET"));

        // C-[ vs ESC — the one that §3.8 depends on.
        assert_encodes_to("\x1b", "ESC"); // legacy byte 27: lossy
        assert_encodes_to("\x1b[27u", "ESC"); // kitty ESC
        assert_encodes_to("\x1b[91;5u", "C-["); // kitty C-[: PRESERVED
        assert_ne!(
            parse_chord("C-["),
            parse_chord("ESC"),
            "a binding on C-[ must not be destroyed by ESC"
        );
    }

    /// The rest of the §3.2 table: keys that are unambiguous in BOTH encodings.
    #[test]
    fn unambiguous_named_keys_agree_across_encodings() {
        // C-] — unambiguous in both (byte 29).
        assert_encodes_to("\u{1d}", "C-]");
        assert_encodes_to("\x1b[93;5u", "C-]");

        // C-SPC
        assert_encodes_to("\u{0}", "C-SPC");
        assert_encodes_to("\x1b[32;5u", "C-SPC");

        // C-h stays C-h and is DISTINCT from DEL
        assert_encodes_to("\u{8}", "C-h");
        assert_encodes_to("\x1b[104;5u", "C-h");
        assert_ne!(parse_chord("C-h"), parse_chord("DEL"));

        // DEL
        assert_encodes_to("\u{7f}", "DEL");
        assert_encodes_to("\x1b[127u", "DEL");

        // M-DEL (backward-kill-word)
        assert_encodes_to("\x1b\u{7f}", "M-DEL");
        assert_encodes_to("\x1b[127;3u", "M-DEL");

        // M-[ / M-] (§3.8) are kitty-only: on a legacy terminal ESC-[ is the
        // CSI introducer and ESC-] is the OSC introducer.
        assert_encodes_to("\x1b[91;3u", "M-[");
        assert_encodes_to("\x1b[93;3u", "M-]");

        // F1 (the live-mode help entry point)
        assert_encodes_to("\x1bOP", "F1");
        assert_encodes_to("\x1b[11~", "F1");
    }

    #[test]
    fn canonical_names_round_trip_through_format() {
        for name in [
            "C-x", "M-x", "C-SPC", "TAB", "RET", "ESC", "DEL", "M-DEL", "C-h", "C-i", "C-m",
            "C-[", "C-]", "M-[", "M-]", "F1", "M-<", "3",
        ] {
            let chord = parse_chord(name).unwrap_or_else(|| panic!("{name} parses"));
            assert_eq!(format_seq(&[chord]), name, "{name} round-trips");
        }
    }

    #[test]
    fn self_insert_is_a_bare_printable_character() {
        assert!(parse_chord("x").unwrap().is_self_insert());
        assert!(parse_chord("3").unwrap().is_self_insert());
        assert!(parse_chord("SPC").unwrap().is_self_insert());
        assert_eq!(parse_chord("x").unwrap().self_insert_char(), Some('x'));
        assert!(!parse_chord("C-x").unwrap().is_self_insert());
        assert!(!parse_chord("M-x").unwrap().is_self_insert());
        assert!(!parse_chord("RET").unwrap().is_self_insert());
        assert!(!parse_chord("DEL").unwrap().is_self_insert());
        assert!(!parse_chord("F1").unwrap().is_self_insert());
        assert_eq!(parse_chord("RET").unwrap().self_insert_char(), None);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::keymap`
Expected: FAIL — `parse_chord("F1")` returns `None` (`named_key` has no `F<n>`), `format_seq` renders `F(1)` via the `{other:?}` fallback, and `is_self_insert` does not exist. The `the_lossy_fold_is_one_directional` test should PASS on the existing `from_key` (it already preserves `C-i`); it is here to lock that behavior down so a future "canonicalization" refactor cannot break it.

- [ ] **Step 3: Implement the normalization boundary** in `src/emacs/keymap.rs`. Replace the whole `impl Chord { ... }` block and the `named_key` function with:

```rust
impl Chord {
    pub fn ctrl(c: char) -> Self {
        Self {
            ctrl: true,
            meta: false,
            code: KeyCode::Char(c),
        }
    }

    /// Normalize a decoded terminal key into a canonical chord. Returns
    /// `None` for keys the layer never binds (media keys, modifier-only
    /// events, ...).
    ///
    /// This and `parse_chord` are the ONLY normalization points — see
    /// `canonical_chord`.
    pub fn from_key(key: &TerminalKey) -> Option<Self> {
        canonical_chord(
            key.modifiers.contains(KeyModifiers::CONTROL),
            key.modifiers.contains(KeyModifiers::ALT),
            key.code,
        )
    }

    /// True for a chord that would insert a character into a buffer: a bare
    /// printable character with no CTRL/META. Spec §3.3 — this is the only
    /// thing that may report "Buffer is read-only".
    pub fn is_self_insert(&self) -> bool {
        !self.ctrl
            && !self.meta
            && matches!(self.code, KeyCode::Char(c) if !c.is_control())
    }

    /// The character this chord inserts, if it is self-inserting.
    pub fn self_insert_char(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(c) if self.is_self_insert() => Some(c),
            _ => None,
        }
    }
}

/// The single normalization boundary (spec §3.2). Decides which key codes
/// the layer can bind at all, and is the documented home of the
/// **one-directional** encoding table:
///
/// | Key    | Legacy                          | Kitty (Ghostty) | Chord            |
/// |--------|---------------------------------|-----------------|------------------|
/// | `C-i`  | byte 9 — same byte as TAB       | `105;5u`        | legacy `TAB` · kitty `C-i` |
/// | `C-m`  | byte 13 — same byte as RET      | `109;5u`        | legacy `RET` · kitty `C-m` |
/// | `C-[`  | byte 27 — same byte as ESC      | `91;5u`         | legacy `ESC` · kitty `C-[` |
/// | `C-]`  | byte 29                         | `93;5u`         | `C-]` (both)     |
/// | `C-SPC`| byte 0                          | `32;5u`         | `C-SPC` (both)   |
/// | `C-h`  | byte 8                          | `104;5u`        | `C-h` (both)     |
/// | `DEL`  | byte 127                        | `127u`          | `DEL` (both)     |
///
/// **There is no collapse here, by design.** `src/input/parse.rs` matches
/// `"\t"` / `"\r"` / `"\x1b"` before `parse_legacy_ctrl_char`, so a legacy
/// byte 9/13/27 already arrives as `Tab`/`Enter`/`Esc` — the terminal lost
/// the distinction, so we do too. A kitty CSI-u event that explicitly
/// reports `Char('[') + CONTROL` must STAY `C-[`: collapsing a distinction
/// the terminal DID give us would silently destroy the §3.8 binding on
/// `C-[`. Ghostty with the kitty protocol is the supported terminal; on a
/// legacy terminal `C-i`/`C-m`/`C-[` are physically unavailable and the
/// layer says so (`C-h b` still lists them — they *are* bound; the terminal
/// just cannot deliver them) rather than pretending otherwise.
pub fn canonical_chord(ctrl: bool, meta: bool, code: KeyCode) -> Option<Chord> {
    match code {
        KeyCode::Char(_)
        | KeyCode::Esc
        | KeyCode::Enter
        | KeyCode::Tab
        | KeyCode::Backspace
        | KeyCode::Delete
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::F(_) => Some(Chord { ctrl, meta, code }),
        _ => None,
    }
}

fn named_key(name: &str) -> Option<KeyCode> {
    Some(match name {
        "SPC" => KeyCode::Char(' '),
        "RET" => KeyCode::Enter,
        "TAB" => KeyCode::Tab,
        "ESC" => KeyCode::Esc,
        "DEL" => KeyCode::Backspace,
        "UP" => KeyCode::Up,
        "DOWN" => KeyCode::Down,
        "LEFT" => KeyCode::Left,
        "RIGHT" => KeyCode::Right,
        "HOME" => KeyCode::Home,
        "END" => KeyCode::End,
        "PRIOR" => KeyCode::PageUp,
        "NEXT" => KeyCode::PageDown,
        "DELETE" => KeyCode::Delete,
        _ => {
            if let Some(digits) = name.strip_prefix('F') {
                if let Ok(n) = digits.parse::<u8>() {
                    if (1..=12).contains(&n) {
                        return Some(KeyCode::F(n));
                    }
                }
            }
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    })
}
```

Then make `parse_chord` route through the same canonicalization — replace its final `Some(Chord { ... })` expression:

```rust
    if rest.is_empty() {
        return None;
    }
    // Same boundary as `Chord::from_key`, so a config binding and a decoded
    // key can never disagree about what a chord is (spec §3.2). `C-i` and
    // `TAB` remain DIFFERENT chords: `C-i` fires only on a kitty terminal.
    canonical_chord(ctrl, meta, named_key(rest)?)
}
```

Finally teach `format_chord` the new names — replace its `match chord.code` block:

```rust
    match chord.code {
        KeyCode::Char(' ') => out.push_str("SPC"),
        KeyCode::Char(c) => out.push(c),
        KeyCode::Enter => out.push_str("RET"),
        KeyCode::Tab => out.push_str("TAB"),
        KeyCode::Esc => out.push_str("ESC"),
        KeyCode::Backspace => out.push_str("DEL"),
        KeyCode::Delete => out.push_str("DELETE"),
        KeyCode::Up => out.push_str("UP"),
        KeyCode::Down => out.push_str("DOWN"),
        KeyCode::Left => out.push_str("LEFT"),
        KeyCode::Right => out.push_str("RIGHT"),
        KeyCode::Home => out.push_str("HOME"),
        KeyCode::End => out.push_str("END"),
        KeyCode::PageUp => out.push_str("PRIOR"),
        KeyCode::PageDown => out.push_str("NEXT"),
        KeyCode::F(n) => out.push_str(&format!("F{n}")),
        other => out.push_str(&format!("{other:?}")),
    }
```

- [ ] **Step 4: Run the tests**

The existing `parses_single_chords` test's `DEL` / `TAB` / `ESC` assertions still hold unchanged — nothing collapses.

Run: `cargo test --locked --bin herdr emacs::keymap`
Expected: `test result: ok.` (13 passed)

- [ ] **Step 5: Verify the rest of the layer still passes**

Run: `cargo test --locked --bin herdr emacs:: app::input::emacs`
Expected: `test result: ok.` (`ESC` still exits TEXT mode; a kitty `C-[` is now a *different* chord and does not, which is what §3.8 needs)

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/emacs/keymap.rs
git commit -m "feat: pin the one-directional key normalization boundary with full encoding coverage"
```

---

### Task 3: Undefined keys must speak

Today every unbound single chord in TEXT mode says "Buffer is read-only" — wrong and actively misleading (spec §3.3). Read-only is for keys that would *insert*; everything else is "undefined".

**Files:**
- Modify: `src/app/input/emacs.rs` (the `Lookup::Unbound` arm)
- Test: inline `#[cfg(test)]` in `src/app/input/emacs.rs`

**Interfaces:**
- Consumes: `Chord::is_self_insert()` (Task 2), `MapContext` (Task 1).

- [ ] **Step 1: Write the failing tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    /// Spec §3.3: an unbound NON-self-inserting key in TEXT mode is
    /// "undefined", not "read-only". The old code said "Buffer is read-only"
    /// for every unbound single chord, which is wrong.
    #[tokio::test]
    async fn unbound_control_key_in_text_mode_is_undefined_not_read_only() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x14]); // C-t: bound nowhere
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-t is undefined"));
        assert!(sent_bytes(&mut rx).is_empty());
        assert!(app.state.emacs.text_mode.is_some(), "still in TEXT mode");
    }

    /// ...and an unbound META key likewise.
    #[tokio::test]
    async fn unbound_meta_key_in_text_mode_is_undefined() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'z']); // M-z
        assert_eq!(app.state.emacs.echo.as_deref(), Some("M-z is undefined"));
    }

    /// Read-only is reserved for keys that WOULD insert.
    #[tokio::test]
    async fn self_inserting_key_in_text_mode_reports_read_only() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![b'x']);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Buffer is read-only"));
        app.route_client_input(vec![b'5']);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Buffer is read-only"));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    /// An unbound multi-chord sequence in TEXT mode names the whole sequence.
    #[tokio::test]
    async fn unbound_sequence_in_text_mode_names_the_sequence() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x18, b'z']); // C-x z
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-x z is undefined"));
    }

    /// Live mode: a single unbound key belongs to the agent, silently.
    #[tokio::test]
    async fn unbound_single_key_in_live_mode_stays_silent_and_reaches_the_pane() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x14]); // C-t
        assert_eq!(app.state.emacs.echo, None, "no echo: the agent owns it");
        assert_eq!(sent_bytes(&mut rx), vec![0x14]);
    }

    /// Live mode: an unbound MULTI-chord sequence is the layer's own fault
    /// and must say so.
    #[tokio::test]
    async fn unbound_sequence_in_live_mode_is_undefined() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x18, b'z']); // C-x z
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-x z is undefined"));
        assert!(sent_bytes(&mut rx).is_empty());
    }
```

Delete the now-superseded test `unbound_printable_keys_report_read_only` (it is replaced verbatim in intent by `self_inserting_key_in_text_mode_reports_read_only`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `unbound_control_key_in_text_mode_is_undefined_not_read_only` FAILS with `left: Some("Buffer is read-only")`, `right: Some("C-t is undefined")`; `unbound_meta_key_in_text_mode_is_undefined` FAILS the same way.

- [ ] **Step 3: Implement the feedback rules.** In `src/app/input/emacs.rs`, replace the `Lookup::Unbound` arm of the dispatcher with:

```rust
            Lookup::Unbound => {
                self.state.emacs.pending.clear();
                self.state.emacs.last_yank = None;
                let single = seq.len() == 1;
                // Spec §3.3. "Buffer is read-only" is ONLY for a key that
                // would insert; it is not the catch-all for unbound keys.
                if text_active && single && chord.is_self_insert() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                    true
                } else if !text_active && single {
                    // Live mode: a single unbound key belongs to the agent.
                    // Silence here is correct — see the term-char-mode
                    // contract in spec §2.
                    false
                } else {
                    self.state.emacs.echo = Some(format!("{} is undefined", format_seq(&seq)));
                    true
                }
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `test result: ok.`

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/app/input/emacs.rs
git commit -m "fix: say 'is undefined' for unbound keys; reserve 'read-only' for self-inserting keys"
```

---

### Task 4: The command surface stops being hand-maintained

`EmacsCommand` splits into `Builtin(EmacsBuiltin)` and `Herdr(NavigateAction)`. A macro generates both an **exhaustive match** over `NavigateAction` and the name table from one list — a new upstream variant fails the build until it is named (spec §3.4, success criterion 6).

**Files:**
- Modify: `src/app/mod.rs:15` (`mod input;` → `pub(crate) mod input;`)
- Modify: `src/app/input/mod.rs:41` (`mod navigate;` → `pub(crate) mod navigate;`)
- Modify: `src/emacs/commands.rs` (the whole enum + tables)
- Modify: `src/app/input/emacs.rs` (`execute_emacs_command`, `emacs_text_motion`)
- Test: inline `#[cfg(test)]` in `src/emacs/commands.rs` and `src/app/input/emacs.rs`

**Interfaces:**
- Consumes: `crate::app::input::navigate::NavigateAction` (45 variants, `Copy + PartialEq + Eq`), `ActionContext::Prefix`.
- Produces (used by Tasks 5-11):
  - `commands::EmacsBuiltin` (Copy enum, layer-native commands)
  - `commands::EmacsCommand { Builtin(EmacsBuiltin), Herdr(NavigateAction) }` (Copy)
  - `EmacsCommand::name(self) -> &'static str`, `EmacsCommand::from_name(&str) -> Option<EmacsCommand>`
  - `commands::all_commands() -> Vec<(&'static str, EmacsCommand)>` — the full M-x namespace, sorted by name
  - `commands::herdr_command_name(NavigateAction) -> &'static str` — the exhaustive match
  - `commands::MapSlot { Global, Text, Both, Minibuffer }`, `EmacsBuiltin::default_map(self) -> MapSlot`
  - `commands::NAVIGATE_ACTION_COUNT: usize = 45`

- [ ] **Step 1: Write the failing command-surface tests.** Replace the ENTIRE `#[cfg(test)] mod tests` block at the bottom of `src/emacs/commands.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs::keymap::{parse_key_seq, Lookup};

    fn builtin(b: EmacsBuiltin) -> EmacsCommand {
        EmacsCommand::Builtin(b)
    }
    fn herdr(a: NavigateAction) -> EmacsCommand {
        EmacsCommand::Herdr(a)
    }

    #[test]
    fn every_navigate_action_has_a_name() {
        // The exhaustive match in `herdr_command_name` is the compiler-enforced
        // guarantee (spec §3.4). This test pins the count so a silently added
        // upstream variant cannot slip past the table either.
        assert_eq!(
            HERDR_COMMANDS.len(),
            NAVIGATE_ACTION_COUNT,
            "every NavigateAction variant must appear in herdr_command_table!"
        );
        for (name, action) in HERDR_COMMANDS.iter().copied() {
            assert_eq!(herdr_command_name(action), name);
            assert_eq!(EmacsCommand::from_name(name), Some(EmacsCommand::Herdr(action)));
        }
    }

    #[test]
    fn command_names_round_trip_and_are_unique() {
        let all = all_commands();
        let mut seen = std::collections::HashSet::new();
        for (name, cmd) in all.iter().copied() {
            assert_eq!(cmd.name(), name, "{name} round-trips");
            assert_eq!(EmacsCommand::from_name(name), Some(cmd));
            assert!(seen.insert(name), "duplicate command name {name}");
        }
        assert_eq!(EmacsCommand::from_name("no-such-command"), None);
        // Sorted, so M-x completion is deterministic.
        let mut sorted: Vec<&str> = all.iter().map(|(name, _)| *name).collect();
        let original = sorted.clone();
        sorted.sort_unstable();
        assert_eq!(original, sorted, "all_commands() is sorted by name");
    }

    #[test]
    fn herdr_actions_use_emacs_vocabulary_where_one_exists() {
        for (name, action) in [
            ("split-window-right", NavigateAction::SplitVertical),
            ("split-window-below", NavigateAction::SplitHorizontal),
            ("other-window", NavigateAction::CyclePaneNext),
            ("previous-window", NavigateAction::CyclePanePrevious),
            ("delete-window", NavigateAction::ClosePane),
            ("delete-other-windows", NavigateAction::Zoom),
            ("switch-to-buffer", NavigateAction::OpenNavigator),
            ("windmove-left", NavigateAction::FocusPaneLeft),
            ("windmove-swap-states-right", NavigateAction::SwapPaneRight),
        ] {
            assert_eq!(herdr_command_name(action), name);
        }
        // ...and herdr vocabulary where none does.
        for (name, action) in [
            ("toggle-sidebar", NavigateAction::ToggleSidebar),
            ("detach", NavigateAction::Detach),
            ("open-navigator-notification-target", NavigateAction::OpenNotificationTarget),
            ("new-worktree", NavigateAction::NewWorktree),
        ] {
            assert_eq!(herdr_command_name(action), name);
        }
    }

    #[test]
    fn indexed_actions_default_to_index_zero() {
        // The index comes from the prefix arg at execution time
        // (`C-u 2 M-x switch-tab`), so the named command carries 0.
        assert_eq!(
            EmacsCommand::from_name("switch-tab"),
            Some(herdr(NavigateAction::SwitchTab(0)))
        );
        assert_eq!(
            EmacsCommand::from_name("switch-workspace"),
            Some(herdr(NavigateAction::SwitchWorkspace(0)))
        );
        assert_eq!(
            EmacsCommand::from_name("focus-agent"),
            Some(herdr(NavigateAction::FocusAgent(0)))
        );
        // Any index still names the same command.
        assert_eq!(herdr_command_name(NavigateAction::SwitchTab(7)), "switch-tab");
    }

    #[test]
    fn default_global_keymap_binds_management_chords() {
        let (keymaps, warnings) = build_keymaps(&Default::default());
        assert!(warnings.is_empty());
        let cases = [
            ("C-x 2", herdr(NavigateAction::SplitHorizontal)),
            ("C-x 3", herdr(NavigateAction::SplitVertical)),
            ("C-x o", herdr(NavigateAction::CyclePaneNext)),
            ("C-x 0", herdr(NavigateAction::ClosePane)),
            ("C-x 1", herdr(NavigateAction::Zoom)),
            ("C-x b", herdr(NavigateAction::OpenNavigator)),
            ("C-x c", herdr(NavigateAction::NewTab)),
            ("C-x n", herdr(NavigateAction::NextTab)),
            ("C-x p", herdr(NavigateAction::PreviousTab)),
            ("C-x k", herdr(NavigateAction::CloseTab)),
            ("C-x w", herdr(NavigateAction::WorkspacePicker)),
            ("C-x [", builtin(EmacsBuiltin::TextMode)),
            ("C-q", builtin(EmacsBuiltin::QuotedInsert)),
            ("C-g", builtin(EmacsBuiltin::KeyboardQuit)),
            ("C-y", builtin(EmacsBuiltin::Yank)),
            ("M-y", builtin(EmacsBuiltin::YankPop)),
        ];
        for (seq, cmd) in cases {
            assert_eq!(
                keymaps.global.lookup(&parse_key_seq(seq).unwrap()),
                Lookup::Bound(cmd),
                "global {seq}"
            );
        }
        assert_eq!(
            keymaps.global.lookup(&parse_key_seq("C-x").unwrap()),
            Lookup::Prefix
        );
    }

    #[test]
    fn default_text_keymap_binds_motions_and_region() {
        let (keymaps, _) = build_keymaps(&Default::default());
        let cases = [
            ("C-f", EmacsBuiltin::ForwardChar),
            ("C-b", EmacsBuiltin::BackwardChar),
            ("C-n", EmacsBuiltin::NextLine),
            ("C-p", EmacsBuiltin::PreviousLine),
            ("M-f", EmacsBuiltin::ForwardWord),
            ("M-b", EmacsBuiltin::BackwardWord),
            ("C-a", EmacsBuiltin::MoveBeginningOfLine),
            ("C-e", EmacsBuiltin::MoveEndOfLine),
            ("C-v", EmacsBuiltin::ScrollUp),
            ("M-v", EmacsBuiltin::ScrollDown),
            ("M-<", EmacsBuiltin::BeginningOfBuffer),
            ("M->", EmacsBuiltin::EndOfBuffer),
            ("M-g g", EmacsBuiltin::GotoLine),
            ("C-SPC", EmacsBuiltin::SetMark),
            ("C-x C-x", EmacsBuiltin::ExchangePointAndMark),
            ("M-w", EmacsBuiltin::KillRingSave),
            ("C-w", EmacsBuiltin::KillRegion),
            ("C-y", EmacsBuiltin::Yank),
            ("M-y", EmacsBuiltin::YankPop),
            ("q", EmacsBuiltin::ExitTextMode),
            ("ESC", EmacsBuiltin::ExitTextMode),
        ];
        for (seq, cmd) in cases {
            assert_eq!(
                keymaps.text.lookup(&parse_key_seq(seq).unwrap()),
                Lookup::Bound(builtin(cmd)),
                "text {seq}"
            );
        }
        assert_eq!(
            keymaps.text.lookup(&parse_key_seq("M-g").unwrap()),
            Lookup::Prefix
        );
        // C-g lives in the global map now and reaches TEXT mode by fallthrough.
        assert_eq!(
            keymaps.lookup(MapContext::Text, &parse_key_seq("C-g").unwrap()),
            Lookup::Bound(builtin(EmacsBuiltin::KeyboardQuit))
        );
    }

    #[test]
    fn active_maps_stack_by_context() {
        let (keymaps, _) = build_keymaps(&Default::default());
        let names = |ctx| {
            keymaps
                .active_maps(ctx)
                .into_iter()
                .map(|active| active.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(MapContext::Live), vec!["global"]);
        assert_eq!(names(MapContext::Text), vec!["text", "global"]);
        assert_eq!(names(MapContext::Minibuffer), vec!["minibuffer", "global"]);
    }

    #[test]
    fn global_bindings_fall_through_in_text_mode() {
        let (keymaps, _) = build_keymaps(&Default::default());
        assert_eq!(
            keymaps.lookup(MapContext::Text, &parse_key_seq("C-x 3").unwrap()),
            Lookup::Bound(herdr(NavigateAction::SplitVertical))
        );
        assert_eq!(
            keymaps.lookup(MapContext::Text, &parse_key_seq("C-x C-x").unwrap()),
            Lookup::Bound(builtin(EmacsBuiltin::ExchangePointAndMark))
        );
        assert_eq!(
            keymaps.lookup(MapContext::Text, &parse_key_seq("C-x").unwrap()),
            Lookup::Prefix
        );
        assert_eq!(
            keymaps.lookup(MapContext::Live, &parse_key_seq("C-f").unwrap()),
            Lookup::Unbound
        );
    }

    #[test]
    fn config_overrides_bind_any_command_and_warn_on_junk() {
        let mut overrides = std::collections::HashMap::new();
        // Spec §4, verbatim.
        overrides.insert("C-x 4".to_string(), "split-window-right".to_string());
        overrides.insert("C-x t".to_string(), "toggle-sidebar".to_string());
        overrides.insert("C-x z".to_string(), "no-such-command".to_string());
        overrides.insert("???".to_string(), "new-tab".to_string());
        let (keymaps, warnings) = build_keymaps(&overrides);
        assert_eq!(
            keymaps.global.lookup(&parse_key_seq("C-x 4").unwrap()),
            Lookup::Bound(herdr(NavigateAction::SplitVertical))
        );
        assert_eq!(
            keymaps.global.lookup(&parse_key_seq("C-x t").unwrap()),
            Lookup::Bound(herdr(NavigateAction::ToggleSidebar)),
            "a herdr action, exposed by name, with no code change"
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn text_only_builtins_override_into_the_text_map() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("C-j".to_string(), "next-line".to_string());
        let (keymaps, warnings) = build_keymaps(&overrides);
        assert!(warnings.is_empty());
        assert_eq!(
            keymaps.text.lookup(&parse_key_seq("C-j").unwrap()),
            Lookup::Bound(builtin(EmacsBuiltin::NextLine))
        );
        assert_eq!(
            keymaps.global.lookup(&parse_key_seq("C-j").unwrap()),
            Lookup::Unbound,
            "a motion must not steal C-j from the agent in live mode"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::commands`
Expected: compile error — `cannot find type EmacsBuiltin`, `cannot find function herdr_command_name`, `NavigateAction` not in scope

- [ ] **Step 3: Open the `NavigateAction` path.** In `src/app/mod.rs` line 15:

```rust
// Emacs layer seam (fork): `crate::emacs::commands` names every NavigateAction.
pub(crate) mod input;
```

In `src/app/input/mod.rs` line 41:

```rust
// Emacs layer seam (fork): `crate::emacs::commands` names every NavigateAction.
pub(crate) mod navigate;
```

- [ ] **Step 4: Rewrite the command table.** Replace everything in `src/emacs/commands.rs` ABOVE the `#[cfg(test)]` module with:

```rust
//! The command table is the spine of the layer: every capability is a
//! named command. Keymaps bind chords to commands, `M-x` invokes them by
//! name, and prefix args are passed to every command via
//! `App::execute_emacs_command(cmd, prefix)`.
//!
//! A command is either a layer **builtin** (motions, rings, TEXT mode,
//! help, M-x, C-u) or one of herdr's own **`NavigateAction`s**. Every
//! `NavigateAction` is named by `herdr_command_table!`, which expands to an
//! exhaustive `match` — when upstream adds an action, this fork stops
//! compiling until the action is named. That compiler error is the whole
//! point (spec §3.4).

use std::collections::HashMap;

use crate::app::input::navigate::NavigateAction;

use super::keymap::{parse_key_seq, stack_lookup, Chord, Keymap, Lookup};

/// Number of `NavigateAction` variants. Pinned so a silent upstream addition
/// is caught by `every_navigate_action_has_a_name` even if someone patches
/// the match with a catch-all arm (don't).
pub const NAVIGATE_ACTION_COUNT: usize = 45;

/// Generates BOTH the exhaustive name match and the name -> action table
/// from a single list, so the two can never drift.
///
/// `indexed` variants carry a `usize` payload that a name cannot express;
/// they are constructed with index 0 and take their real index from the
/// prefix argument at execution time (`C-u 2 M-x switch-tab`).
macro_rules! herdr_command_table {
    (
        unit: [ $( $unit:ident => $unit_name:literal ),* $(,)? ],
        indexed: [ $( $idx:ident => $idx_name:literal ),* $(,)? ],
    ) => {
        /// Exhaustive over `NavigateAction`: a new upstream variant fails the
        /// build here until it is named.
        pub fn herdr_command_name(action: NavigateAction) -> &'static str {
            match action {
                $( NavigateAction::$unit => $unit_name, )*
                $( NavigateAction::$idx(_) => $idx_name, )*
            }
        }

        /// Every herdr action, by name. Sorted at use sites via `all_commands`.
        pub const HERDR_COMMANDS: &[(&str, NavigateAction)] = &[
            $( ($unit_name, NavigateAction::$unit), )*
            $( ($idx_name, NavigateAction::$idx(0)), )*
        ];

        /// True when this action takes its index from the prefix argument.
        pub fn herdr_action_is_indexed(action: NavigateAction) -> bool {
            matches!(action, $( NavigateAction::$idx(_) )|*)
        }
    };
}

herdr_command_table! {
    unit: [
        // Emacs vocabulary where a real equivalent exists.
        SplitVertical            => "split-window-right",
        SplitHorizontal          => "split-window-below",
        ClosePane                => "delete-window",
        Zoom                     => "delete-other-windows",
        CyclePaneNext            => "other-window",
        CyclePanePrevious        => "previous-window",
        OpenNavigator            => "switch-to-buffer",
        FocusPaneLeft            => "windmove-left",
        FocusPaneDown            => "windmove-down",
        FocusPaneUp              => "windmove-up",
        FocusPaneRight           => "windmove-right",
        SwapPaneLeft             => "windmove-swap-states-left",
        SwapPaneDown             => "windmove-swap-states-down",
        SwapPaneUp               => "windmove-swap-states-up",
        SwapPaneRight            => "windmove-swap-states-right",
        CloseTab                 => "kill-tab",
        // herdr vocabulary where none does.
        NewWorkspace             => "new-workspace",
        NewWorktree              => "new-worktree",
        OpenWorktree             => "open-worktree",
        RemoveWorktree           => "remove-worktree",
        RenameWorkspace          => "rename-workspace",
        CloseWorkspace           => "close-workspace",
        WorkspacePicker          => "workspace-picker",
        PreviousWorkspace        => "previous-workspace",
        NextWorkspace            => "next-workspace",
        PreviousAgent            => "previous-agent",
        NextAgent                => "next-agent",
        NewTab                   => "new-tab",
        RenameTab                => "rename-tab",
        PreviousTab              => "previous-tab",
        NextTab                  => "next-tab",
        RenamePane               => "rename-pane",
        EditScrollback           => "edit-scrollback",
        CopyMode                 => "copy-mode",
        EnterResizeMode          => "resize-mode",
        ToggleSidebar            => "toggle-sidebar",
        LastPane                 => "last-pane",
        Help                     => "herdr-help",
        Settings                 => "settings",
        ReloadConfig             => "reload-config",
        OpenNotificationTarget   => "open-navigator-notification-target",
        Detach                   => "detach",
    ],
    indexed: [
        SwitchWorkspace          => "switch-workspace",
        SwitchTab                => "switch-tab",
        FocusAgent               => "focus-agent",
    ],
}

/// Layer-native commands: things herdr has no action for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmacsBuiltin {
    // Dispatcher
    UniversalArgument,
    ExecuteExtendedCommand,
    KeyboardQuit,
    QuotedInsert,
    // TEXT mode
    TextMode,
    ExitTextMode,
    ForwardChar,
    BackwardChar,
    NextLine,
    PreviousLine,
    ForwardWord,
    BackwardWord,
    MoveBeginningOfLine,
    MoveEndOfLine,
    ScrollUp,
    ScrollDown,
    BeginningOfBuffer,
    EndOfBuffer,
    GotoLine,
    SetMark,
    ExchangePointAndMark,
    // Rings
    KillRingSave,
    KillRegion,
    Yank,
    YankPop,
    // Minibuffer editing
    DeleteBackwardChar,
    KillLine,
    BackwardKillWord,
    MinibufferComplete,
    ExitMinibuffer,
    // Help
    DescribeKey,
    DescribeBindings,
}

/// Canonical builtin name table (part of the M-x namespace). Keep sorted.
pub const BUILTIN_NAMES: &[(EmacsBuiltin, &str)] = &[
    (EmacsBuiltin::BackwardChar, "backward-char"),
    (EmacsBuiltin::BackwardKillWord, "backward-kill-word"),
    (EmacsBuiltin::BackwardWord, "backward-word"),
    (EmacsBuiltin::BeginningOfBuffer, "beginning-of-buffer"),
    (EmacsBuiltin::DeleteBackwardChar, "delete-backward-char"),
    (EmacsBuiltin::DescribeBindings, "describe-bindings"),
    (EmacsBuiltin::DescribeKey, "describe-key"),
    (EmacsBuiltin::EndOfBuffer, "end-of-buffer"),
    (EmacsBuiltin::ExchangePointAndMark, "exchange-point-and-mark"),
    (EmacsBuiltin::ExecuteExtendedCommand, "execute-extended-command"),
    (EmacsBuiltin::ExitMinibuffer, "exit-minibuffer"),
    (EmacsBuiltin::ExitTextMode, "exit-text-mode"),
    (EmacsBuiltin::ForwardChar, "forward-char"),
    (EmacsBuiltin::ForwardWord, "forward-word"),
    (EmacsBuiltin::GotoLine, "goto-line"),
    (EmacsBuiltin::KeyboardQuit, "keyboard-quit"),
    (EmacsBuiltin::KillLine, "kill-line"),
    (EmacsBuiltin::KillRegion, "kill-region"),
    (EmacsBuiltin::KillRingSave, "kill-ring-save"),
    (EmacsBuiltin::MinibufferComplete, "minibuffer-complete"),
    (EmacsBuiltin::MoveBeginningOfLine, "move-beginning-of-line"),
    (EmacsBuiltin::MoveEndOfLine, "move-end-of-line"),
    (EmacsBuiltin::NextLine, "next-line"),
    (EmacsBuiltin::PreviousLine, "previous-line"),
    (EmacsBuiltin::QuotedInsert, "quoted-insert"),
    (EmacsBuiltin::ScrollDown, "scroll-down"),
    (EmacsBuiltin::ScrollUp, "scroll-up"),
    (EmacsBuiltin::SetMark, "set-mark-command"),
    (EmacsBuiltin::TextMode, "text-mode"),
    (EmacsBuiltin::UniversalArgument, "universal-argument"),
    (EmacsBuiltin::Yank, "yank"),
    (EmacsBuiltin::YankPop, "yank-pop"),
];

/// Which keymap a `[emacs.keys]` override for this command lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapSlot {
    /// Live mode (and, via fallthrough, everywhere else).
    Global,
    /// TEXT mode only — must not steal the key from the agent in live mode.
    Text,
    /// Both maps (yank works in a pane and in a buffer).
    Both,
    /// Minibuffer editing only.
    Minibuffer,
}

impl EmacsBuiltin {
    pub fn name(self) -> &'static str {
        BUILTIN_NAMES
            .iter()
            .find(|(cmd, _)| *cmd == self)
            .map(|(_, name)| *name)
            .expect("every builtin has a name")
    }

    /// Exhaustive: a new builtin must declare where an override binds it.
    pub fn default_map(self) -> MapSlot {
        match self {
            Self::UniversalArgument
            | Self::ExecuteExtendedCommand
            | Self::KeyboardQuit
            | Self::QuotedInsert
            | Self::TextMode => MapSlot::Global,
            Self::ExitTextMode
            | Self::ForwardChar
            | Self::BackwardChar
            | Self::NextLine
            | Self::PreviousLine
            | Self::ForwardWord
            | Self::BackwardWord
            | Self::MoveBeginningOfLine
            | Self::MoveEndOfLine
            | Self::ScrollUp
            | Self::ScrollDown
            | Self::BeginningOfBuffer
            | Self::EndOfBuffer
            | Self::GotoLine
            | Self::SetMark
            | Self::ExchangePointAndMark
            | Self::KillRingSave
            | Self::KillRegion => MapSlot::Text,
            Self::Yank | Self::YankPop | Self::DescribeKey | Self::DescribeBindings => {
                MapSlot::Both
            }
            Self::DeleteBackwardChar
            | Self::KillLine
            | Self::BackwardKillWord
            | Self::MinibufferComplete
            | Self::ExitMinibuffer => MapSlot::Minibuffer,
        }
    }
}

/// A bound command: a layer builtin or one of herdr's own actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmacsCommand {
    Builtin(EmacsBuiltin),
    Herdr(NavigateAction),
}

impl EmacsCommand {
    pub fn name(self) -> &'static str {
        match self {
            Self::Builtin(builtin) => builtin.name(),
            Self::Herdr(action) => herdr_command_name(action),
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        if let Some((builtin, _)) = BUILTIN_NAMES.iter().find(|(_, n)| *n == name) {
            return Some(Self::Builtin(*builtin));
        }
        HERDR_COMMANDS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, action)| Self::Herdr(*action))
    }

    fn map_slot(self) -> MapSlot {
        match self {
            Self::Builtin(builtin) => builtin.default_map(),
            // Every herdr action is a global (management) command.
            Self::Herdr(_) => MapSlot::Global,
        }
    }
}

/// The full M-x namespace: builtins + all 45 herdr actions, sorted by name.
pub fn all_commands() -> Vec<(&'static str, EmacsCommand)> {
    let mut all: Vec<(&'static str, EmacsCommand)> = BUILTIN_NAMES
        .iter()
        .map(|(builtin, name)| (*name, EmacsCommand::Builtin(*builtin)))
        .chain(
            HERDR_COMMANDS
                .iter()
                .map(|(name, action)| (*name, EmacsCommand::Herdr(*action))),
        )
        .collect();
    all.sort_unstable_by_key(|(name, _)| *name);
    all
}

/// Which keymaps are active. Derived from `EmacsState` (see
/// `EmacsState::map_context`), never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapContext {
    /// Pane focused, TEXT mode off: the pane owns every key we do not steal.
    Live,
    /// TEXT mode: an ordinary read-only Emacs buffer — the full stack applies.
    Text,
    /// A minibuffer prompt is open.
    Minibuffer,
}

/// One entry of the active keymap stack: a display name (for
/// `describe-bindings`) and the map itself.
#[derive(Debug, Clone, Copy)]
pub struct ActiveMap<'a> {
    pub name: &'static str,
    pub map: &'a Keymap<EmacsCommand>,
}

#[derive(Debug, Clone, Default)]
pub struct KeymapSet {
    /// Always active. In TEXT mode and in the minibuffer it is the
    /// fallthrough map, exactly like Emacs's `global-map`.
    pub global: Keymap<EmacsCommand>,
    /// Active while TEXT mode is on (shadows `global`).
    pub text: Keymap<EmacsCommand>,
    /// Active while a minibuffer prompt is open (shadows `global`).
    pub minibuffer: Keymap<EmacsCommand>,
}

impl KeymapSet {
    /// The active keymap stack, highest priority first (spec §3.1).
    /// Emacs's minibuffer does not inherit the previous buffer's local map,
    /// so `Minibuffer` is `[minibuffer, global]`, not `[minibuffer, text, global]`.
    pub fn active_maps(&self, ctx: MapContext) -> Vec<ActiveMap<'_>> {
        let global = ActiveMap {
            name: "global",
            map: &self.global,
        };
        match ctx {
            MapContext::Live => vec![global],
            MapContext::Text => vec![
                ActiveMap {
                    name: "text",
                    map: &self.text,
                },
                global,
            ],
            MapContext::Minibuffer => vec![
                ActiveMap {
                    name: "minibuffer",
                    map: &self.minibuffer,
                },
                global,
            ],
        }
    }

    /// Look `seq` up across the active stack.
    pub fn lookup(&self, ctx: MapContext, seq: &[Chord]) -> Lookup<EmacsCommand> {
        stack_lookup(
            self.active_maps(ctx).into_iter().map(|active| active.map),
            seq,
        )
    }
}

const DEFAULT_GLOBAL_BINDINGS: &[(&str, EmacsCommand)] = &[
    ("C-x 2", EmacsCommand::Herdr(NavigateAction::SplitHorizontal)),
    ("C-x 3", EmacsCommand::Herdr(NavigateAction::SplitVertical)),
    ("C-x o", EmacsCommand::Herdr(NavigateAction::CyclePaneNext)),
    ("C-x 0", EmacsCommand::Herdr(NavigateAction::ClosePane)),
    ("C-x 1", EmacsCommand::Herdr(NavigateAction::Zoom)),
    ("C-x b", EmacsCommand::Herdr(NavigateAction::OpenNavigator)),
    ("C-x c", EmacsCommand::Herdr(NavigateAction::NewTab)),
    ("C-x n", EmacsCommand::Herdr(NavigateAction::NextTab)),
    ("C-x p", EmacsCommand::Herdr(NavigateAction::PreviousTab)),
    ("C-x k", EmacsCommand::Herdr(NavigateAction::CloseTab)),
    ("C-x w", EmacsCommand::Herdr(NavigateAction::WorkspacePicker)),
    ("C-x [", EmacsCommand::Builtin(EmacsBuiltin::TextMode)),
    ("C-q", EmacsCommand::Builtin(EmacsBuiltin::QuotedInsert)),
    ("C-g", EmacsCommand::Builtin(EmacsBuiltin::KeyboardQuit)),
    ("C-y", EmacsCommand::Builtin(EmacsBuiltin::Yank)),
    ("M-y", EmacsCommand::Builtin(EmacsBuiltin::YankPop)),
];

const DEFAULT_TEXT_BINDINGS: &[(&str, EmacsCommand)] = &[
    ("C-f", EmacsCommand::Builtin(EmacsBuiltin::ForwardChar)),
    ("C-b", EmacsCommand::Builtin(EmacsBuiltin::BackwardChar)),
    ("C-n", EmacsCommand::Builtin(EmacsBuiltin::NextLine)),
    ("C-p", EmacsCommand::Builtin(EmacsBuiltin::PreviousLine)),
    ("M-f", EmacsCommand::Builtin(EmacsBuiltin::ForwardWord)),
    ("M-b", EmacsCommand::Builtin(EmacsBuiltin::BackwardWord)),
    ("C-a", EmacsCommand::Builtin(EmacsBuiltin::MoveBeginningOfLine)),
    ("C-e", EmacsCommand::Builtin(EmacsBuiltin::MoveEndOfLine)),
    ("C-v", EmacsCommand::Builtin(EmacsBuiltin::ScrollUp)),
    ("M-v", EmacsCommand::Builtin(EmacsBuiltin::ScrollDown)),
    ("M-<", EmacsCommand::Builtin(EmacsBuiltin::BeginningOfBuffer)),
    ("M->", EmacsCommand::Builtin(EmacsBuiltin::EndOfBuffer)),
    ("M-g g", EmacsCommand::Builtin(EmacsBuiltin::GotoLine)),
    ("C-SPC", EmacsCommand::Builtin(EmacsBuiltin::SetMark)),
    (
        "C-x C-x",
        EmacsCommand::Builtin(EmacsBuiltin::ExchangePointAndMark),
    ),
    ("M-w", EmacsCommand::Builtin(EmacsBuiltin::KillRingSave)),
    ("C-w", EmacsCommand::Builtin(EmacsBuiltin::KillRegion)),
    ("C-y", EmacsCommand::Builtin(EmacsBuiltin::Yank)),
    ("M-y", EmacsCommand::Builtin(EmacsBuiltin::YankPop)),
    ("q", EmacsCommand::Builtin(EmacsBuiltin::ExitTextMode)),
    ("ESC", EmacsCommand::Builtin(EmacsBuiltin::ExitTextMode)),
];

/// Filled in by Task 8 (minibuffer) and Task 10 (help).
const DEFAULT_MINIBUFFER_BINDINGS: &[(&str, EmacsCommand)] = &[];

/// Build the default keymaps and apply `[emacs.keys]` overrides.
/// Returns (keymaps, warnings) — invalid chord strings or unknown command
/// names become warnings, never hard errors. Task 6 routes the warnings
/// through the config diagnostics pipeline.
pub fn build_keymaps(overrides: &HashMap<String, String>) -> (KeymapSet, Vec<String>) {
    let mut set = KeymapSet::default();
    for (seq, cmd) in DEFAULT_GLOBAL_BINDINGS {
        set.global.bind(
            parse_key_seq(seq).expect("default global binding parses"),
            *cmd,
        );
    }
    for (seq, cmd) in DEFAULT_TEXT_BINDINGS {
        set.text.bind(
            parse_key_seq(seq).expect("default text binding parses"),
            *cmd,
        );
    }
    for (seq, cmd) in DEFAULT_MINIBUFFER_BINDINGS {
        set.minibuffer.bind(
            parse_key_seq(seq).expect("default minibuffer binding parses"),
            *cmd,
        );
    }

    let mut warnings = Vec::new();
    // Sort for deterministic application order.
    let mut entries: Vec<_> = overrides.iter().collect();
    entries.sort();
    for (seq_str, cmd_name) in entries {
        let Some(seq) = parse_key_seq(seq_str) else {
            warnings.push(format!("[emacs.keys] invalid key sequence \"{seq_str}\""));
            continue;
        };
        let Some(cmd) = EmacsCommand::from_name(cmd_name) else {
            warnings.push(format!("[emacs.keys] unknown command \"{cmd_name}\""));
            continue;
        };
        match cmd.map_slot() {
            MapSlot::Global => set.global.bind(seq, cmd),
            MapSlot::Text => set.text.bind(seq, cmd),
            MapSlot::Minibuffer => set.minibuffer.bind(seq, cmd),
            MapSlot::Both => {
                set.global.bind(seq.clone(), cmd);
                set.text.bind(seq, cmd);
            }
        }
    }
    (set, warnings)
}
```

- [ ] **Step 5: Rewrite the executor.** In `src/app/input/emacs.rs`, change the import

```rust
use crate::emacs::commands::EmacsCommand;
```

to

```rust
use crate::emacs::commands::{herdr_action_is_indexed, EmacsBuiltin, EmacsCommand, MapContext};
```

(and drop the now-duplicate `use crate::emacs::commands::MapContext;` line added in Task 1).

Replace the whole `execute_emacs_command` function with:

```rust
    /// Execute a named command. `prefix` is the universal argument
    /// (`Option<i64>`): motions repeat, `C-u C-SPC` pops the mark ring, and
    /// the three indexed herdr actions take their index from it.
    pub(crate) fn execute_emacs_command(&mut self, cmd: EmacsCommand, prefix: Option<i64>) {
        if !matches!(
            cmd,
            EmacsCommand::Builtin(EmacsBuiltin::Yank | EmacsBuiltin::YankPop)
        ) {
            self.state.emacs.last_yank = None;
        }
        match cmd {
            EmacsCommand::Herdr(action) => self.emacs_navigate(action, prefix),
            EmacsCommand::Builtin(builtin) => self.execute_emacs_builtin(builtin, prefix),
        }
    }

    fn execute_emacs_builtin(&mut self, builtin: EmacsBuiltin, prefix: Option<i64>) {
        match builtin {
            EmacsBuiltin::QuotedInsert => {
                if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                } else {
                    self.state.emacs.quoted_insert = true;
                    self.state.emacs.echo = Some("C-q-".to_string());
                }
            }
            EmacsBuiltin::KeyboardQuit => {
                self.state.emacs.pending.clear();
                if let Some(text) = self.state.emacs.text_mode.as_mut() {
                    text.mark_active = false;
                }
                self.state.emacs.echo = Some("Quit".to_string());
            }
            EmacsBuiltin::TextMode => {
                if self.state.emacs.text_mode.is_none() {
                    self.emacs_enter_text_mode();
                }
            }
            EmacsBuiltin::ExitTextMode => self.emacs_exit_text_mode(),
            EmacsBuiltin::ForwardChar
            | EmacsBuiltin::BackwardChar
            | EmacsBuiltin::NextLine
            | EmacsBuiltin::PreviousLine
            | EmacsBuiltin::ForwardWord
            | EmacsBuiltin::BackwardWord
            | EmacsBuiltin::MoveBeginningOfLine
            | EmacsBuiltin::MoveEndOfLine
            | EmacsBuiltin::ScrollUp
            | EmacsBuiltin::ScrollDown
            | EmacsBuiltin::BeginningOfBuffer
            | EmacsBuiltin::EndOfBuffer => self.emacs_text_motion(builtin, prefix),
            EmacsBuiltin::SetMark => self.emacs_set_mark(),
            EmacsBuiltin::ExchangePointAndMark => self.emacs_exchange_point_and_mark(),
            // In a read-only buffer C-w cannot delete, so kill-region
            // degrades to kill-ring-save.
            EmacsBuiltin::KillRingSave | EmacsBuiltin::KillRegion => self.emacs_kill_ring_save(),
            EmacsBuiltin::Yank => {
                if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                } else {
                    self.emacs_yank_live();
                }
            }
            EmacsBuiltin::YankPop => {
                if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                } else {
                    self.emacs_yank_pop_live();
                }
            }
            EmacsBuiltin::GotoLine => {
                if let Some(text) = self.state.emacs.text_mode.as_mut() {
                    text.goto_line = Some(String::new());
                }
            }
            // Wired in later tasks; named and reachable from M-x now.
            EmacsBuiltin::UniversalArgument
            | EmacsBuiltin::ExecuteExtendedCommand
            | EmacsBuiltin::DeleteBackwardChar
            | EmacsBuiltin::KillLine
            | EmacsBuiltin::BackwardKillWord
            | EmacsBuiltin::MinibufferComplete
            | EmacsBuiltin::ExitMinibuffer
            | EmacsBuiltin::DescribeKey
            | EmacsBuiltin::DescribeBindings => {
                self.state.emacs.echo = Some(format!(
                    "{} is not implemented yet",
                    EmacsCommand::Builtin(builtin).name()
                ));
            }
        }
    }

    /// Run a herdr action. The three indexed actions take their index from
    /// the prefix argument: `C-u 2 M-x switch-tab` is tab index 1 (the
    /// prefix arg is 1-based, herdr's index is 0-based).
    fn emacs_navigate(&mut self, action: NavigateAction, prefix: Option<i64>) {
        let action = if herdr_action_is_indexed(action) {
            let index = prefix.unwrap_or(1).max(1).saturating_sub(1) as usize;
            match action {
                NavigateAction::SwitchWorkspace(_) => NavigateAction::SwitchWorkspace(index),
                NavigateAction::SwitchTab(_) => NavigateAction::SwitchTab(index),
                NavigateAction::FocusAgent(_) => NavigateAction::FocusAgent(index),
                other => other,
            }
        } else {
            action
        };
        self.execute_tui_navigate_action(action, ActionContext::Prefix);
    }
```

Change `emacs_text_motion`'s signature and body head — replace

```rust
    fn emacs_text_motion(&mut self, cmd: EmacsCommand) {
```

with

```rust
    fn emacs_text_motion(&mut self, cmd: EmacsBuiltin, prefix: Option<i64>) {
        let _ = prefix; // Task 7 makes motions repeat.
```

and inside its `match cmd { ... }` change every `EmacsCommand::X =>` to `EmacsBuiltin::X =>` (12 arms: `ForwardChar`, `BackwardChar`, `NextLine`, `PreviousLine`, `ForwardWord`, `BackwardWord`, `MoveBeginningOfLine`, `MoveEndOfLine`, `ScrollUp`, `ScrollDown`, `BeginningOfBuffer`, `EndOfBuffer`, plus the `_ => point` catch-all).

Finally, in the dispatcher, the mid-chord C-g line becomes:

```rust
            self.execute_emacs_command(EmacsCommand::Builtin(EmacsBuiltin::KeyboardQuit), None);
```

- [ ] **Step 6: Write the failing "herdr action by name" adapter test** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    /// A herdr action that has no default binding at all becomes reachable
    /// purely by naming it in config (spec §7.5).
    #[tokio::test]
    async fn a_config_bound_herdr_action_runs_without_a_code_change() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        let mut keys = std::collections::HashMap::new();
        keys.insert("C-x t".to_string(), "toggle-sidebar".to_string());
        app.state.emacs = crate::emacs::EmacsState::from_config(&crate::config::EmacsConfig {
            enabled: true,
            clipboard_sync: false,
            keys,
            ..Default::default()
        });
        let before = app.state.sidebar_collapsed;
        app.route_client_input(vec![0x18, b't']); // C-x t
        assert_ne!(app.state.sidebar_collapsed, before, "toggle-sidebar ran");
        assert!(sent_bytes(&mut rx).is_empty());
    }
```

- [ ] **Step 7: Run the tests**

Run: `cargo test --locked --bin herdr emacs:: app::input::emacs`
Expected: `test result: ok.` on both. If `every_navigate_action_has_a_name` fails on the count, upstream changed `NavigateAction` — add the new variant to `herdr_command_table!` and bump `NAVIGATE_ACTION_COUNT`. That is the treadmill-ending compiler error working as designed.

- [ ] **Step 8: Prove the compiler guarantee by hand** (do not commit this)

Temporarily add a variant to `NavigateAction` in `src/app/input/navigate.rs`:

```rust
    OpenNavigator,
    ProbeVariant,
```

Run: `cargo build --locked 2>&1 | grep -A 3 "non-exhaustive"`
Expected: `error[E0004]: non-exhaustive patterns: \`NavigateAction::ProbeVariant\` not covered` pointing into `herdr_command_table!` in `src/emacs/commands.rs`.
Then revert: `git checkout -- src/app/input/navigate.rs` and re-run `cargo build --locked` to confirm it is clean again.

- [ ] **Step 9: Commit**

```bash
cargo fmt
git add src/emacs/commands.rs src/app/input/emacs.rs src/app/mod.rs src/app/input/mod.rs
git commit -m "feat: name every NavigateAction via an exhaustive match; split EmacsCommand into Builtin/Herdr"
```

---

### Task 5: Tab navigation and tab reordering (§3.8)

Four new default bindings:

```
C-[   previous-tab      C-]   next-tab
M-[   move-tab-left     M-]   move-tab-right
```

`previous-tab` / `next-tab` are existing `NavigateAction`s — they cost two table rows now that Task 4 landed. `move-tab-left` / `move-tab-right` are NEW builtins: herdr's `tab.move` API exists but is reachable only by mouse drag (`move_tab_via_api`, `src/app/input/mod.rs:334`), with no `NavigateAction` behind it. They clamp at the ends — **no wraparound**.

All four require the kitty keyboard protocol (`C-[` is byte 27 = `ESC` on a legacy terminal, and `M-[` is the CSI introducer). Task 2 pinned exactly that. The layer does not pretend otherwise: on a legacy terminal these bindings simply never fire, and `C-h b` still lists them.

**Files:**
- Modify: `src/emacs/commands.rs` (`EmacsBuiltin` + 2 names + `default_map` + 4 default global bindings)
- Modify: `src/app/input/emacs.rs` (2 executor arms + `emacs_move_tab`)
- Test: inline `#[cfg(test)]` in `src/emacs/commands.rs` and `src/app/input/emacs.rs`

**Interfaces:**
- Consumes: `App::move_tab_via_api(&mut self, ws_idx: usize, source_tab_idx: usize, insert_idx: usize)` (`src/app/input/navigate.rs:480`, `pub(crate)`). Its `insert_idx` is a **pre-removal** slot: `Workspace::move_tab` (`src/workspace.rs:577`) computes `target = if source < insert { insert - 1 } else { insert }`. So moving tab `i` **left** is `insert_idx = i - 1`, and moving it **right** is `insert_idx = i + 2`. `Workspace { pub tabs: Vec<Tab>, pub active_tab: usize }` (`src/workspace.rs:166`).
- Consumes: `Workspace::test_add_tab(&mut self, name: Option<&str>) -> usize` (`src/workspace.rs:1241`) — adds a tab with no PTY, for the fixture.
- Produces: `EmacsBuiltin::{MoveTabLeft, MoveTabRight}`, named `move-tab-left` / `move-tab-right`.

- [ ] **Step 1: Write the failing binding test** — append to `mod tests` in `src/emacs/commands.rs`:

```rust
    #[test]
    fn tab_navigation_and_reordering_are_bound_by_default() {
        let (keymaps, _) = build_keymaps(&Default::default());
        let cases = [
            ("C-[", herdr(NavigateAction::PreviousTab)),
            ("C-]", herdr(NavigateAction::NextTab)),
            ("M-[", builtin(EmacsBuiltin::MoveTabLeft)),
            ("M-]", builtin(EmacsBuiltin::MoveTabRight)),
        ];
        for (seq, cmd) in cases {
            assert_eq!(
                keymaps.global.lookup(&parse_key_seq(seq).unwrap()),
                Lookup::Bound(cmd),
                "global {seq}"
            );
            // ...and reachable from TEXT mode by fallthrough (spec §3.1).
            assert_eq!(
                keymaps.lookup(MapContext::Text, &parse_key_seq(seq).unwrap()),
                Lookup::Bound(cmd),
                "text fallthrough {seq}"
            );
        }
        // C-[ must NOT shadow ESC (which still exits TEXT mode) — they are
        // different chords (spec §3.2, one-directional fold).
        assert_eq!(
            keymaps.lookup(MapContext::Text, &parse_key_seq("ESC").unwrap()),
            Lookup::Bound(builtin(EmacsBuiltin::ExitTextMode))
        );
    }

    #[test]
    fn the_move_tab_commands_are_named_and_global() {
        assert_eq!(
            EmacsCommand::from_name("move-tab-left"),
            Some(builtin(EmacsBuiltin::MoveTabLeft))
        );
        assert_eq!(
            EmacsCommand::from_name("move-tab-right"),
            Some(builtin(EmacsBuiltin::MoveTabRight))
        );
        assert_eq!(EmacsBuiltin::MoveTabLeft.default_map(), MapSlot::Global);
        assert_eq!(EmacsBuiltin::MoveTabRight.default_map(), MapSlot::Global);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::commands`
Expected: compile error — `no variant MoveTabLeft on EmacsBuiltin`

- [ ] **Step 3: Add the commands.** In `src/emacs/commands.rs`, add to `EmacsBuiltin` (after `QuotedInsert,` in the `// Dispatcher` group):

```rust
    // Tab reordering (spec §3.8): herdr has a tab.move API but no
    // NavigateAction for it, so these are builtins.
    MoveTabLeft,
    MoveTabRight,
```

Add to `BUILTIN_NAMES` (keeping it sorted by name — between `minibuffer-complete` and `move-beginning-of-line`):

```rust
    (EmacsBuiltin::MoveTabLeft, "move-tab-left"),
    (EmacsBuiltin::MoveTabRight, "move-tab-right"),
```

Add them to the `MapSlot::Global` arm of `EmacsBuiltin::default_map` (the exhaustive match will not compile otherwise — that is the guarantee working):

```rust
            Self::UniversalArgument
            | Self::ExecuteExtendedCommand
            | Self::KeyboardQuit
            | Self::QuotedInsert
            | Self::MoveTabLeft
            | Self::MoveTabRight
            | Self::TextMode => MapSlot::Global,
```

Add the four bindings to `DEFAULT_GLOBAL_BINDINGS` (after the `C-x w` line):

```rust
    // Spec §3.8. Kitty-protocol only: on a legacy terminal C-[ is byte 27
    // (ESC) and M-[ is the CSI introducer, so these never fire there.
    ("C-[", EmacsCommand::Herdr(NavigateAction::PreviousTab)),
    ("C-]", EmacsCommand::Herdr(NavigateAction::NextTab)),
    ("M-[", EmacsCommand::Builtin(EmacsBuiltin::MoveTabLeft)),
    ("M-]", EmacsCommand::Builtin(EmacsBuiltin::MoveTabRight)),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::commands`
Expected: `test result: ok.` — except `every_command_appears_in_the_bindings_reference` does not exist yet (Task 11), so nothing else should fail.

- [ ] **Step 5: Write the failing dispatcher tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    /// Three tabs (no PTYs), the middle one focused.
    fn emacs_app_with_three_tabs() -> (App, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let (mut app, _pane, rx) = emacs_app_with_channel(b"");
        let ws = &mut app.state.workspaces[0];
        ws.test_add_tab(Some("b"));
        ws.test_add_tab(Some("c"));
        assert_eq!(ws.tabs.len(), 3);
        ws.active_tab = 1;
        (app, rx)
    }

    fn tab_names(app: &App) -> Vec<String> {
        app.state.workspaces[0]
            .tabs
            .iter()
            .map(|tab| tab.custom_name.clone().unwrap_or_else(|| "a".to_string()))
            .collect()
    }

    /// Kitty encodings: C-[ = CSI 91;5u, C-] = CSI 93;5u.
    const KITTY_C_LBRACKET: &[u8] = b"\x1b[91;5u";
    const KITTY_C_RBRACKET: &[u8] = b"\x1b[93;5u";
    /// M-[ = CSI 91;3u, M-] = CSI 93;3u.
    const KITTY_M_LBRACKET: &[u8] = b"\x1b[91;3u";
    const KITTY_M_RBRACKET: &[u8] = b"\x1b[93;3u";

    #[tokio::test]
    async fn c_bracket_moves_between_tabs() {
        let (mut app, mut rx) = emacs_app_with_three_tabs();
        assert_eq!(app.state.workspaces[0].active_tab, 1);
        app.route_client_input(KITTY_C_LBRACKET.to_vec()); // C-[ : previous-tab
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        app.route_client_input(KITTY_C_RBRACKET.to_vec()); // C-] : next-tab
        assert_eq!(app.state.workspaces[0].active_tab, 1);
        app.route_client_input(KITTY_C_RBRACKET.to_vec());
        assert_eq!(app.state.workspaces[0].active_tab, 2);
        assert!(sent_bytes(&mut rx).is_empty(), "chords never reach the pane");
    }

    #[tokio::test]
    async fn m_bracket_reorders_tabs_and_the_moved_tab_stays_focused() {
        let (mut app, mut rx) = emacs_app_with_three_tabs();
        assert_eq!(tab_names(&app), vec!["a", "b", "c"]);

        app.route_client_input(KITTY_M_LBRACKET.to_vec()); // M-[ : move-tab-left
        assert_eq!(tab_names(&app), vec!["b", "a", "c"]);
        assert_eq!(
            app.state.workspaces[0].active_tab, 0,
            "the moved tab keeps focus"
        );

        app.route_client_input(KITTY_M_RBRACKET.to_vec()); // M-] : move-tab-right
        assert_eq!(tab_names(&app), vec!["a", "b", "c"]);
        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert!(sent_bytes(&mut rx).is_empty());
    }

    /// Spec §3.8: clamp at the ends, no wraparound.
    #[tokio::test]
    async fn move_tab_clamps_at_both_ends_without_wrapping() {
        let (mut app, _rx) = emacs_app_with_three_tabs();
        app.state.workspaces[0].active_tab = 0;
        app.route_client_input(KITTY_M_LBRACKET.to_vec()); // M-[ at the left edge
        assert_eq!(tab_names(&app), vec!["a", "b", "c"], "no wraparound");
        assert_eq!(app.state.workspaces[0].active_tab, 0);

        app.state.workspaces[0].active_tab = 2;
        app.route_client_input(KITTY_M_RBRACKET.to_vec()); // M-] at the right edge
        assert_eq!(tab_names(&app), vec!["a", "b", "c"], "no wraparound");
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    /// All four work from TEXT mode, by fallthrough to the global map.
    #[tokio::test]
    async fn tab_bindings_work_from_text_mode() {
        let (mut app, _rx) = emacs_app_with_three_tabs();
        // TEXT mode needs the focused pane's runtime, which lives in tab 0.
        app.state.workspaces[0].active_tab = 0;
        enter_text_mode(&mut app);
        assert!(app.state.emacs.text_mode.is_some());
        app.route_client_input(KITTY_M_RBRACKET.to_vec()); // M-]
        assert_eq!(tab_names(&app), vec!["b", "a", "c"]);
    }

    /// ESC is still exit-text-mode — a legacy byte 27 must NOT be read as C-[.
    #[tokio::test]
    async fn legacy_esc_still_exits_text_mode_and_does_not_switch_tabs() {
        let (mut app, _rx) = emacs_app_with_three_tabs();
        app.state.workspaces[0].active_tab = 0;
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b]); // legacy ESC
        assert!(app.state.emacs.text_mode.is_none(), "ESC exited TEXT mode");
        assert_eq!(
            app.state.workspaces[0].active_tab, 0,
            "ESC is not C-[ : no tab switch"
        );
    }
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `m_bracket_reorders_tabs_and_the_moved_tab_stays_focused` FAILS (`move-tab-left` echoes "is not implemented yet"; the tab order is unchanged). `c_bracket_moves_between_tabs` should already PASS — `previous-tab`/`next-tab` came free with Task 4.

- [ ] **Step 7: Implement the two builtins.** In `src/app/input/emacs.rs`, replace the `EmacsBuiltin::UniversalArgument | ... => { "is not implemented yet" }` catch-all arm's variant list so `MoveTabLeft`/`MoveTabRight` are handled, by adding these two arms ABOVE it:

```rust
            EmacsBuiltin::MoveTabLeft => self.emacs_move_tab(-1),
            EmacsBuiltin::MoveTabRight => self.emacs_move_tab(1),
```

and add the method inside `impl App`, next to `emacs_navigate`:

```rust
    /// `M-[` / `M-]` — reorder the active tab. herdr exposes tab.move only
    /// through mouse drag (`move_tab_via_api`), so this is a builtin rather
    /// than a `NavigateAction` (spec §3.8).
    ///
    /// `Workspace::move_tab(source, insert)` takes a PRE-removal slot:
    /// `target = if source < insert { insert - 1 } else { insert }`. So
    /// left is `source - 1` and right is `source + 2`. Clamped at both ends
    /// — no wraparound.
    fn emacs_move_tab(&mut self, delta: i64) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        let source = ws.active_tab;
        let last = ws.tabs.len().saturating_sub(1);
        let insert_idx = if delta < 0 {
            if source == 0 {
                return; // already leftmost: no-op, no wraparound
            }
            source - 1
        } else {
            if source >= last {
                return; // already rightmost: no-op, no wraparound
            }
            source + 2
        };
        self.move_tab_via_api(ws_idx, source, insert_idx);
    }
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs:: app::input::emacs`
Expected: `test result: ok.`

- [ ] **Step 9: Commit**

```bash
cargo fmt
git add src/emacs/commands.rs src/app/input/emacs.rs
git commit -m "feat: C-[/C-] tab navigation and M-[/M-] tab reordering"
```

---

### Task 6: `[emacs.keys]` errors route through config diagnostics

Today `build_keymaps`'s warnings go to `tracing::warn!` — invisible. Route them through the same pipeline as `[keys]` errors: `Config::collect_diagnostics()` at startup, `apply_live_config`'s `diagnostics` vec on reload (spec §4).

**Files:**
- Modify: `src/config/model.rs` (`EmacsConfig::binding_diagnostics`)
- Modify: `src/config.rs` (`Config::collect_diagnostics` chain, ~:69-78)
- Modify: `src/emacs/mod.rs` (`from_config` / `apply_config` stop logging; `apply_config` returns the warnings)
- Modify: `src/app/mod.rs` (`apply_live_config` emacs branch, ~:1366)
- Test: inline `#[cfg(test)]` in `src/config/model.rs` and `src/app/mod.rs`

**Interfaces:**
- Consumes: `commands::build_keymaps` (Task 4).
- Produces: `EmacsConfig::binding_diagnostics(&self) -> Vec<String>`; `EmacsState::apply_config(&mut self, &EmacsConfig) -> Vec<String>` (was `()`).

- [ ] **Step 1: Write the failing startup-diagnostics test** — in the `#[cfg(test)] mod tests` at the bottom of `src/config/model.rs`, after `emacs_config_defaults_and_parses`:

```rust
    #[test]
    fn emacs_binding_errors_become_config_diagnostics() {
        let parsed: Config = toml::from_str(
            r#"
[emacs]
enabled = true

[emacs.keys]
"C-x t" = "no-such-command"
"???" = "new-tab"
"C-x 4" = "split-window-right"
"#,
        )
        .expect("emacs config parses");

        let diagnostics = parsed.collect_diagnostics();
        assert!(
            diagnostics
                .iter()
                .any(|d| d.contains("unknown command \"no-such-command\"")),
            "{diagnostics:?}"
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.contains("invalid key sequence \"???\"")),
            "{diagnostics:?}"
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|d| d.starts_with("[emacs.keys]"))
                .count(),
            2,
            "the valid binding produces no diagnostic: {diagnostics:?}"
        );
    }

    #[test]
    fn a_clean_emacs_config_produces_no_diagnostics() {
        let parsed: Config = toml::from_str(
            r#"
[emacs]
enabled = true

[emacs.keys]
"C-x 4" = "split-window-right"
"#,
        )
        .expect("emacs config parses");
        assert!(
            parsed
                .collect_diagnostics()
                .iter()
                .all(|d| !d.starts_with("[emacs.keys]")),
            "{:?}",
            parsed.collect_diagnostics()
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr config::model::tests::emacs`
Expected: FAIL — `collect_diagnostics()` never mentions `[emacs.keys]`

- [ ] **Step 3: Add `binding_diagnostics`.** In `src/config/model.rs`, directly after the `impl Default for EmacsConfig` block:

```rust
impl EmacsConfig {
    /// Binding errors, in the shape the diagnostics pipeline expects.
    /// Same source of truth as the live keymaps (`build_keymaps`), so a
    /// diagnostic and a dropped binding can never disagree.
    pub fn binding_diagnostics(&self) -> Vec<String> {
        crate::emacs::commands::build_keymaps(&self.keys).1
    }
}
```

In `src/config.rs`, extend the `collect_diagnostics` chain (add the emacs link after `ui.sound`):

```rust
    pub fn collect_diagnostics(&self) -> Vec<String> {
        let (prefix_diag, _, keybind_diags, _) = self.validated_keybinds();
        prefix_diag
            .into_iter()
            .chain(keybind_diags)
            .chain(self.remote_image_paste_key().err())
            .chain(self.ui.sound.diagnostics())
            // Emacs layer seam (fork).
            .chain(self.emacs.binding_diagnostics())
            .chain(self.invalid_sidebar_bounds_diagnostic())
            .collect()
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr config::`
Expected: `test result: ok.`

- [ ] **Step 5: Write the failing live-reload test** — in the `#[cfg(test)] mod tests` of `src/app/mod.rs`, next to the existing `report.diagnostics` tests (~:2937):

```rust
    #[tokio::test]
    async fn emacs_binding_errors_reach_the_reload_report() {
        let mut app = App::test_new();
        let mut keys = std::collections::HashMap::new();
        keys.insert("C-x t".to_string(), "no-such-command".to_string());
        let config = crate::config::Config {
            emacs: crate::config::EmacsConfig {
                enabled: true,
                keys,
                ..Default::default()
            },
            ..Default::default()
        };

        let report = app.apply_live_config(&config, &[], &[], false);

        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.contains("[emacs.keys] unknown command \"no-such-command\"")),
            "{:?}",
            report.diagnostics
        );
        assert_eq!(report.status, crate::config::ConfigReloadStatus::Partial);
        assert!(
            app.state.config_diagnostic.is_some(),
            "a bad binding surfaces in the UI, not just the log"
        );
    }
```

If `App::test_new()` does not exist under that name, use the same constructor the neighbouring tests at `src/app/mod.rs:2937` use (read them and copy the fixture line verbatim).

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test --locked --bin herdr app::tests::emacs_binding_errors_reach_the_reload_report`
Expected: FAIL — `report.diagnostics` is empty; status is `Applied`

- [ ] **Step 7: Return the warnings from `apply_config`.** In `src/emacs/mod.rs`, replace `from_config` and `apply_config`:

```rust
    pub fn from_config(config: &EmacsConfig) -> Self {
        // Warnings are NOT logged here: they are surfaced by
        // `EmacsConfig::binding_diagnostics()` through the config
        // diagnostics pipeline (spec §4).
        let (keymaps, _warnings) = commands::build_keymaps(&config.keys);
        Self {
            enabled: config.enabled,
            clipboard_sync: config.clipboard_sync,
            kill_ring_max: config.kill_ring_max.max(1),
            mark_ring_max: config.mark_ring_max.max(1),
            keymaps,
            pending: Vec::new(),
            quoted_insert: false,
            echo: None,
            text_mode: None,
            kill_ring: rings::KillRing::new(config.kill_ring_max.max(1)),
            mark_rings: std::collections::HashMap::new(),
            last_yank: None,
        }
    }

    /// Live config reload: refresh config-derived fields, preserve runtime
    /// state (rings survive a reload); drop transient state when disabling.
    /// Returns binding warnings for the caller's diagnostics vec.
    pub fn apply_config(&mut self, config: &EmacsConfig) -> Vec<String> {
        let (keymaps, warnings) = commands::build_keymaps(&config.keys);
        self.enabled = config.enabled;
        self.clipboard_sync = config.clipboard_sync;
        self.kill_ring_max = config.kill_ring_max.max(1);
        self.mark_ring_max = config.mark_ring_max.max(1);
        self.kill_ring.set_max(config.kill_ring_max.max(1));
        for ring in self.mark_rings.values_mut() {
            ring.set_max(self.mark_ring_max);
        }
        self.keymaps = keymaps;
        if !self.enabled {
            self.pending.clear();
            self.quoted_insert = false;
            self.echo = None;
            self.text_mode = None;
            self.last_yank = None;
        }
        warnings
    }
```

In `src/app/mod.rs`, replace the emacs branch of `apply_live_config`:

```rust
        // Emacs layer seam (fork).
        if !invalid_section("emacs") {
            diagnostics.extend(self.state.emacs.apply_config(&config.emacs));
        }
```

- [ ] **Step 8: Fix the one call site that ignored the return value.** `src/app/input/emacs.rs`'s test `disabled_layer_is_bit_for_bit_passthrough` calls `apply_config` for effect; a non-`()` return in statement position is a warning-free no-op in Rust only if bound. Change that line to:

```rust
        let _ = app
            .state
            .emacs
            .apply_config(&crate::config::EmacsConfig::default());
```

Do the same in `src/emacs/mod.rs`'s `apply_config_trims_existing_pane_mark_rings` test (both `state.apply_config(...)` calls).

- [ ] **Step 9: Run the tests**

Run: `cargo test --locked --bin herdr emacs:: config:: app::input::emacs`
Expected: `test result: ok.`

Run: `cargo test --locked --bin herdr app::tests::emacs_binding_errors_reach_the_reload_report`
Expected: `test result: ok. 1 passed`

- [ ] **Step 10: Commit**

```bash
cargo fmt
git add src/config/model.rs src/config.rs src/emacs/mod.rs src/app/mod.rs src/app/input/emacs.rs
git commit -m "feat: route [emacs.keys] binding errors through the config diagnostics pipeline"
```

---

### Task 7: Prefix arguments (`C-u`, `M-<digit>`)

`C-u` (chainable: `C-u C-u` = 16) and `M-<digit>`, threaded into the `Option<i64>` slot `execute_emacs_command` has carried since Phase 1. Motions repeat; `C-u C-SPC` pops the mark ring (spec §3.6).

**Files:**
- Modify: `src/emacs/mod.rs` (`PrefixReader`, two `EmacsState` fields)
- Modify: `src/emacs/commands.rs` (`DEFAULT_GLOBAL_BINDINGS`: `C-u`)
- Modify: `src/emacs/render.rs` (echo the reader)
- Modify: `src/app/input/emacs.rs` (reader stage, motion repeat, `C-u C-SPC`)
- Test: inline `#[cfg(test)]` in `src/emacs/mod.rs` and `src/app/input/emacs.rs`

**Interfaces:**
- Consumes: `EmacsBuiltin::UniversalArgument` (Task 4), `Chord` (Task 2).
- Produces:
  - `emacs::PrefixReader { pub count: i64, pub has_digits: bool, pub keys: Vec<Chord> }`
  - `PrefixReader::start_universal() -> Self`, `PrefixReader::start_digit(d: i64, chord: Chord) -> Self`
  - `PrefixReader::push_universal(&mut self, chord: Chord)`, `PrefixReader::push_digit(&mut self, d: i64, chord: Chord)`
  - `PrefixReader::echo(&self) -> String`
  - `EmacsState::prefix: Option<PrefixReader>`, `EmacsState::prefix_arg: Option<i64>`
  - `App::emacs_take_prefix_arg(&mut self) -> Option<i64>`

- [ ] **Step 1: Write the failing `PrefixReader` unit tests** — append to `mod tests` in `src/emacs/mod.rs`:

```rust
    use crate::emacs::keymap::parse_chord;

    #[test]
    fn universal_argument_chains_by_four() {
        let c_u = parse_chord("C-u").unwrap();
        let mut reader = PrefixReader::start_universal();
        assert_eq!(reader.count, 4);
        assert_eq!(reader.echo(), "C-u-");
        reader.push_universal(c_u);
        assert_eq!(reader.count, 16, "C-u C-u = 16");
        assert_eq!(reader.echo(), "C-u C-u-");
        reader.push_universal(c_u);
        assert_eq!(reader.count, 64);
    }

    #[test]
    fn digits_replace_then_accumulate() {
        let c_u = parse_chord("C-u").unwrap();
        let mut reader = PrefixReader::start_universal();
        reader.push_digit(5, parse_chord("5").unwrap());
        assert_eq!(reader.count, 5, "the first digit replaces the default 4");
        assert!(reader.has_digits);
        reader.push_digit(3, parse_chord("3").unwrap());
        assert_eq!(reader.count, 53, "later digits accumulate");
        assert_eq!(reader.echo(), "C-u 5 3-");
        // C-u after digits is a no-op on the count (Emacs multiplies only
        // while the argument is still the implicit one).
        reader.push_universal(c_u);
        assert_eq!(reader.count, 53);
    }

    #[test]
    fn meta_digit_starts_an_argument() {
        let mut reader = PrefixReader::start_digit(5, parse_chord("M-5").unwrap());
        assert_eq!(reader.count, 5);
        assert!(reader.has_digits);
        assert_eq!(reader.echo(), "M-5-");
        reader.push_digit(3, parse_chord("3").unwrap());
        assert_eq!(reader.count, 53);
    }

    #[test]
    fn the_count_saturates_instead_of_overflowing() {
        let mut reader = PrefixReader::start_digit(9, parse_chord("M-9").unwrap());
        for _ in 0..40 {
            reader.push_digit(9, parse_chord("9").unwrap());
        }
        assert_eq!(reader.count, PrefixReader::MAX_COUNT);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::tests`
Expected: compile error — `cannot find type PrefixReader`

- [ ] **Step 3: Implement `PrefixReader`** in `src/emacs/mod.rs`, directly above `pub struct EmacsState`:

```rust
/// The universal-argument reader (spec §3.6). Emacs implements this with a
/// transient `universal-argument-map`; the layer implements it as a reader
/// stage in the dispatcher, which is the same thing without the ceremony.
#[derive(Debug, Clone)]
pub struct PrefixReader {
    /// The argument so far.
    pub count: i64,
    /// True once an explicit digit has been typed (so `C-u` stops
    /// multiplying and further digits accumulate).
    pub has_digits: bool,
    /// The chords typed so far, for the echo area.
    pub keys: Vec<Chord>,
}

impl PrefixReader {
    /// Clamped so a wall of digits cannot overflow or hang a motion loop.
    pub const MAX_COUNT: i64 = 1_000_000;

    /// `C-u`: the implicit argument 4.
    pub fn start_universal() -> Self {
        Self {
            count: 4,
            has_digits: false,
            keys: vec![Chord {
                ctrl: true,
                meta: false,
                code: crossterm::event::KeyCode::Char('u'),
            }],
        }
    }

    /// `M-<digit>`: an explicit argument.
    pub fn start_digit(digit: i64, chord: Chord) -> Self {
        Self {
            count: digit,
            has_digits: true,
            keys: vec![chord],
        }
    }

    /// Another `C-u`: multiply by four, but only while the argument is
    /// still implicit (Emacs: `C-u C-u` = 16, `C-u 5 C-u` = 5).
    pub fn push_universal(&mut self, chord: Chord) {
        if !self.has_digits {
            self.count = self.count.saturating_mul(4).min(Self::MAX_COUNT);
        }
        self.keys.push(chord);
    }

    /// A digit: the first replaces the implicit argument, later ones
    /// accumulate decimally.
    pub fn push_digit(&mut self, digit: i64, chord: Chord) {
        if self.has_digits {
            self.count = self
                .count
                .saturating_mul(10)
                .saturating_add(digit)
                .min(Self::MAX_COUNT);
        } else {
            self.count = digit;
            self.has_digits = true;
        }
        self.keys.push(chord);
    }

    /// Echo-area rendering: the literal keys typed, then a trailing dash.
    pub fn echo(&self) -> String {
        format!("{}-", keymap::format_seq(&self.keys))
    }
}
```

Add the two fields to `EmacsState` (after `pub quoted_insert: bool,`):

```rust
    /// Active universal-argument reader (`C-u` / `M-<digit>`).
    pub prefix: Option<PrefixReader>,
    /// The argument the next command will receive, once the reader closed.
    pub prefix_arg: Option<i64>,
```

Initialize both in `from_config` (`prefix: None, prefix_arg: None,`) and clear both in `apply_config`'s `if !self.enabled { ... }` block:

```rust
            self.prefix = None;
            self.prefix_arg = None;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::tests`
Expected: `test result: ok. 5 passed`

- [ ] **Step 5: Write the failing dispatcher tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    /// Spec §7.4: C-u 5 C-f moves point five characters.
    #[tokio::test]
    async fn c_u_5_c_f_moves_five_characters() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-< : (0, 0)
        app.route_client_input(vec![0x15]); // C-u
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-u-"));
        app.route_client_input(b"5".to_vec()); // digit
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-u 5-"));
        app.route_client_input(vec![0x06]); // C-f
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!((point.row, point.col), (0, 5));
        assert_eq!(app.state.emacs.prefix_arg, None, "argument consumed");
        assert!(app.state.emacs.prefix.is_none());
    }

    /// M-<digit> is the shorthand form.
    #[tokio::test]
    async fn m_3_c_f_moves_three_characters() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        app.route_client_input(vec![0x1b, b'3']); // M-3
        app.route_client_input(vec![0x06]); // C-f
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!((point.row, point.col), (0, 3));
    }

    /// C-u C-u = 16 (chaining).
    #[tokio::test]
    async fn c_u_c_u_is_sixteen() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x15, 0x15]); // C-u C-u
        let reader = app.state.emacs.prefix.as_ref().expect("reader open");
        assert_eq!(reader.count, 16);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-u C-u-"));
    }

    /// A prefix argument survives a multi-chord sequence.
    #[tokio::test]
    async fn a_prefix_argument_reaches_a_multi_chord_command() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        app.route_client_input(vec![0x00]); // C-SPC : push (0,0)
        app.route_client_input(vec![0x0e]); // C-n : point (1,0)
        app.route_client_input(vec![0x00]); // C-SPC : push (1,0)
        app.route_client_input(vec![0x0e]); // C-n : point (2,0)
        // Spec §3.6: C-u C-SPC pops the mark ring and jumps there.
        app.route_client_input(vec![0x15, 0x00]); // C-u C-SPC
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!(
            (text.point.row, text.point.col),
            (1, 0),
            "point jumped to the last pushed mark"
        );
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Mark popped"));
        app.route_client_input(vec![0x15, 0x00]); // C-u C-SPC again
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!((text.point.row, text.point.col), (0, 0), "and again");
    }

    /// C-u with an empty mark ring says so instead of moving.
    #[tokio::test]
    async fn c_u_c_spc_with_an_empty_mark_ring_complains() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x15, 0x00]); // C-u C-SPC
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("No mark set in this buffer")
        );
    }

    /// C-g abandons a half-typed argument.
    #[tokio::test]
    async fn c_g_abandons_the_prefix_argument() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x15]); // C-u
        app.route_client_input(b"7".to_vec());
        app.route_client_input(vec![0x07]); // C-g
        assert!(app.state.emacs.prefix.is_none());
        assert_eq!(app.state.emacs.prefix_arg, None);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Quit"));
        assert!(sent_bytes(&mut rx).is_empty(), "digits never reach the PTY");
    }

    /// A digit typed with NO argument in progress is an ordinary key.
    #[tokio::test]
    async fn a_bare_digit_is_not_an_argument() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(b"5".to_vec());
        assert!(app.state.emacs.prefix.is_none());
        assert_eq!(sent_bytes(&mut rx), b"5".to_vec());
    }

    /// C-u then a herdr action: the argument is simply ignored by actions
    /// that do not take one, and the action still runs.
    #[tokio::test]
    async fn a_prefix_argument_before_an_unindexed_action_is_harmless() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x15]); // C-u
        app.route_client_input(vec![0x18, b'b']); // C-x b
        assert_eq!(app.state.mode, Mode::Navigator);
        assert_eq!(app.state.emacs.prefix_arg, None, "argument consumed");
    }
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: FAIL — `C-u` is unbound (`no field prefix` compiles now, but `C-u` echoes `is undefined`), motions do not repeat, `C-u C-SPC` sets a mark instead of popping.

- [ ] **Step 7: Bind `C-u`.** In `src/emacs/commands.rs`, add to `DEFAULT_GLOBAL_BINDINGS` (after the `C-q` line):

```rust
    ("C-u", EmacsCommand::Builtin(EmacsBuiltin::UniversalArgument)),
```

- [ ] **Step 8: Add the reader stage to the dispatcher.** In `src/app/input/emacs.rs`, insert this block in `emacs_intercept_key` immediately AFTER the `let Some(chord) = Chord::from_key(&key) else { ... };` line and BEFORE the mid-chord C-g check:

```rust
        // Emacs layer seam (fork): the universal-argument reader (spec §3.6).
        // Runs before keymap lookup, like Emacs's transient
        // `universal-argument-map`. It only ever swallows digits while an
        // argument is actually being read.
        if self.state.emacs.pending.is_empty() {
            if let Some(digit) = emacs_meta_digit(chord) {
                match self.state.emacs.prefix.as_mut() {
                    Some(reader) => reader.push_digit(digit, chord),
                    None => {
                        self.state.emacs.prefix =
                            Some(crate::emacs::PrefixReader::start_digit(digit, chord));
                    }
                }
                self.state.emacs.echo =
                    self.state.emacs.prefix.as_ref().map(|reader| reader.echo());
                return true;
            }
            if self.state.emacs.prefix.is_some() {
                if let Some(digit) = emacs_plain_digit(chord) {
                    if let Some(reader) = self.state.emacs.prefix.as_mut() {
                        reader.push_digit(digit, chord);
                    }
                    self.state.emacs.echo =
                        self.state.emacs.prefix.as_ref().map(|reader| reader.echo());
                    return true;
                }
            }
        }
```

Add the two helper functions at the bottom of the file, next to `RuntimeBuffer` (outside `impl App`):

```rust
/// `M-1` ... `M-9`, `M-0` — the digit-argument chords.
fn emacs_meta_digit(chord: Chord) -> Option<i64> {
    match chord.code {
        crossterm::event::KeyCode::Char(c @ '0'..='9') if chord.meta && !chord.ctrl => {
            Some(i64::from(c as u8 - b'0'))
        }
        _ => None,
    }
}

/// A bare digit — an argument continuation ONLY while a reader is open.
fn emacs_plain_digit(chord: Chord) -> Option<i64> {
    match chord.code {
        crossterm::event::KeyCode::Char(c @ '0'..='9') if !chord.meta && !chord.ctrl => {
            Some(i64::from(c as u8 - b'0'))
        }
        _ => None,
    }
}
```

- [ ] **Step 9: Close the reader when a real command starts.** Still in `emacs_intercept_key`, replace the `Lookup::Bound(cmd)` arm of the `match`. `C-u` must EXTEND the reader (chaining), so it is checked before the reader is consumed:

```rust
            Lookup::Bound(cmd) => {
                self.state.emacs.pending.clear();
                // `C-u` extends the argument it is building instead of
                // consuming it (`C-u C-u` = 16).
                if cmd == EmacsCommand::Builtin(EmacsBuiltin::UniversalArgument) {
                    match self.state.emacs.prefix.as_mut() {
                        Some(reader) => reader.push_universal(chord),
                        None => {
                            self.state.emacs.prefix =
                                Some(crate::emacs::PrefixReader::start_universal());
                        }
                    }
                    self.state.emacs.echo =
                        self.state.emacs.prefix.as_ref().map(|reader| reader.echo());
                    return true;
                }
                let arg = self.emacs_take_prefix_arg();
                self.execute_emacs_command(cmd, arg);
                true
            }
```

Add `emacs_take_prefix_arg` inside `impl App`, next to `emacs_focused_pane`. Note what it does NOT touch: `prefix_arg` is a separate holding slot, written only by `M-x` (Task 8) and consumed only by `emacs_minibuffer_exit`. Clearing it here would wipe the argument a `C-u 2 M-x switch-tab` is holding across the prompt.

```rust
    /// Close the universal-argument reader, if one is open, and hand its
    /// value to the command about to run.
    fn emacs_take_prefix_arg(&mut self) -> Option<i64> {
        self.state.emacs.prefix.take().map(|reader| reader.count)
    }
```

Also clear the reader on the two paths that abandon input — in the `Lookup::Unbound` arm add, as its first line:

```rust
                self.state.emacs.prefix = None;
```

and in `execute_emacs_builtin`'s `EmacsBuiltin::KeyboardQuit` arm add, before setting the echo:

```rust
                self.state.emacs.prefix = None;
                self.state.emacs.prefix_arg = None;
```

- [ ] **Step 10: Make motions repeat and `C-u C-SPC` pop the mark ring.** In `src/app/input/emacs.rs`, replace the head of `emacs_text_motion`:

```rust
    /// Run one motion command against the frozen buffer `prefix` times
    /// (default 1), then keep the point visible.
    fn emacs_text_motion(&mut self, cmd: EmacsBuiltin, prefix: Option<i64>) {
        let repeat = prefix
            .unwrap_or(1)
            .clamp(1, crate::emacs::PrefixReader::MAX_COUNT) as usize;
        for _ in 0..repeat {
            self.emacs_text_motion_once(cmd);
        }
    }

    fn emacs_text_motion_once(&mut self, cmd: EmacsBuiltin) {
```

(the rest of the old body, from `let Some(text) = self.state.emacs.text_mode.as_ref() else {` onward, becomes the body of `emacs_text_motion_once` unchanged).

Then split `set-mark`. Replace the `EmacsBuiltin::SetMark` arm of `execute_emacs_builtin`:

```rust
            EmacsBuiltin::SetMark => {
                if prefix.is_some() {
                    self.emacs_pop_mark();
                } else {
                    self.emacs_set_mark();
                }
            }
```

and add `emacs_pop_mark` directly after `emacs_set_mark`:

```rust
    /// `C-u C-SPC` — pop the pane's mark ring and move point there
    /// (spec §3.6).
    fn emacs_pop_mark(&mut self) {
        let Some(pane_id) = self.state.emacs.text_mode.as_ref().map(|text| text.pane_id) else {
            return;
        };
        let popped = self
            .state
            .emacs
            .mark_rings
            .get_mut(&pane_id)
            .and_then(|ring| ring.pop());
        let Some((row, col)) = popped else {
            self.state.emacs.echo = Some("No mark set in this buffer".to_string());
            return;
        };
        if let Some(text) = self.state.emacs.text_mode.as_mut() {
            text.point = Pos { row, col };
            text.mark_active = false;
        }
        self.state.emacs.echo = Some("Mark popped".to_string());
        self.emacs_scroll_point_into_view(pane_id);
    }
```

`MarkRing::pop` may not exist. Check `src/emacs/rings.rs`; if it does not, add it to `impl MarkRing` (the ring stores `(u32, u32)` positions, newest last):

```rust
    /// Remove and return the newest mark (`C-u C-SPC`).
    pub fn pop(&mut self) -> Option<(u32, u32)> {
        self.entries.pop_back()
    }
```

Adjust the field/collection name to whatever `MarkRing` actually uses — read `src/emacs/rings.rs` first and match its existing `push`/`len` implementation exactly.

- [ ] **Step 11: Echo the reader.** In `src/emacs/render.rs`, `render_echo_area`, add a branch so an open reader shows even after `echo` is cleared. Replace the `let content = ...` chain's first two arms:

```rust
    let content = if let Some(prompt) = app
        .emacs
        .text_mode
        .as_ref()
        .and_then(|text| text.goto_line.as_deref())
    {
        format!("Goto line: {prompt}")
    } else if let Some(echo) = app.emacs.echo.as_deref() {
        echo.to_string()
    } else if let Some(reader) = app.emacs.prefix.as_ref() {
        reader.echo()
    } else if !app.emacs.pending.is_empty() {
        format!("{}-", crate::emacs::keymap::format_seq(&app.emacs.pending))
    } else {
        return;
    };
```

- [ ] **Step 12: Run the tests**

Run: `cargo test --locked --bin herdr app::input::emacs emacs::`
Expected: `test result: ok.`

- [ ] **Step 13: Commit**

```bash
cargo fmt
git add src/emacs src/app/input/emacs.rs
git commit -m "feat: universal argument (C-u, M-<digit>) with repeating motions and C-u C-SPC"
```

---

### Task 8: Minibuffer core + `M-x` by name

A one-line editable prompt in the echo area, with its own keymap **on the stack** (not a special case in the dispatcher), `C-g` to abort (spec §3.5). Completion is Task 9.

**Files:**
- Create: `src/emacs/minibuffer.rs`
- Modify: `src/emacs/mod.rs` (`pub mod minibuffer;`, `minibuffer` field, `map_context`)
- Modify: `src/emacs/commands.rs` (`DEFAULT_MINIBUFFER_BINDINGS`, `M-x` in global)
- Modify: `src/emacs/render.rs` (draw the prompt)
- Modify: `src/app/input/emacs.rs` (open/abort/execute, self-insert routing)
- Test: inline `#[cfg(test)]` in `src/emacs/minibuffer.rs` and `src/app/input/emacs.rs`

**Interfaces:**
- Produces:
  - `minibuffer::MinibufferState { pub prompt: String, pub input: String, pub cursor: usize, pub candidates: Vec<&'static str>, pub selected: usize }` — `cursor` is a **char** index into `input`
  - `MinibufferState::new(prompt: &str) -> Self`
  - Pure editing: `insert_char(&mut self, c: char)`, `delete_backward_char(&mut self)`, `kill_line(&mut self)`, `backward_kill_word(&mut self)`, `move_beginning_of_line(&mut self)`, `move_end_of_line(&mut self)`, `forward_char(&mut self)`, `backward_char(&mut self)`, `insert_str(&mut self, s: &str)`
  - `MinibufferState::render_line(&self) -> String`
  - `EmacsState::minibuffer: Option<MinibufferState>`

- [ ] **Step 1: Write the failing editing tests.** Create `src/emacs/minibuffer.rs` with ONLY the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn mb(input: &str) -> MinibufferState {
        let mut state = MinibufferState::new("M-x ");
        state.insert_str(input);
        state
    }

    #[test]
    fn insert_and_delete_at_the_cursor() {
        let mut state = mb("abc");
        assert_eq!(state.input, "abc");
        assert_eq!(state.cursor, 3);
        state.delete_backward_char();
        assert_eq!(state.input, "ab");
        assert_eq!(state.cursor, 2);
        state.backward_char();
        state.insert_char('X');
        assert_eq!(state.input, "aXb");
        assert_eq!(state.cursor, 2);
        // Deleting at the very beginning is a no-op, not a panic.
        state.move_beginning_of_line();
        state.delete_backward_char();
        assert_eq!(state.input, "aXb");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn motions_clamp_at_both_ends() {
        let mut state = mb("hi");
        state.move_beginning_of_line();
        assert_eq!(state.cursor, 0);
        state.backward_char();
        assert_eq!(state.cursor, 0);
        state.move_end_of_line();
        assert_eq!(state.cursor, 2);
        state.forward_char();
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn kill_line_kills_to_the_end() {
        let mut state = mb("split-window");
        state.move_beginning_of_line();
        state.forward_char();
        state.forward_char();
        state.kill_line();
        assert_eq!(state.input, "sp");
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn backward_kill_word_eats_one_word_and_its_separators() {
        let mut state = mb("split-window-right");
        state.backward_kill_word();
        assert_eq!(state.input, "split-window-");
        state.backward_kill_word();
        assert_eq!(state.input, "split-");
        state.backward_kill_word();
        assert_eq!(state.input, "");
        state.backward_kill_word(); // empty: no panic
        assert_eq!(state.input, "");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn multibyte_input_is_edited_by_characters_not_bytes() {
        let mut state = mb("héllo");
        state.delete_backward_char();
        assert_eq!(state.input, "héll");
        state.move_beginning_of_line();
        state.forward_char();
        state.delete_backward_char();
        assert_eq!(state.input, "éll");
    }

    #[test]
    fn render_line_shows_the_prompt_and_the_input() {
        let state = mb("toggle");
        assert_eq!(state.render_line(), "M-x toggle");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::minibuffer`
Expected: compile error — module `minibuffer` not declared / `MinibufferState` not found

- [ ] **Step 3: Implement the minibuffer.** Add `pub mod minibuffer;` to `src/emacs/mod.rs`'s module list (alphabetically, after `pub mod keymap;`), then fill `src/emacs/minibuffer.rs` above the tests:

```rust
//! The minibuffer: a one-line editable prompt in the echo area.
//!
//! Pure state + pure editing functions. The minibuffer is a keymap on the
//! stack (`MapContext::Minibuffer`), NOT a special case in the dispatcher —
//! spec §3.5.

/// A live minibuffer prompt. `cursor` is a CHARACTER index into `input`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinibufferState {
    pub prompt: String,
    pub input: String,
    pub cursor: usize,
    /// Completion candidates for `input` (filled by Task 9).
    pub candidates: Vec<&'static str>,
    /// Index into `candidates`.
    pub selected: usize,
}

impl MinibufferState {
    pub fn new(prompt: &str) -> Self {
        Self {
            prompt: prompt.to_string(),
            input: String::new(),
            cursor: 0,
            candidates: Vec::new(),
            selected: 0,
        }
    }

    fn char_len(&self) -> usize {
        self.input.chars().count()
    }

    /// Byte offset of character index `idx`.
    fn byte_at(&self, idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(idx)
            .map(|(byte, _)| byte)
            .unwrap_or(self.input.len())
    }

    pub fn insert_char(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.input.insert(at, c);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert_char(c);
        }
    }

    pub fn delete_backward_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let from = self.byte_at(self.cursor - 1);
        let to = self.byte_at(self.cursor);
        self.input.replace_range(from..to, "");
        self.cursor -= 1;
    }

    pub fn kill_line(&mut self) {
        let at = self.byte_at(self.cursor);
        self.input.truncate(at);
    }

    /// `M-DEL` / `C-w`: delete back over any separators, then back over one
    /// run of word characters. `-` is a separator, so `split-window-right`
    /// peels one segment at a time — exactly what M-x needs.
    pub fn backward_kill_word(&mut self) {
        let is_word = |c: char| c.is_alphanumeric();
        let chars: Vec<char> = self.input.chars().collect();
        let mut idx = self.cursor;
        while idx > 0 && !is_word(chars[idx - 1]) {
            idx -= 1;
        }
        while idx > 0 && is_word(chars[idx - 1]) {
            idx -= 1;
        }
        let from = self.byte_at(idx);
        let to = self.byte_at(self.cursor);
        self.input.replace_range(from..to, "");
        self.cursor = idx;
    }

    pub fn move_beginning_of_line(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end_of_line(&mut self) {
        self.cursor = self.char_len();
    }

    pub fn forward_char(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_len());
    }

    pub fn backward_char(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// The prompt line as drawn in the echo area.
    pub fn render_line(&self) -> String {
        format!("{}{}", self.prompt, self.input)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::minibuffer`
Expected: `test result: ok. 6 passed`

- [ ] **Step 5: Write the failing `M-x` dispatcher tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    fn type_ascii(app: &mut App, text: &str) {
        app.route_client_input(text.as_bytes().to_vec());
    }

    /// Spec §7.3: M-x runs a herdr action that has no binding at all.
    #[tokio::test]
    async fn m_x_runs_a_herdr_action_by_name() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        let before = app.state.sidebar_collapsed;
        app.route_client_input(vec![0x1b, b'x']); // M-x
        assert!(app.state.emacs.minibuffer.is_some(), "prompt open");
        type_ascii(&mut app, "toggle-sidebar");
        assert_eq!(
            app.state.emacs.minibuffer.as_ref().unwrap().input,
            "toggle-sidebar"
        );
        app.route_client_input(vec![0x0d]); // RET
        assert!(app.state.emacs.minibuffer.is_none(), "prompt closed");
        assert_ne!(app.state.sidebar_collapsed, before, "the action ran");
        assert!(sent_bytes(&mut rx).is_empty(), "M-x never reaches the pane");
    }

    /// M-x works from TEXT mode too (spec §3.5).
    #[tokio::test]
    async fn m_x_works_from_text_mode() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'x']); // M-x
        assert!(app.state.emacs.minibuffer.is_some());
        type_ascii(&mut app, "beginning-of-buffer");
        app.route_client_input(vec![0x0d]); // RET
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!((point.row, point.col), (0, 0), "the builtin ran in TEXT mode");
        assert!(app.state.emacs.minibuffer.is_none());
    }

    /// C-g aborts.
    #[tokio::test]
    async fn c_g_aborts_the_minibuffer() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x1b, b'x']); // M-x
        type_ascii(&mut app, "detach");
        app.route_client_input(vec![0x07]); // C-g
        assert!(app.state.emacs.minibuffer.is_none());
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Quit"));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    /// The minibuffer keymap is on the stack: the editing chords are real
    /// commands, and they act on the minibuffer, not on the buffer.
    #[tokio::test]
    async fn minibuffer_editing_keys_work() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x1b, b'x']); // M-x
        type_ascii(&mut app, "split-window-right");
        app.route_client_input(vec![0x1b, 0x7f]); // M-DEL -> "split-window-"
        assert_eq!(
            app.state.emacs.minibuffer.as_ref().unwrap().input,
            "split-window-"
        );
        app.route_client_input(vec![0x17]); // C-w  -> "split-"
        assert_eq!(
            app.state.emacs.minibuffer.as_ref().unwrap().input,
            "split-"
        );
        app.route_client_input(vec![0x01]); // C-a
        assert_eq!(app.state.emacs.minibuffer.as_ref().unwrap().cursor, 0);
        app.route_client_input(vec![0x0b]); // C-k -> ""
        assert_eq!(app.state.emacs.minibuffer.as_ref().unwrap().input, "");
        type_ascii(&mut app, "detachX");
        app.route_client_input(vec![0x7f]); // DEL -> "detach"
        assert_eq!(
            app.state.emacs.minibuffer.as_ref().unwrap().input,
            "detach"
        );
    }

    /// An unknown name says so and leaves the prompt closed.
    #[tokio::test]
    async fn m_x_with_an_unknown_name_complains() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x1b, b'x']); // M-x
        type_ascii(&mut app, "no-such-command");
        app.route_client_input(vec![0x0d]); // RET
        assert!(app.state.emacs.minibuffer.is_none());
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("No match: no-such-command")
        );
    }

    /// A prefix argument survives the minibuffer round trip:
    /// `C-u 2 M-x switch-tab` targets tab index 1.
    #[tokio::test]
    async fn a_prefix_argument_reaches_the_m_x_command() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x15]); // C-u
        app.route_client_input(b"2".to_vec()); // 2
        app.route_client_input(vec![0x1b, b'x']); // M-x
        assert_eq!(
            app.state.emacs.minibuffer.as_ref().unwrap().prompt,
            "2 M-x ",
            "the pending argument is shown in the prompt, like Emacs"
        );
        type_ascii(&mut app, "switch-tab");
        app.route_client_input(vec![0x0d]); // RET
        assert!(app.state.emacs.minibuffer.is_none());
        assert_eq!(app.state.emacs.prefix_arg, None, "argument consumed");
    }
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: compile error — `no field minibuffer on type EmacsState`

- [ ] **Step 7: Add the state.** In `src/emacs/mod.rs`, add the field to `EmacsState` (after `pub prefix_arg: Option<i64>,`):

```rust
    /// Open minibuffer prompt (`M-x`), if any.
    pub minibuffer: Option<minibuffer::MinibufferState>,
```

Initialize `minibuffer: None,` in `from_config`, clear it in `apply_config`'s disabled block, and extend `map_context`:

```rust
    /// Which keymap stack is active right now (spec §3.1).
    pub fn map_context(&self) -> MapContext {
        if self.minibuffer.is_some() {
            MapContext::Minibuffer
        } else if self.text_mode.is_some() {
            MapContext::Text
        } else {
            MapContext::Live
        }
    }
```

- [ ] **Step 8: Bind the minibuffer keymap.** In `src/emacs/commands.rs`, add `M-x` to `DEFAULT_GLOBAL_BINDINGS` (after the `C-u` line):

```rust
    (
        "M-x",
        EmacsCommand::Builtin(EmacsBuiltin::ExecuteExtendedCommand),
    ),
```

and fill `DEFAULT_MINIBUFFER_BINDINGS`:

```rust
/// The minibuffer's local map. `C-g` (keyboard-quit) is NOT here: it lives
/// in the global map and reaches the minibuffer by fallthrough (spec §3.1).
const DEFAULT_MINIBUFFER_BINDINGS: &[(&str, EmacsCommand)] = &[
    ("RET", EmacsCommand::Builtin(EmacsBuiltin::ExitMinibuffer)),
    ("TAB", EmacsCommand::Builtin(EmacsBuiltin::MinibufferComplete)),
    (
        "DEL",
        EmacsCommand::Builtin(EmacsBuiltin::DeleteBackwardChar),
    ),
    ("C-k", EmacsCommand::Builtin(EmacsBuiltin::KillLine)),
    // Spec §3.5 lists C-w among the editing keys; `kill-region` in a
    // minibuffer with no mark is inert, so both C-w and M-DEL are
    // `backward-kill-word` (the readline habit). Documented in
    // docs/emacs-layer.md.
    (
        "C-w",
        EmacsCommand::Builtin(EmacsBuiltin::BackwardKillWord),
    ),
    (
        "M-DEL",
        EmacsCommand::Builtin(EmacsBuiltin::BackwardKillWord),
    ),
    (
        "C-a",
        EmacsCommand::Builtin(EmacsBuiltin::MoveBeginningOfLine),
    ),
    ("C-e", EmacsCommand::Builtin(EmacsBuiltin::MoveEndOfLine)),
    ("C-f", EmacsCommand::Builtin(EmacsBuiltin::ForwardChar)),
    ("C-b", EmacsCommand::Builtin(EmacsBuiltin::BackwardChar)),
    ("C-y", EmacsCommand::Builtin(EmacsBuiltin::Yank)),
    ("C-n", EmacsCommand::Builtin(EmacsBuiltin::NextLine)),
    ("C-p", EmacsCommand::Builtin(EmacsBuiltin::PreviousLine)),
];
```

- [ ] **Step 9: Route the commands.** In `src/app/input/emacs.rs`:

(a) Self-insert into the minibuffer. In `emacs_intercept_key`'s `Lookup::Unbound` arm, insert this BEFORE the `if text_active && single && chord.is_self_insert()` check:

```rust
                if self.state.emacs.minibuffer.is_some() {
                    if let Some(c) = chord.self_insert_char().filter(|_| single) {
                        if let Some(mb) = self.state.emacs.minibuffer.as_mut() {
                            mb.insert_char(c);
                        }
                        self.emacs_refresh_candidates();
                        return true;
                    }
                    self.state.emacs.echo = Some(format!("{} is undefined", format_seq(&seq)));
                    return true;
                }
```

(b) `emacs_refresh_candidates` is a no-op until Task 9. Add it inside `impl App`, next to `emacs_focused_pane`:

```rust
    /// Recompute the minibuffer's completion candidates. Task 9 fills this in.
    fn emacs_refresh_candidates(&mut self) {}
```

(c) Route the editing/builtin commands. In `execute_emacs_builtin`, replace the "not implemented yet" arm and add minibuffer routing. The full set of new/changed arms:

```rust
            EmacsBuiltin::ExecuteExtendedCommand => {
                let prompt = match prefix {
                    Some(n) => format!("{n} M-x "),
                    None => "M-x ".to_string(),
                };
                // The argument is held for the command the user is about to
                // name (Emacs shows it in the prompt).
                self.state.emacs.prefix_arg = prefix;
                self.state.emacs.minibuffer =
                    Some(crate::emacs::minibuffer::MinibufferState::new(&prompt));
                self.emacs_refresh_candidates();
            }
            EmacsBuiltin::ExitMinibuffer => self.emacs_minibuffer_exit(),
            EmacsBuiltin::DeleteBackwardChar => {
                if let Some(mb) = self.state.emacs.minibuffer.as_mut() {
                    mb.delete_backward_char();
                    self.emacs_refresh_candidates();
                }
            }
            EmacsBuiltin::KillLine => {
                if let Some(mb) = self.state.emacs.minibuffer.as_mut() {
                    mb.kill_line();
                    self.emacs_refresh_candidates();
                }
            }
            EmacsBuiltin::BackwardKillWord => {
                if let Some(mb) = self.state.emacs.minibuffer.as_mut() {
                    mb.backward_kill_word();
                    self.emacs_refresh_candidates();
                }
            }
            // Task 9.
            EmacsBuiltin::MinibufferComplete => {}
            // Task 10.
            EmacsBuiltin::DescribeKey | EmacsBuiltin::DescribeBindings => {}
            EmacsBuiltin::UniversalArgument => {
                // Handled by the reader stage in `emacs_intercept_key`;
                // reachable here only via M-x, where it is a no-op.
            }
```

The four commands shared with TEXT mode must act on the minibuffer when it is open. Replace their arms:

```rust
            EmacsBuiltin::MoveBeginningOfLine
            | EmacsBuiltin::MoveEndOfLine
            | EmacsBuiltin::ForwardChar
            | EmacsBuiltin::BackwardChar
            | EmacsBuiltin::NextLine
            | EmacsBuiltin::PreviousLine
            | EmacsBuiltin::ForwardWord
            | EmacsBuiltin::BackwardWord
            | EmacsBuiltin::ScrollUp
            | EmacsBuiltin::ScrollDown
            | EmacsBuiltin::BeginningOfBuffer
            | EmacsBuiltin::EndOfBuffer => {
                if self.state.emacs.minibuffer.is_some() {
                    self.emacs_minibuffer_motion(builtin);
                } else {
                    self.emacs_text_motion(builtin, prefix);
                }
            }
```

(Remove those variants from the old motion arm so each variant appears once.)

Add the two new methods inside `impl App`:

```rust
    /// Motions inside the minibuffer. `next-line`/`previous-line` move
    /// through the candidate list (vertico-style) — Task 9 makes that
    /// visible; here they simply move `selected`.
    fn emacs_minibuffer_motion(&mut self, builtin: EmacsBuiltin) {
        let Some(mb) = self.state.emacs.minibuffer.as_mut() else {
            return;
        };
        match builtin {
            EmacsBuiltin::MoveBeginningOfLine => mb.move_beginning_of_line(),
            EmacsBuiltin::MoveEndOfLine => mb.move_end_of_line(),
            EmacsBuiltin::ForwardChar => mb.forward_char(),
            EmacsBuiltin::BackwardChar => mb.backward_char(),
            EmacsBuiltin::NextLine => {
                if !mb.candidates.is_empty() {
                    mb.selected = (mb.selected + 1) % mb.candidates.len();
                }
            }
            EmacsBuiltin::PreviousLine => {
                if !mb.candidates.is_empty() {
                    mb.selected = (mb.selected + mb.candidates.len() - 1) % mb.candidates.len();
                }
            }
            _ => {}
        }
    }

    /// `RET` — run the named command, then close the prompt. An exact name
    /// wins; otherwise the selected candidate (Task 9) is used.
    fn emacs_minibuffer_exit(&mut self) {
        let Some(mb) = self.state.emacs.minibuffer.take() else {
            return;
        };
        let name = if crate::emacs::commands::EmacsCommand::from_name(&mb.input).is_some() {
            mb.input.clone()
        } else {
            mb.candidates
                .get(mb.selected)
                .map(|candidate| candidate.to_string())
                .unwrap_or_else(|| mb.input.clone())
        };
        let arg = self.state.emacs.prefix_arg.take();
        match crate::emacs::commands::EmacsCommand::from_name(&name) {
            Some(cmd) => self.execute_emacs_command(cmd, arg),
            None => self.state.emacs.echo = Some(format!("No match: {name}")),
        }
    }
```

(d) `C-g` must close the minibuffer. In the `EmacsBuiltin::KeyboardQuit` arm, add before the echo:

```rust
                self.state.emacs.minibuffer = None;
```

(e) The minibuffer must own every key. In `emacs_intercept_key`, the early `if let Some(text_pane_id) = ... { auto-exit TEXT mode }` block must not fire while the minibuffer is open (a `M-x` prompt should not tear down TEXT mode). Wrap that whole block:

```rust
        if self.state.emacs.minibuffer.is_none() {
            if let Some(text_pane_id) = self.state.emacs.text_mode.as_ref().map(|text| text.pane_id)
            {
                // ...existing body, unchanged...
            }
        }
```

Also make `emacs_would_consume` return true whenever a minibuffer is open — change its first lines to:

```rust
        if self.state.emacs.text_mode.is_some() || self.state.emacs.minibuffer.is_some() {
            return true;
        }
```

- [ ] **Step 10: Render the prompt.** In `src/emacs/render.rs`, `render_echo_area`, make the minibuffer the highest-priority content — insert as the FIRST arm of the `let content = ...` chain:

```rust
    let content = if let Some(mb) = app.emacs.minibuffer.as_ref() {
        mb.render_line()
    } else if let Some(prompt) = app
```

- [ ] **Step 11: Run the tests**

Run: `cargo test --locked --bin herdr emacs:: app::input::emacs`
Expected: `test result: ok.`

- [ ] **Step 12: Commit**

```bash
cargo fmt
git add src/emacs src/app/input/emacs.rs
git commit -m "feat: minibuffer with M-x, its own keymap on the stack, and C-g abort"
```

---

### Task 9: `M-x` fuzzy completion

Vertical candidate list above the minibuffer (vertico-style), `C-n`/`C-p` to select, `TAB` to complete, `RET` to run (spec §3.5).

**Files:**
- Modify: `src/emacs/minibuffer.rs` (`fuzzy_score`, `filter_candidates`)
- Modify: `src/emacs/render.rs` (candidate list)
- Modify: `src/app/input/emacs.rs` (`emacs_refresh_candidates`, `MinibufferComplete`)
- Test: inline `#[cfg(test)]` in `src/emacs/minibuffer.rs`, `src/emacs/render.rs`, `src/app/input/emacs.rs`

**Interfaces:**
- Consumes: `commands::all_commands()` (Task 4) — the full M-x namespace, builtins plus all 45 herdr actions.
- Produces:
  - `minibuffer::fuzzy_score(query: &str, name: &str) -> Option<i64>` (lower is better; `None` = no match)
  - `minibuffer::filter_candidates(query: &str, names: &[&'static str]) -> Vec<&'static str>`
  - `minibuffer::MAX_VISIBLE_CANDIDATES: usize = 10`
  - `MinibufferState::common_prefix(&self) -> Option<String>`

- [ ] **Step 1: Write the failing completion tests** — append to `mod tests` in `src/emacs/minibuffer.rs`:

```rust
    const NAMES: &[&str] = &[
        "backward-char",
        "detach",
        "forward-char",
        "other-window",
        "split-window-below",
        "split-window-right",
        "switch-tab",
        "toggle-sidebar",
    ];

    #[test]
    fn an_empty_query_matches_everything_in_order() {
        assert_eq!(filter_candidates("", NAMES), NAMES.to_vec());
    }

    #[test]
    fn a_prefix_query_ranks_the_prefix_matches_first() {
        let hits = filter_candidates("split", NAMES);
        assert_eq!(hits[0], "split-window-below");
        assert_eq!(hits[1], "split-window-right");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn a_subsequence_query_matches_out_of_order_characters() {
        // t-s-b: Toggle-Sidebar
        let hits = filter_candidates("tsb", NAMES);
        assert!(hits.contains(&"toggle-sidebar"), "{hits:?}");
        // An exact name is always its own best match.
        assert_eq!(filter_candidates("detach", NAMES)[0], "detach");
    }

    #[test]
    fn a_contiguous_match_beats_a_scattered_one() {
        // "swt" is contiguous nowhere, but "switch-tab" has it early;
        // "split-window-below" needs a wide scatter.
        assert_eq!(filter_candidates("swt", NAMES)[0], "switch-tab");
    }

    #[test]
    fn no_match_yields_no_candidates() {
        assert!(filter_candidates("zzzz", NAMES).is_empty());
        assert_eq!(fuzzy_score("zzzz", "detach"), None);
    }

    #[test]
    fn tab_completes_to_the_longest_common_prefix() {
        let mut state = MinibufferState::new("M-x ");
        state.insert_str("split");
        state.candidates = filter_candidates("split", NAMES);
        assert_eq!(state.common_prefix().as_deref(), Some("split-window-"));

        let mut state = MinibufferState::new("M-x ");
        state.insert_str("det");
        state.candidates = filter_candidates("det", NAMES);
        assert_eq!(
            state.common_prefix().as_deref(),
            Some("detach"),
            "a unique match completes fully"
        );

        let mut state = MinibufferState::new("M-x ");
        state.candidates = Vec::new();
        assert_eq!(state.common_prefix(), None);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::minibuffer`
Expected: compile error — `cannot find function filter_candidates` / `fuzzy_score` / no method `common_prefix`

- [ ] **Step 3: Implement completion.** Append to `src/emacs/minibuffer.rs`, above the tests:

```rust
/// How many candidates the vertical list shows at once.
pub const MAX_VISIBLE_CANDIDATES: usize = 10;

/// Fuzzy subsequence score. **Lower is better.** `None` when `query` is not
/// a subsequence of `name` (case-insensitive on ASCII).
///
/// The score rewards, in order: matching early, matching contiguously, and
/// a shorter name. That is enough to make `split` rank the two
/// `split-window-*` commands first and `tsb` find `toggle-sidebar`.
pub fn fuzzy_score(query: &str, name: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(name.len() as i64);
    }
    let name_chars: Vec<char> = name.chars().collect();
    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;
    let mut gaps: i64 = 0;
    let mut at = 0usize;

    for q in query.chars() {
        let q = q.to_ascii_lowercase();
        let found = name_chars[at..]
            .iter()
            .position(|c| c.to_ascii_lowercase() == q)?
            + at;
        if first.is_none() {
            first = Some(found);
        }
        if let Some(prev) = last {
            if found > prev + 1 {
                gaps += 1;
            }
        }
        last = Some(found);
        at = found + 1;
    }

    let first = first.unwrap_or(0) as i64;
    Some(first * 10 + gaps * 5 + name_chars.len() as i64)
}

/// All names matching `query`, best first. Ties break alphabetically, so
/// the list is deterministic.
pub fn filter_candidates(query: &str, names: &[&'static str]) -> Vec<&'static str> {
    let mut scored: Vec<(i64, &'static str)> = names
        .iter()
        .filter_map(|name| fuzzy_score(query, name).map(|score| (score, *name)))
        .collect();
    scored.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().map(|(_, name)| name).collect()
}

impl MinibufferState {
    /// The longest prefix shared by every candidate — what `TAB` completes
    /// to. `None` when there is nothing to complete to.
    pub fn common_prefix(&self) -> Option<String> {
        let first = self.candidates.first()?;
        let mut len = first.len();
        for candidate in &self.candidates[1..] {
            len = len.min(
                first
                    .chars()
                    .zip(candidate.chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a.len_utf8())
                    .sum(),
            );
        }
        if len == 0 {
            return None;
        }
        Some(first[..len].to_string())
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::minibuffer`
Expected: `test result: ok. 12 passed`

- [ ] **Step 5: Write the failing dispatcher + render tests.** Append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    #[tokio::test]
    async fn m_x_lists_candidates_and_c_n_selects() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x1b, b'x']); // M-x
        // Empty query: the whole command table (builtins + all herdr actions).
        let count = app.state.emacs.minibuffer.as_ref().unwrap().candidates.len();
        assert_eq!(count, crate::emacs::commands::all_commands().len());

        type_ascii(&mut app, "split");
        let mb = app.state.emacs.minibuffer.as_ref().unwrap();
        assert_eq!(mb.candidates, vec!["split-window-below", "split-window-right"]);
        assert_eq!(mb.selected, 0);

        app.route_client_input(vec![0x0e]); // C-n
        assert_eq!(app.state.emacs.minibuffer.as_ref().unwrap().selected, 1);
        app.route_client_input(vec![0x10]); // C-p
        assert_eq!(app.state.emacs.minibuffer.as_ref().unwrap().selected, 0);
    }

    #[tokio::test]
    async fn tab_completes_to_the_common_prefix() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x1b, b'x']); // M-x
        type_ascii(&mut app, "split");
        app.route_client_input(vec![0x09]); // TAB
        let mb = app.state.emacs.minibuffer.as_ref().unwrap();
        assert_eq!(mb.input, "split-window-");
        assert_eq!(mb.cursor, "split-window-".chars().count());
    }

    /// The selected candidate is what RET runs, even if the typed text is
    /// only a fuzzy fragment.
    #[tokio::test]
    async fn ret_runs_the_selected_candidate() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        let before = app.state.sidebar_collapsed;
        app.route_client_input(vec![0x1b, b'x']); // M-x
        type_ascii(&mut app, "tsb"); // fuzzy: toggle-sidebar
        assert_eq!(
            app.state.emacs.minibuffer.as_ref().unwrap().candidates[0],
            "toggle-sidebar"
        );
        app.route_client_input(vec![0x0d]); // RET
        assert_ne!(app.state.sidebar_collapsed, before);
    }
```

Append to `mod tests` in `src/emacs/render.rs`:

```rust
    #[test]
    fn the_candidate_list_renders_above_the_minibuffer() {
        let mut state = AppState::test_new();
        state.emacs.enabled = true;
        let mut mb = crate::emacs::minibuffer::MinibufferState::new("M-x ");
        mb.insert_str("split");
        mb.candidates = vec!["split-window-below", "split-window-right"];
        mb.selected = 1;
        state.emacs.minibuffer = Some(mb);

        let area = Rect::new(0, 0, 40, 10);
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_echo_area(&state, frame, area))
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();

        let row = |y: u16| -> String {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect::<String>()
                .trim_end()
                .to_string()
        };
        // Bottom row: the prompt. Above it, newest-last: the candidates.
        assert_eq!(row(9), "M-x split");
        assert_eq!(row(8), "split-window-right");
        assert_eq!(row(7), "split-window-below");
        // The selected candidate is highlighted.
        assert!(
            buffer[(0, 8)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED),
            "the selected candidate is reversed"
        );
    }
```

If `AppState::test_new()` is not what the other `render.rs` tests use, copy the fixture line from the existing `mod tests` in that file verbatim.

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::render app::input::emacs`
Expected: FAIL — candidates are always empty; TAB does nothing; the candidate rows are blank.

- [ ] **Step 7: Wire the candidates.** In `src/app/input/emacs.rs`, replace the stub `emacs_refresh_candidates`:

```rust
    /// Recompute the minibuffer's completion candidates over the FULL
    /// command table — builtins plus every one of herdr's actions (spec
    /// §3.5). Selection resets to the best match.
    fn emacs_refresh_candidates(&mut self) {
        let Some(query) = self
            .state
            .emacs
            .minibuffer
            .as_ref()
            .map(|mb| mb.input.clone())
        else {
            return;
        };
        let names: Vec<&'static str> = crate::emacs::commands::all_commands()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let candidates = crate::emacs::minibuffer::filter_candidates(&query, &names);
        if let Some(mb) = self.state.emacs.minibuffer.as_mut() {
            mb.candidates = candidates;
            mb.selected = 0;
        }
    }
```

and replace the `EmacsBuiltin::MinibufferComplete => {}` arm:

```rust
            EmacsBuiltin::MinibufferComplete => {
                let completion = self
                    .state
                    .emacs
                    .minibuffer
                    .as_ref()
                    .and_then(|mb| mb.common_prefix());
                if let Some(completion) = completion {
                    if let Some(mb) = self.state.emacs.minibuffer.as_mut() {
                        if completion != mb.input {
                            mb.input = completion;
                            mb.move_end_of_line();
                        }
                    }
                    self.emacs_refresh_candidates();
                } else {
                    self.state.emacs.echo = Some("No match".to_string());
                }
            }
```

- [ ] **Step 8: Render the candidate list.** In `src/emacs/render.rs`, replace `render_echo_area` with:

```rust
/// One-line echo area drawn over the bottom row of the terminal area, plus
/// the minibuffer's vertical candidate list (vertico-style) above it.
/// herdr has no persistent status line, so this is an overlay that only
/// appears when the layer has something to say.
pub fn render_echo_area(app: &AppState, frame: &mut Frame, terminal_area: Rect) {
    if terminal_area.height == 0 || terminal_area.width == 0 {
        return;
    }
    let content = if let Some(mb) = app.emacs.minibuffer.as_ref() {
        mb.render_line()
    } else if let Some(prompt) = app
        .emacs
        .text_mode
        .as_ref()
        .and_then(|text| text.goto_line.as_deref())
    {
        format!("Goto line: {prompt}")
    } else if let Some(echo) = app.emacs.echo.as_deref() {
        echo.to_string()
    } else if let Some(reader) = app.emacs.prefix.as_ref() {
        reader.echo()
    } else if !app.emacs.pending.is_empty() {
        format!("{}-", crate::emacs::keymap::format_seq(&app.emacs.pending))
    } else {
        return;
    };

    let bottom = terminal_area.y + terminal_area.height - 1;

    // Candidate list, growing upward from just above the prompt: the best
    // match sits closest to the prompt line.
    if let Some(mb) = app.emacs.minibuffer.as_ref() {
        let rows = mb
            .candidates
            .len()
            .min(crate::emacs::minibuffer::MAX_VISIBLE_CANDIDATES)
            .min(usize::from(terminal_area.height.saturating_sub(1)));
        for (idx, candidate) in mb.candidates.iter().take(rows).enumerate() {
            let y = bottom - 1 - idx as u16;
            let style = if idx == mb.selected {
                Style::default()
                    .bg(app.palette.surface1)
                    .fg(app.palette.text)
                    .add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
                    .bg(app.palette.surface0)
                    .fg(app.palette.subtext0)
            };
            frame.render_widget(
                Paragraph::new(Line::from(candidate.to_string())).style(style),
                Rect {
                    x: terminal_area.x,
                    y,
                    width: terminal_area.width,
                    height: 1,
                },
            );
        }
    }

    let area = Rect {
        x: terminal_area.x,
        y: bottom,
        width: terminal_area.width,
        height: 1,
    };
    let paragraph = Paragraph::new(Line::from(content)).style(
        Style::default()
            .bg(app.palette.surface0)
            .fg(app.palette.text),
    );
    frame.render_widget(paragraph, area);
}
```

If `app.palette.subtext0` does not exist, use the palette field the rest of `src/ui.rs` uses for dimmed text (grep `palette.` in `src/ui.rs` and pick the dim/secondary one).

- [ ] **Step 9: Run the tests**

Run: `cargo test --locked --bin herdr emacs:: app::input::emacs`
Expected: `test result: ok.`

- [ ] **Step 10: Commit**

```bash
cargo fmt
git add src/emacs src/app/input/emacs.rs
git commit -m "feat: fuzzy M-x completion with a vertical candidate list"
```

---

### Task 10: The `C-h` help prefix

`C-h k` (describe-key) and `C-h b` (describe-bindings, the whole active stack in a scrollable overlay), plus `F1` as the live-mode entry point — the pane owns `C-h` there, and `C-h` is byte 8 on legacy terminals anyway (spec §3.7).

**Files:**
- Create: `src/emacs/help.rs`
- Modify: `src/emacs/mod.rs` (`pub mod help;`, two `EmacsState` fields)
- Modify: `src/emacs/commands.rs` (help bindings in global + text)
- Modify: `src/emacs/render.rs` (`render_help_overlay`)
- Modify: `src/ui.rs` (1 call)
- Modify: `src/app/input/emacs.rs` (describe-key reader, overlay keys, executor arms)
- Test: inline `#[cfg(test)]` in `src/emacs/help.rs` and `src/app/input/emacs.rs`

**Interfaces:**
- Consumes: `KeymapSet::active_maps(ctx)` and `ActiveMap { name, map }` (Task 1), `Keymap::bindings()` (Task 1), `format_seq` (Task 2), `EmacsCommand::name` (Task 4).
- Produces:
  - `help::HelpOverlay { pub title: String, pub lines: Vec<String>, pub scroll: usize }`
  - `help::HelpOverlay::scroll_by(&mut self, delta: i64, page: usize)`
  - `help::describe_bindings_lines(keymaps: &KeymapSet, ctx: MapContext) -> Vec<String>`
  - `EmacsState::help: Option<HelpOverlay>`, `EmacsState::describe_key: bool`

- [ ] **Step 1: Write the failing help tests.** Create `src/emacs/help.rs` with ONLY the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs::commands::{build_keymaps, MapContext};

    #[test]
    fn describe_bindings_lists_every_active_map() {
        let (keymaps, _) = build_keymaps(&Default::default());
        let lines = describe_bindings_lines(&keymaps, MapContext::Text);
        let joined = lines.join("\n");

        // Grouped by map, highest priority first.
        assert_eq!(lines[0], "text");
        assert!(lines.contains(&"global".to_string()), "{joined}");
        assert!(
            lines.iter().position(|l| l == "text")
                < lines.iter().position(|l| l == "global"),
            "text is listed before global"
        );

        // A binding from EACH active map (spec §6).
        assert!(
            lines.iter().any(|l| l.contains("C-f") && l.contains("forward-char")),
            "text binding missing: {joined}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("C-x 3") && l.contains("split-window-right")),
            "global binding missing: {joined}"
        );
    }

    #[test]
    fn describe_bindings_in_live_mode_lists_only_global() {
        let (keymaps, _) = build_keymaps(&Default::default());
        let lines = describe_bindings_lines(&keymaps, MapContext::Live);
        assert_eq!(lines[0], "global");
        assert!(!lines.iter().any(|l| l == "text"));
        assert!(!lines.iter().any(|l| l.contains("forward-char")));
    }

    #[test]
    fn bindings_are_sorted_within_a_map() {
        let (keymaps, _) = build_keymaps(&Default::default());
        let lines = describe_bindings_lines(&keymaps, MapContext::Live);
        let entries: Vec<&String> = lines[1..].iter().collect();
        let mut sorted = entries.clone();
        sorted.sort();
        assert_eq!(entries, sorted, "deterministic order");
    }

    #[test]
    fn the_overlay_scrolls_and_clamps() {
        let mut overlay = HelpOverlay {
            title: "Bindings".to_string(),
            lines: (0..30).map(|i| format!("line {i}")).collect(),
            scroll: 0,
        };
        overlay.scroll_by(1, 10);
        assert_eq!(overlay.scroll, 1);
        overlay.scroll_by(-5, 10);
        assert_eq!(overlay.scroll, 0, "clamped at the top");
        overlay.scroll_by(1000, 10);
        assert_eq!(overlay.scroll, 20, "clamped so the last page stays full");
        let mut short = HelpOverlay {
            title: "Bindings".to_string(),
            lines: vec!["only".to_string()],
            scroll: 0,
        };
        short.scroll_by(9, 10);
        assert_eq!(short.scroll, 0, "nothing to scroll");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::help`
Expected: compile error — module `help` not declared

- [ ] **Step 3: Implement help.** Add `pub mod help;` to `src/emacs/mod.rs`'s module list (after `pub mod commands;`), then fill `src/emacs/help.rs` above the tests:

```rust
//! `C-h` — the discoverability half of the layer (spec §3.7).
//!
//! `describe-bindings` renders the WHOLE active keymap stack, so it is
//! correct by construction in live mode, TEXT mode and the minibuffer: it
//! just walks whatever `active_maps` returns.

use super::commands::{KeymapSet, MapContext};
use super::keymap::format_seq;

/// A scrollable read-only overlay (`C-h b`).
#[derive(Debug, Clone)]
pub struct HelpOverlay {
    pub title: String,
    pub lines: Vec<String>,
    /// First visible line.
    pub scroll: usize,
}

impl HelpOverlay {
    /// Scroll by `delta` lines, clamped so the view never runs past the end.
    pub fn scroll_by(&mut self, delta: i64, page: usize) {
        let max = self.lines.len().saturating_sub(page);
        let next = (self.scroll as i64).saturating_add(delta);
        self.scroll = next.clamp(0, max as i64) as usize;
    }
}

/// Every binding in the active keymap stack, grouped by map (highest
/// priority first), chord sequence -> command name.
pub fn describe_bindings_lines(keymaps: &KeymapSet, ctx: MapContext) -> Vec<String> {
    let mut lines = Vec::new();
    for active in keymaps.active_maps(ctx) {
        lines.push(active.name.to_string());
        let mut entries: Vec<String> = active
            .map
            .bindings()
            .iter()
            .map(|(seq, cmd)| format!("  {:<12} {}", format_seq(seq), cmd.name()))
            .collect();
        entries.sort();
        lines.extend(entries);
    }
    lines
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::help`
Expected: `test result: ok. 4 passed`

- [ ] **Step 5: Write the failing dispatcher tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    /// Spec §7.2: C-h b lists every active binding.
    #[tokio::test]
    async fn c_h_b_describes_the_whole_active_stack_in_text_mode() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x08]); // C-h (legacy byte 8)
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-h-"), "a prefix key");
        app.route_client_input(vec![b'b']);
        let help = app.state.emacs.help.as_ref().expect("overlay open");
        assert!(help.lines.iter().any(|l| l == "text"));
        assert!(help.lines.iter().any(|l| l == "global"));
        assert!(help
            .lines
            .iter()
            .any(|l| l.contains("C-x 3") && l.contains("split-window-right")));
        assert!(sent_bytes(&mut rx).is_empty());
        // q closes it.
        app.route_client_input(vec![b'q']);
        assert!(app.state.emacs.help.is_none());
        assert!(app.state.emacs.text_mode.is_some(), "TEXT mode survives");
    }

    /// Spec §7.2: C-h k C-x 3 answers `split-window-right`.
    #[tokio::test]
    async fn c_h_k_names_the_command_a_sequence_runs() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x08, b'k']); // C-h k
        assert!(app.state.emacs.describe_key, "reading a key");
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Describe key: "));
        app.route_client_input(vec![0x18]); // C-x (a prefix: keep reading)
        assert!(app.state.emacs.describe_key);
        app.route_client_input(vec![b'3']);
        assert!(!app.state.emacs.describe_key);
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("C-x 3 runs the command split-window-right")
        );
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes.len(),
            1,
            "describe-key must NOT run the command"
        );
    }

    #[tokio::test]
    async fn c_h_k_reports_an_undefined_sequence() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x08, b'k']); // C-h k
        app.route_client_input(vec![0x14]); // C-t
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-t is undefined"));
        assert!(!app.state.emacs.describe_key);
    }

    /// In LIVE mode the pane owns C-h — it must reach the agent untouched.
    /// F1 is the live-mode help entry point (spec §3.7).
    #[tokio::test]
    async fn c_h_reaches_the_pane_in_live_mode_and_f1_opens_help() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x08]); // C-h
        assert!(app.state.emacs.help.is_none());
        assert_eq!(sent_bytes(&mut rx), vec![0x08], "the agent owns C-h");

        app.route_client_input(b"\x1bOP".to_vec()); // F1
        assert_eq!(app.state.emacs.echo.as_deref(), Some("F1-"));
        app.route_client_input(vec![b'b']);
        let help = app.state.emacs.help.as_ref().expect("overlay open");
        assert_eq!(help.lines[0], "global");
        assert!(!help.lines.iter().any(|l| l == "text"), "live mode: no text map");
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn the_help_overlay_scrolls() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.route_client_input(b"\x1bOP".to_vec()); // F1
        app.route_client_input(vec![b'b']);
        assert_eq!(app.state.emacs.help.as_ref().unwrap().scroll, 0);
        app.route_client_input(vec![0x0e]); // C-n
        assert_eq!(app.state.emacs.help.as_ref().unwrap().scroll, 1);
        app.route_client_input(vec![0x10]); // C-p
        assert_eq!(app.state.emacs.help.as_ref().unwrap().scroll, 0);
        app.route_client_input(vec![0x1b]); // ESC closes
        assert!(app.state.emacs.help.is_none());
    }
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: compile error — `no field help on type EmacsState`

- [ ] **Step 7: Add the state.** In `src/emacs/mod.rs`, add to `EmacsState` (after `pub minibuffer: ...`):

```rust
    /// Open help overlay (`C-h b` / `F1 b`).
    pub help: Option<help::HelpOverlay>,
    /// True while `C-h k` is reading the sequence to describe.
    pub describe_key: bool,
```

Initialize `help: None, describe_key: false,` in `from_config` and clear both in `apply_config`'s disabled block.

- [ ] **Step 8: Bind the help prefix.** In `src/emacs/commands.rs`, add to `DEFAULT_GLOBAL_BINDINGS`:

```rust
    // F1 is the live-mode help entry point: in live mode the pane owns C-h
    // (and C-h is byte 8 on legacy terminals anyway) — spec §3.7.
    ("F1 k", EmacsCommand::Builtin(EmacsBuiltin::DescribeKey)),
    (
        "F1 b",
        EmacsCommand::Builtin(EmacsBuiltin::DescribeBindings),
    ),
```

and to `DEFAULT_TEXT_BINDINGS`:

```rust
    ("C-h k", EmacsCommand::Builtin(EmacsBuiltin::DescribeKey)),
    (
        "C-h b",
        EmacsCommand::Builtin(EmacsBuiltin::DescribeBindings),
    ),
```

- [ ] **Step 9: Wire the reader stages and the executor.** In `src/app/input/emacs.rs`:

(a) Overlay keys own the keyboard. Insert this in `emacs_intercept_key` immediately AFTER the `if self.state.emacs.quoted_insert { ... }` block and BEFORE the goto-line reader:

```rust
        // Emacs layer seam (fork): the help overlay owns every key while open.
        if self.state.emacs.help.is_some() {
            return self.emacs_help_overlay_key(key);
        }
```

and, after that, the describe-key reader — insert directly BEFORE `let ctx = self.state.emacs.map_context();`:

```rust
        if self.state.emacs.describe_key {
            return self.emacs_describe_key_read(key);
        }
```

(b) Add the three methods inside `impl App`:

```rust
    /// `C-h b` / `F1 b` overlay: scroll with C-n/C-p/C-v/M-v, close with
    /// q / ESC / C-g. Everything else is swallowed.
    fn emacs_help_overlay_key(&mut self, key: TerminalKey) -> bool {
        use crossterm::event::KeyCode;
        let Some(chord) = Chord::from_key(&key) else {
            return true;
        };
        let page = usize::from(self.state.view.terminal_area.height.saturating_sub(2).max(1));
        let close = matches!(chord.code, KeyCode::Char('q') | KeyCode::Esc)
            && !chord.ctrl
            && !chord.meta
            || chord == Chord::ctrl('g');
        if close {
            self.state.emacs.help = None;
            self.state.emacs.echo = None;
            return true;
        }
        let delta = match (chord.ctrl, chord.meta, chord.code) {
            (true, false, KeyCode::Char('n')) | (false, false, KeyCode::Down) => 1,
            (true, false, KeyCode::Char('p')) | (false, false, KeyCode::Up) => -1,
            (true, false, KeyCode::Char('v')) | (false, false, KeyCode::PageDown) => page as i64,
            (false, true, KeyCode::Char('v')) | (false, false, KeyCode::PageUp) => -(page as i64),
            _ => 0,
        };
        if delta != 0 {
            if let Some(help) = self.state.emacs.help.as_mut() {
                help.scroll_by(delta, page);
            }
        }
        true
    }

    /// `C-h k` — read a sequence and NAME the command it runs, without
    /// running it.
    fn emacs_describe_key_read(&mut self, key: TerminalKey) -> bool {
        let Some(chord) = Chord::from_key(&key) else {
            return true;
        };
        if chord == Chord::ctrl('g') {
            self.state.emacs.describe_key = false;
            self.state.emacs.pending.clear();
            self.state.emacs.echo = Some("Quit".to_string());
            return true;
        }
        let mut seq = self.state.emacs.pending.clone();
        seq.push(chord);
        // The stack the user is actually in — NOT a describe-key context.
        let ctx = if self.state.emacs.text_mode.is_some() {
            MapContext::Text
        } else {
            MapContext::Live
        };
        match self.state.emacs.keymaps.lookup(ctx, &seq) {
            Lookup::Prefix => {
                self.state.emacs.pending = seq.clone();
                self.state.emacs.echo = Some(format!("Describe key: {}-", format_seq(&seq)));
            }
            Lookup::Bound(cmd) => {
                self.state.emacs.pending.clear();
                self.state.emacs.describe_key = false;
                self.state.emacs.echo = Some(format!(
                    "{} runs the command {}",
                    format_seq(&seq),
                    cmd.name()
                ));
            }
            Lookup::Unbound => {
                self.state.emacs.pending.clear();
                self.state.emacs.describe_key = false;
                self.state.emacs.echo = Some(format!("{} is undefined", format_seq(&seq)));
            }
        }
        true
    }

    /// `C-h b` / `F1 b` — render the whole active keymap stack.
    fn emacs_describe_bindings(&mut self) {
        let ctx = self.state.emacs.map_context();
        let lines = crate::emacs::help::describe_bindings_lines(&self.state.emacs.keymaps, ctx);
        self.state.emacs.help = Some(crate::emacs::help::HelpOverlay {
            title: "Active bindings".to_string(),
            lines,
            scroll: 0,
        });
        self.state.emacs.echo = None;
    }
```

(c) Replace the `EmacsBuiltin::DescribeKey | EmacsBuiltin::DescribeBindings => {}` arm of `execute_emacs_builtin`:

```rust
            EmacsBuiltin::DescribeKey => {
                self.state.emacs.describe_key = true;
                self.state.emacs.pending.clear();
                self.state.emacs.echo = Some("Describe key: ".to_string());
            }
            EmacsBuiltin::DescribeBindings => self.emacs_describe_bindings(),
```

(d) `emacs_would_consume` must also cover the two readers — extend its first condition:

```rust
        if self.state.emacs.text_mode.is_some()
            || self.state.emacs.minibuffer.is_some()
            || self.state.emacs.help.is_some()
            || self.state.emacs.describe_key
        {
            return true;
        }
```

- [ ] **Step 10: Render the overlay.** Append to `src/emacs/render.rs`:

```rust
/// The `C-h b` overlay: a centered, scrollable, read-only pane. Drawn above
/// the echo area and below herdr's own modal overlays.
pub fn render_help_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(help) = app.emacs.help.as_ref() else {
        return;
    };
    if area.width < 20 || area.height < 5 {
        return;
    }
    let width = area.width.saturating_sub(8).min(72).max(20);
    let height = area.height.saturating_sub(4).max(5);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(ratatui::widgets::Clear, rect);
    let body: Vec<Line> = help
        .lines
        .iter()
        .skip(help.scroll)
        .take(usize::from(height.saturating_sub(2)))
        .map(|line| Line::from(line.clone()))
        .collect();
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .title(format!("{}  (C-n/C-p scroll, q to close)", help.title));
    frame.render_widget(
        Paragraph::new(body).block(block).style(
            Style::default()
                .bg(app.palette.surface0)
                .fg(app.palette.text),
        ),
        rect,
    );
}
```

In `src/ui.rs`, directly after the existing echo-area seam call (line ~429):

```rust
    // Emacs layer seam (fork).
    crate::emacs::render::render_echo_area(app, frame, terminal_area);
    crate::emacs::render::render_help_overlay(app, frame, frame.area());
```

- [ ] **Step 11: Run the tests**

Run: `cargo test --locked --bin herdr emacs:: app::input::emacs`
Expected: `test result: ok.`

- [ ] **Step 12: Commit**

```bash
cargo fmt
git add src/emacs src/app/input/emacs.rs src/ui.rs
git commit -m "feat: C-h/F1 help prefix with describe-key and a scrollable describe-bindings overlay"
```

---

### Task 11: Documentation

The config reference (both copies, kept identical by `scripts/config_reference_check.py`) and a user-facing bindings reference.

**Files:**
- Create: `docs/emacs-layer.md`
- Modify: `src/main.rs` (`DEFAULT_CONFIG` `[emacs]` block)
- Modify: `docs/next/website/src/data/config-reference.json`
- Modify: `website/src/data/config-reference.json`

**Interfaces:**
- Consumes: `commands::all_commands()` (Task 4) — the doc is generated from it once, by hand, and a test pins the count so it cannot silently rot.

- [ ] **Step 1: Write the failing doc-coverage test** — append to `mod tests` in `src/emacs/commands.rs`:

```rust
    /// The bindings reference (docs/emacs-layer.md) lists every command.
    /// If this fails, a command was added without documenting it.
    #[test]
    fn every_command_appears_in_the_bindings_reference() {
        let doc = include_str!("../../docs/emacs-layer.md");
        let missing: Vec<&str> = all_commands()
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| !doc.contains(&format!("`{name}`")))
            .collect();
        assert!(
            missing.is_empty(),
            "undocumented commands in docs/emacs-layer.md: {missing:?}"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked --bin herdr emacs::commands::tests::every_command_appears_in_the_bindings_reference`
Expected: compile error — `couldn't read ../../docs/emacs-layer.md: No such file or directory`

- [ ] **Step 3: Read the authoritative command list** (do not write the doc from memory — the tables in `src/emacs/commands.rs` are the source of truth, and the Step 1 test enforces it)

```bash
grep -oE '=> "[a-z-]+"' src/emacs/commands.rs | sort -u          # the 45 herdr command names
grep -oE '"[a-z-]+"\),$' src/emacs/commands.rs | sort -u         # the builtin names (BUILTIN_NAMES)
```

- [ ] **Step 4: Write `docs/emacs-layer.md`**

````markdown
# Emacs layer

A fork feature. Off by default — with `[emacs] enabled = false` this build
behaves exactly like stock herdr.

```toml
[emacs]
enabled = true
clipboard_sync = true

[emacs.keys]
"C-x 4" = "split-window-right"   # any command, no recompile
"C-x t" = "toggle-sidebar"       # a herdr action, exposed by name
```

## The contract

herdr's Emacs layer copies Emacs's own answer to "a terminal inside an
editor" (`term-mode`):

| Emacs | herdr | Who owns the keyboard |
|---|---|---|
| `term-char-mode` | **live mode** (pane focused) | The pane. The layer steals only its own chords; everything else reaches the agent. |
| `term-line-mode` | **TEXT mode** (`C-x [`) | Emacs. The buffer is an ordinary read-only Emacs buffer; the **full keymap stack** applies. |

Consequences worth internalizing:

- **In TEXT mode every global command works.** `C-x 3` splits the window
  from a read-only buffer — that is the point of a global map.
- **In live mode the pane wins by default.** The layer owns `C-x`, `C-q`,
  `C-y`, `M-y`, `M-x`, `C-u`, `C-g` and `F1`; every other key belongs to the
  agent. `C-h` reaching your agent in live mode is correct behavior — use
  `F1` for help there.
- Ghostty (kitty keyboard protocol) is the supported terminal. Without it,
  `C-i` / `C-m` / `C-[` are physically indistinguishable from `TAB` / `RET`
  / `ESC`; the layer canonicalizes them to the latter.

## The keymap stack

Maps are consulted highest-priority first; the first exact match wins, and a
sequence unbound in a local map falls through to `global`.

| Context | Stack |
|---|---|
| live mode | `global` |
| TEXT mode (`C-x [`) | `text`, `global` |
| minibuffer (`M-x`) | `minibuffer`, `global` |

Press `C-h b` (TEXT mode) or `F1 b` (anywhere) to see the live stack.

## Global map (live mode, and fallthrough everywhere)

| Keys | Command |
|---|---|
| `C-x 2` | `split-window-below` |
| `C-x 3` | `split-window-right` |
| `C-x o` | `other-window` |
| `C-x 0` | `delete-window` |
| `C-x 1` | `delete-other-windows` |
| `C-x b` | `switch-to-buffer` (herdr's navigator) |
| `C-x c` | `new-tab` |
| `C-x n` | `next-tab` |
| `C-x p` | `previous-tab` |
| `C-x k` | `kill-tab` |
| `C-x w` | `workspace-picker` |
| `C-x [` | `text-mode` |
| `C-[` | `previous-tab` (kitty only — see below) |
| `C-]` | `next-tab` |
| `M-[` | `move-tab-left` (kitty only) |
| `M-]` | `move-tab-right` (kitty only) |
| `C-q` | `quoted-insert` |
| `C-g` | `keyboard-quit` |
| `C-u` | `universal-argument` |
| `M-x` | `execute-extended-command` |
| `C-y` | `yank` |
| `M-y` | `yank-pop` |
| `F1 k` | `describe-key` |
| `F1 b` | `describe-bindings` |

`move-tab-left` / `move-tab-right` clamp at the ends — a tab never wraps
from last to first.

> **`C-[` / `C-]` / `M-[` / `M-]` need the kitty keyboard protocol.** On a
> legacy terminal `C-[` *is* byte 27 — the ESC key — and `M-[` *is* the CSI
> introducer, so the terminal cannot deliver them and the bindings simply
> never fire. They still show up in `C-h b`, because they *are* bound. `ESC`
> continues to mean `exit-text-mode`: the layer never folds a kitty `C-[`
> into `ESC`, which is exactly why binding `C-[` is safe.

## Text map (TEXT mode)

| Keys | Command |
|---|---|
| `C-f` / `C-b` | `forward-char` / `backward-char` |
| `C-n` / `C-p` | `next-line` / `previous-line` |
| `M-f` / `M-b` | `forward-word` / `backward-word` |
| `C-a` / `C-e` | `move-beginning-of-line` / `move-end-of-line` |
| `C-v` / `M-v` | `scroll-up` / `scroll-down` |
| `M-<` / `M->` | `beginning-of-buffer` / `end-of-buffer` |
| `M-g g` | `goto-line` |
| `C-SPC` | `set-mark-command` (`C-u C-SPC` pops the mark ring) |
| `C-x C-x` | `exchange-point-and-mark` |
| `M-w` | `kill-ring-save` |
| `C-w` | `kill-region` (read-only buffer: same as `M-w`) |
| `C-h k` | `describe-key` |
| `C-h b` | `describe-bindings` |
| `q` / `ESC` | `exit-text-mode` |

## Minibuffer map (`M-x`)

| Keys | Command |
|---|---|
| `RET` | `exit-minibuffer` |
| `TAB` | `minibuffer-complete` |
| `DEL` | `delete-backward-char` |
| `C-k` | `kill-line` |
| `C-w` / `M-DEL` | `backward-kill-word` |
| `C-a` / `C-e` | `move-beginning-of-line` / `move-end-of-line` |
| `C-f` / `C-b` | `forward-char` / `backward-char` |
| `C-n` / `C-p` | `next-line` / `previous-line` (candidate selection) |
| `C-y` | `yank` |
| `C-g` | `keyboard-quit` (abort) |

> Deviation from GNU Emacs: `C-w` in the minibuffer is `backward-kill-word`,
> not `kill-region` — a minibuffer has no mark, so `kill-region` would be
> inert.

## Prefix arguments

`C-u` gives 4; `C-u C-u` gives 16; `C-u 5` (or `M-5`) gives 5; digits after
the first accumulate (`C-u 5 3` = 53). Motions repeat. `C-u C-SPC` pops the
mark ring. `C-u 2 M-x switch-tab` targets the 2nd tab — the three indexed
herdr commands (`switch-tab`, `switch-workspace`, `focus-agent`) take their
1-based index from the prefix argument.

## Every command (the `M-x` namespace)

Any of these can be bound in `[emacs.keys]` or run with `M-x`.

**Layer builtins:** `backward-char`, `backward-kill-word`, `backward-word`,
`beginning-of-buffer`, `delete-backward-char`, `describe-bindings`,
`describe-key`, `end-of-buffer`, `exchange-point-and-mark`,
`execute-extended-command`, `exit-minibuffer`, `exit-text-mode`,
`forward-char`, `forward-word`, `goto-line`, `keyboard-quit`, `kill-line`,
`kill-region`, `kill-ring-save`, `minibuffer-complete`,
`move-beginning-of-line`, `move-end-of-line`, `move-tab-left`,
`move-tab-right`, `next-line`, `previous-line`, `quoted-insert`,
`scroll-down`, `scroll-up`, `set-mark-command`, `text-mode`,
`universal-argument`, `yank`, `yank-pop`.

**herdr actions — Emacs vocabulary:** `split-window-right`,
`split-window-below`, `delete-window`, `delete-other-windows`,
`other-window`, `previous-window`, `switch-to-buffer`, `windmove-left`,
`windmove-down`, `windmove-up`, `windmove-right`,
`windmove-swap-states-left`, `windmove-swap-states-down`,
`windmove-swap-states-up`, `windmove-swap-states-right`, `kill-tab`.

**herdr actions — herdr vocabulary:** `new-workspace`, `new-worktree`,
`open-worktree`, `remove-worktree`, `rename-workspace`, `close-workspace`,
`workspace-picker`, `previous-workspace`, `next-workspace`,
`previous-agent`, `next-agent`, `new-tab`, `rename-tab`, `previous-tab`,
`next-tab`, `rename-pane`, `edit-scrollback`, `copy-mode`, `resize-mode`,
`toggle-sidebar`, `last-pane`, `herdr-help`, `settings`, `reload-config`,
`open-navigator-notification-target`, `detach`, `switch-workspace`,
`switch-tab`, `focus-agent`.

## Not included

isearch (`C-s`/`C-r`), occur, and keyboard macros.
````

- [ ] **Step 5: Run the doc-coverage test**

Run: `cargo test --locked --bin herdr emacs::commands::tests::every_command_appears_in_the_bindings_reference`
Expected: `test result: ok. 1 passed`. If it fails, it prints exactly which command names are missing from the doc — add them to the lists above.

- [ ] **Step 6: Refresh the config reference.** In `src/main.rs`'s `DEFAULT_CONFIG`, replace the `[emacs]` block with:

```toml

[emacs]
# Emacs keyboard layer (fork feature): a layered keymap stack over herdr.
# C-x chords for window/tab/workspace management, a read-only TEXT mode over
# pane scrollback (C-x [), prefix arguments (C-u), M-x with completion over
# every command, and C-h/F1 help. When disabled, this build behaves exactly
# like stock herdr. See docs/emacs-layer.md.
# enabled = false
# Sync the kill-ring head with the system clipboard.
# clipboard_sync = true
# kill_ring_max = 60
# mark_ring_max = 16
# Bind any command (M-x names them all: `F1 b` lists the live stack):
# [emacs.keys]
# "C-x 4" = "split-window-right"
# "C-x t" = "toggle-sidebar"
```

In BOTH `docs/next/website/src/data/config-reference.json` and
`website/src/data/config-reference.json`, update the two `emacs` keys whose
descriptions are now stale (leave `enabled`, `kill_ring_max`,
`mark_ring_max` alone):

```json
  {
   "key": "emacs.enabled",
   "type": "boolean",
   "default": "false",
   "description": "Enable the Emacs keyboard layer (fork feature): a layered keymap stack, TEXT mode over pane scrollback, prefix arguments, M-x, and C-h help."
  },
```

```json
  {
   "key": "emacs.keys",
   "type": "table",
   "default": "{}",
   "description": "Bind any command: chord sequence (e.g. \"C-x 4\") to command name (e.g. \"split-window-right\"). Every layer builtin and every herdr action has a name; run \"F1 b\" to list the active bindings."
  }
```

- [ ] **Step 7: Verify the reference checker**

Run: `python3 scripts/config_reference_check.py && diff website/src/data/config-reference.json docs/next/website/src/data/config-reference.json && echo REFERENCE-OK`
Expected: `REFERENCE-OK`

- [ ] **Step 8: Run the full layer suite one last time**

Run: `cargo test --locked --bin herdr emacs:: app::input::emacs config::`
Expected: `test result: ok.` on all three

Run: `cargo test --locked --bin herdr 2>&1 | tail -5`
Expected: only the 14 documented upstream env-race flakes may fail. Re-run any failure in isolation (`cargo test --locked --bin herdr <exact::test::name>`) before treating it as a regression.

- [ ] **Step 9: Commit** (note the `-f`: `docs/` is gitignored except `docs/next/`)

```bash
cargo fmt
git add -f docs/emacs-layer.md
git add src/emacs/commands.rs src/main.rs \
  docs/next/website/src/data/config-reference.json website/src/data/config-reference.json
git commit -m "docs: emacs layer bindings reference and refreshed config reference"
```

---

## Spec coverage

| Spec | Task |
|---|---|
| §3.1 Keymap stack (`active_maps`, first-Bound, union-Prefix) | 1 |
| §3.2 Key normalization, one-directional fold, full coverage table | 2 |
| §3.3 Undefined keys speak; read-only only for self-insert | 3 |
| §3.4 `EmacsCommand::{Builtin,Herdr}`, exhaustive match over every action | 4 |
| §3.5 Minibuffer, `M-x`, fuzzy completion, vertical candidate list | 8, 9 |
| §3.6 Prefix arguments (`C-u`, `M-<digit>`, repeat, `C-u C-SPC`) | 7 |
| §3.7 `C-h` help prefix, `describe-key`, `describe-bindings`, F1 | 10 |
| §3.8 `C-[`/`C-]` tab navigation, `M-[`/`M-]` tab reordering (clamped) | 5 |
| §4 Config surface; binding errors as diagnostics, not `tracing::warn!` | 6, 11 |
| §6 Testing (all bullets) | 1-10 |
| §7.1 `C-x [` then `C-x 3` splits from TEXT mode | 1 |
| §7.2 `C-h b` lists every binding; `C-h k C-x 3` answers | 10 |
| §7.3 `M-x toggle-sidebar` runs an unbound herdr action | 8, 9 |
| §7.4 `C-u 5 C-f` moves five characters | 7 |
| §7.5 `"M-h" = "mark-paragraph"`-style config binding works | 4 |
| §7.6 A new upstream `NavigateAction` fails the build until named | 4 (Step 8 proves it) |
| §7.7 `[emacs] enabled = false` → stock herdr | every task (the existing test must stay green) |
