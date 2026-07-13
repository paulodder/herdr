# Emacs Layer Phase 0 + Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement Phase 0 (C-x management layer, `[emacs]` config, fork guards) and Phase 1 (TEXT mode with point/mark/region, Emacs motions over scrollback, kill ring + mark ring, clipboard sync, echo area) of the Emacs layer described in `docs/superpowers/specs/2026-07-13-emacs-layer-design.md`. Phases 2–4 (isearch/occur, minibuffer/M-x, prefix args/kmacros) are explicitly OUT OF SCOPE.

**Architecture:** A pure, unit-testable engine in a new `src/emacs/` module (chord parser, keymap, command table, rings, motions) plus one thin `App` adapter file `src/app/input/emacs.rs` that executes commands. Upstream code is touched at small, well-marked seams: one key-interception hook in `App::route_client_events`, two 1-line render calls, read-side scrollback accessors, config plumbing, and two fork guards.

**Tech Stack:** Rust 2021, crossterm 0.29, ratatui 0.30, serde/toml, tokio; vendored libghostty-vt built via zig 0.15.2.

## Deviations from the design spec (discovered against real source)

The spec's architecture section made assumptions that do not match the codebase. The plan is designed around reality:

1. **Key dispatch is SERVER-side, not client-side.** The spec says "a hook early in the client key-dispatch path (`src/input/`)". In reality the client (`src/client/input.rs`) forwards **raw bytes** to the server; the server parses them (`src/raw_input.rs`) into `RawInputEvent::Key(TerminalKey)` and interprets them in `App::route_client_events` (`src/app/mod.rs:1549`). `App::handle_key` (`src/app/input/mod.rs:72`) exists but is only called from tests — the production path is exclusively `route_client_events`. **The hook goes there.** Bonus: `App::route_client_input(bytes)` (`src/app/mod.rs:1544`, `#[cfg(test)]`) drives the full parse+dispatch path from unit tests.
2. **The `App` glue cannot live in `src/emacs/`.** Executing herdr actions requires `App::execute_tui_navigate_action` and `AppState::set_pane_scroll_offset`, which are `pub(super)` inside `src/app/input/`. Rust visibility therefore forces the adapter into a NEW file `src/app/input/emacs.rs` (registered with one `mod emacs;` line). All pure logic still lives in `src/emacs/`. Net upstream-file edits stay tiny.
3. **There is no bottom status line to reuse.** herdr has floating toasts (`render_notifications`, `src/ui.rs:461`), not a persistent bottom bar. The echo area is drawn as a one-line overlay on the bottom row of the terminal area via one call added in `render_with_runtime_registry` (`src/ui.rs:403`).
4. **Copy-mode state is a template, not a base.** `CopyModeState` (`src/app/state.rs`) uses viewport-relative cursor coordinates. TEXT mode instead stores **absolute scrollback rows** (`u32`, same addressing as `Selection::line_range` and `TerminalTextMatch`); the viewport follows the point. We do not touch copy mode.
5. **No `Mode` enum variant is added.** `Mode` is matched exhaustively in many files; adding a variant would create a wide rebase surface. TEXT mode is `app.state.emacs.text_mode: Option<TextModeState>` while `mode` stays `Mode::Terminal`; the interception hook owns all keys while it is `Some`.
6. **Toolchain quirk:** `rust-toolchain.toml` pins 1.96.1 but only stable 1.88.0 is installed and the `~/.cargo/bin` rustup shims are absent. Invoking cargo directly from the toolchain dir bypasses the pin and **builds cleanly** (verified: full `cargo build --locked` succeeds in 4m45s). The vendored libghostty-vt needs zig 0.15.2 (installed at `~/.local/opt/zig-x86_64-linux-0.15.2/zig`; CI pins the same version).

**Known accepted limitations (document, don't fix in Phase 1):** motions treat one `char` as one grid column (wide/CJK cells drift, same class of limitation as upstream copy-mode column math); `next-line` clamps to line length instead of tracking an Emacs goal column; live-mode `M-y` replaces the previous yank by sending backspaces, which is unreliable for multi-line yanks into line-editing prompts.

## Global Constraints

- **Environment (every shell):**
  ```bash
  export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"
  export ZIG="$HOME/.local/opt/zig-x86_64-linux-0.15.2/zig"
  cd /home/paul/projects/herdr
  ```
- Branch: `emacs` (fork of `ogulcancelik/herdr`, remote `upstream`). Never commit to `master`.
- `[emacs] enabled = false` (the default) must yield **bit-for-bit stock herdr behavior** — every seam is gated on `state.emacs.enabled` or is a pure addition.
- Keep upstream-file edits minimal and commented with `// Emacs layer seam (fork):` — each task lists its rebase surface.
- Every command is a **named command** in the command table taking `Option<i64>` prefix arg (C-u itself is Phase 4; the calling convention is wired now).
- Test command: `cargo test --locked --bin herdr <filter>`. Full unit suite is ~2500 tests. `cargo-nextest` is NOT installed; do not use `just test`.
- Format with `cargo fmt` before every commit. `src/emacs/mod.rs` carries `#![allow(dead_code)]` until Phase 2 fills in the remaining commands.
- Adding config keys requires updating **both** `docs/next/website/src/data/config-reference.json` and `website/src/data/config-reference.json` (they must stay identical; `scripts/config_reference_check.py` walks the serde model and fails on drift).
- Requires a kitty-keyboard-protocol terminal (Ghostty) for chords like `C-SPC`, `M-<` in live use; the raw-input parser also decodes the legacy encodings used in tests (`0x00` → `C-SPC`, `ESC w` → `M-w`).
- Do not implement backward compatibility or config migration — hard cutover, personal fork.

## File Structure

New files (zero rebase conflict surface):

| File | Responsibility |
|---|---|
| `src/emacs/mod.rs` | Module root: `EmacsState` (all layer state), config application |
| `src/emacs/keymap.rs` | `Chord`, Emacs chord-string parser, `Keymap<T>`, `Lookup` |
| `src/emacs/commands.rs` | `EmacsCommand` enum + name table + default keymaps + config overrides |
| `src/emacs/rings.rs` | `KillRing`, `MarkRing` |
| `src/emacs/text_mode.rs` | `Pos`, `TextBuffer` trait, pure motion functions, `TextModeState` |
| `src/emacs/render.rs` | TEXT-mode point/region overlay + echo-area rendering (pure draw fns) |
| `src/app/input/emacs.rs` | `App` adapter: interception hook body, command executor, PTY/clipboard IO, tests |

Modified upstream files (the whole rebase surface of Phase 0+1):

| File | Edit |
|---|---|
| `src/main.rs` | `mod emacs;` + `[emacs]` block in `DEFAULT_CONFIG` |
| `src/config/model.rs` | `EmacsConfig` struct + `emacs` field on `Config` |
| `src/config/io.rs` | `"emacs"` in `KNOWN_TOP_LEVEL_CONFIG_KEYS` + `load_live_section` call |
| `src/config.rs` | re-export `EmacsConfig` |
| `src/app/state.rs` | `emacs` field on `AppState` + init in `test_new()` |
| `src/app/mod.rs` | init in `App::new`, hook in `route_client_events`, `apply_live_config` branch, `auto_updates_enabled` guard |
| `src/app/input/mod.rs` | `mod emacs;` |
| `src/update.rs` | early-return fork guard in `self_update` |
| `src/ui.rs` | 1 call: echo area |
| `src/ui/panes.rs` | 1 call: TEXT-mode overlay + 1-condition cursor gate |
| `src/pane/terminal.rs`, `src/pane.rs`, `src/terminal/runtime.rs` | read-side scrollback accessors (`text_dims`, `text_row`, `read_text_range`) |
| `docs/next/website/src/data/config-reference.json`, `website/src/data/config-reference.json` | `[emacs]` reference section |

---

### Task 1: `[emacs]` config section

**Files:**
- Modify: `src/config/model.rs` (Config struct at :35-51; add `EmacsConfig` after `UpdateConfig` impl ~:60)
- Modify: `src/config/io.rs` (`KNOWN_TOP_LEVEL_CONFIG_KEYS` at :7-19; live-section loading before the `Ok(LoadedConfig {` return ~:328)
- Modify: `src/config.rs` (re-export list at :21-26)
- Modify: `src/main.rs` (`DEFAULT_CONFIG` string; add `[emacs]` block after the `[advanced]` lines, immediately before the closing `"##;` at ~:403)
- Modify: `docs/next/website/src/data/config-reference.json` and `website/src/data/config-reference.json` (append a section object to `"sections"`)
- Test: inline `#[cfg(test)]` tests in `src/config/model.rs` and `src/config/io.rs`

**Interfaces:**
- Produces: `crate::config::EmacsConfig { enabled: bool, clipboard_sync: bool, kill_ring_max: usize, mark_ring_max: usize, keys: HashMap<String, String> }`, reachable as `config.emacs`. Task 4 consumes it via `EmacsState::from_config(&config.emacs)`.

- [ ] **Step 1: Write the failing test** — in the `#[cfg(test)] mod tests` at the bottom of `src/config/model.rs`, following the style of the existing config tests there:

```rust
#[test]
fn emacs_config_defaults_and_parses() {
    let config = Config::default();
    assert!(!config.emacs.enabled);
    assert!(config.emacs.clipboard_sync);
    assert_eq!(config.emacs.kill_ring_max, 60);
    assert_eq!(config.emacs.mark_ring_max, 16);
    assert!(config.emacs.keys.is_empty());

    let parsed: Config = toml::from_str(
        r#"
[emacs]
enabled = true
clipboard_sync = false
kill_ring_max = 10
mark_ring_max = 4

[emacs.keys]
"C-x c" = "new-tab"
"#,
    )
    .expect("emacs config parses");
    assert!(parsed.emacs.enabled);
    assert!(!parsed.emacs.clipboard_sync);
    assert_eq!(parsed.emacs.kill_ring_max, 10);
    assert_eq!(parsed.emacs.mark_ring_max, 4);
    assert_eq!(
        parsed.emacs.keys.get("C-x c").map(String::as_str),
        Some("new-tab")
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked --bin herdr config::model::tests::emacs_config_defaults_and_parses`
Expected: compile error `no field `emacs` on type `Config``

- [ ] **Step 3: Add `EmacsConfig` to `src/config/model.rs`** — insert after the `default_update_channel` function (mirror `UpdateConfig`'s derive style; note upstream uses `#[serde(default)]` + manual `Default`):

```rust
/// Emacs keyboard layer (fork feature): C-x management chords and TEXT mode
/// over pane scrollback. Off by default; with `enabled = false` the fork
/// behaves exactly like stock herdr.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EmacsConfig {
    pub enabled: bool,
    pub clipboard_sync: bool,
    pub kill_ring_max: usize,
    pub mark_ring_max: usize,
    /// Binding overrides: Emacs chord sequence -> command name,
    /// e.g. `"C-x c" = "new-tab"`.
    pub keys: std::collections::HashMap<String, String>,
}

impl Default for EmacsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            clipboard_sync: true,
            kill_ring_max: 60,
            mark_ring_max: 16,
            keys: std::collections::HashMap::new(),
        }
    }
}
```

Then add the field to `Config` (after `pub remote: RemoteConfig,`):

```rust
    pub emacs: EmacsConfig,
```

And add `EmacsConfig` to the `model::{...}` re-export group in `src/config.rs` (alphabetical position, before `HostCursorModeConfig`):

```rust
        validated_sidebar_bounds, AgentPanelSortConfig, Config, ConfigReloadReport,
        ConfigReloadStatus, EmacsConfig, HostCursorModeConfig, NewTerminalCwdConfig,
        ShellModeConfig, SidebarCollapsedModeConfig, ToastClipboardPosition, ToastConfig,
        ToastDelivery, ToastHerdrPosition, UpdateChannelConfig, MAX_TOAST_DELAY_SECONDS,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --locked --bin herdr config::model::tests::emacs_config_defaults_and_parses`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: Write the failing live-reload test** — in `mod tests` of `src/config/io.rs` (at :583), directly after `load_live_config_parses_session_section` (:695), using the same file-local helper `load_live_config_from_str`:

```rust
#[test]
fn load_live_config_parses_emacs_section() {
    let loaded = load_live_config_from_str(
        r#"
[emacs]
enabled = true
"#,
    )
    .unwrap();

    assert!(loaded.config.emacs.enabled);
    assert!(loaded.diagnostics.is_empty());
    assert!(loaded.invalid_sections.is_empty());
}
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test --locked --bin herdr config::io`
Expected: the new test FAILS — `[emacs]` is reported as an unknown section (or `config.emacs.enabled` stays false because the live loader never copies the section).

- [ ] **Step 7: Wire `[emacs]` into `src/config/io.rs`.** Add `"emacs"` to `KNOWN_TOP_LEVEL_CONFIG_KEYS` (alphabetical):

```rust
const KNOWN_TOP_LEVEL_CONFIG_KEYS: &[&str] = &[
    "advanced",
    "emacs",
    "experimental",
    "keys",
    "onboarding",
    "remote",
    "session",
    "terminal",
    "theme",
    "ui",
    "update",
    "worktrees",
];
```

And add a `load_live_section` call directly after the existing `"remote"` one (before `Ok(LoadedConfig {`):

```rust
    load_live_section(
        table,
        "emacs",
        "emacs config",
        &mut diagnostics,
        &mut invalid_sections,
        |section| config.emacs = section,
    );
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test --locked --bin herdr config::io`
Expected: `test result: ok.` (all `config::io` tests pass, including the new one)

- [ ] **Step 9: Document the section.** In `src/main.rs`, extend `DEFAULT_CONFIG` by inserting before the closing `"##;` (after the `[advanced]` block):

```toml

[emacs]
# Emacs keyboard layer (fork feature): C-x chords for tab/pane/workspace
# management and a read-only TEXT mode over pane scrollback.
# When disabled, this build behaves exactly like stock herdr.
# enabled = false
# Sync the kill-ring head with the system clipboard.
# clipboard_sync = true
# kill_ring_max = 60
# mark_ring_max = 16
# Override default bindings with Emacs key syntax under [emacs.keys]:
# [emacs.keys]
# "C-x c" = "new-tab"
```

Then append this object to the `"sections"` array in **both** `docs/next/website/src/data/config-reference.json` and `website/src/data/config-reference.json` (identical content; match the JSON style of the `"update"` section):

```json
{
 "id": "emacs",
 "title": "Emacs layer",
 "keys": [
  {
   "key": "emacs.enabled",
   "type": "boolean",
   "default": "false",
   "description": "Enable the Emacs keyboard layer (fork feature): C-x management chords and TEXT mode over pane scrollback."
  },
  {
   "key": "emacs.clipboard_sync",
   "type": "boolean",
   "default": "true",
   "description": "Sync the kill-ring head with the system clipboard."
  },
  {
   "key": "emacs.kill_ring_max",
   "type": "integer",
   "default": "60",
   "description": "Maximum number of kill ring entries."
  },
  {
   "key": "emacs.mark_ring_max",
   "type": "integer",
   "default": "16",
   "description": "Maximum number of mark ring entries per pane."
  },
  {
   "key": "emacs.keys",
   "type": "table",
   "default": "{}",
   "description": "Override default Emacs bindings: chord sequence (e.g. \"C-x c\") to command name (e.g. \"new-tab\")."
  }
 ]
}
```

- [ ] **Step 10: Verify the reference checker and full config tests**

Run: `python3 scripts/config_reference_check.py && diff website/src/data/config-reference.json docs/next/website/src/data/config-reference.json && echo REFERENCE-OK`
Expected: `REFERENCE-OK` (checker prints errors and exits non-zero on drift; fix any `emacs.*: in src/config but missing from ...` messages it names)

Run: `cargo test --locked --bin herdr config::`
Expected: `test result: ok.` (~50 tests)

- [ ] **Step 11: Commit**

```bash
cargo fmt
git add src/config/model.rs src/config/io.rs src/config.rs src/main.rs \
  docs/next/website/src/data/config-reference.json website/src/data/config-reference.json
git commit -m "feat: add [emacs] config section"
```

### Task 2: `src/emacs` module skeleton + keymap/chord engine

**Files:**
- Create: `src/emacs/mod.rs`
- Create: `src/emacs/keymap.rs`
- Modify: `src/main.rs` (module list; insert `mod emacs;` between `mod detect;` at :66 and `mod events;` at :67)
- Test: inline `#[cfg(test)]` tests in `src/emacs/keymap.rs`

**Interfaces:**
- Consumes: `crate::input::TerminalKey { code: KeyCode, modifiers: KeyModifiers, kind, shifted_codepoint }` (`src/input/model.rs:6`).
- Produces (used by Tasks 3, 4, 12):
  - `Chord { ctrl: bool, meta: bool, code: KeyCode }`, `Chord::from_key(&TerminalKey) -> Option<Chord>`, `Chord::ctrl(char) -> Chord`
  - `parse_chord(&str) -> Option<Chord>`, `parse_key_seq(&str) -> Option<Vec<Chord>>`, `format_seq(&[Chord]) -> String`
  - `Keymap<T: Copy>` with `bind(Vec<Chord>, T)` and `lookup(&[Chord]) -> Lookup<T>`; `Lookup<T> { Bound(T), Prefix, Unbound }`

- [ ] **Step 1: Register the module.** In `src/main.rs` add to the module list:

```rust
mod detect;
mod emacs;
mod events;
```

Create `src/emacs/mod.rs`:

```rust
//! Emacs keyboard layer (fork feature).
//!
//! Pure engine code lives in this module: chord parsing, keymaps, the
//! command table, kill/mark rings, and TEXT-mode motion logic. The thin
//! `App` adapter that executes commands lives in `src/app/input/emacs.rs`
//! because command execution needs `pub(super)` App internals.
//!
//! Everything is keyed off `[emacs] enabled` in config: when disabled, the
//! layer consumes no keys and the fork behaves as stock herdr.
#![allow(dead_code)] // remove when Phase 2 (isearch/occur) fills in the remaining commands

pub mod keymap;
```

- [ ] **Step 2: Write the failing chord-parser tests.** Create `src/emacs/keymap.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn parses_single_chords() {
        assert_eq!(
            parse_chord("C-x"),
            Some(Chord { ctrl: true, meta: false, code: KeyCode::Char('x') })
        );
        assert_eq!(
            parse_chord("M-<"),
            Some(Chord { ctrl: false, meta: true, code: KeyCode::Char('<') })
        );
        assert_eq!(
            parse_chord("C-SPC"),
            Some(Chord { ctrl: true, meta: false, code: KeyCode::Char(' ') })
        );
        assert_eq!(
            parse_chord("RET"),
            Some(Chord { ctrl: false, meta: false, code: KeyCode::Enter })
        );
        assert_eq!(
            parse_chord("["),
            Some(Chord { ctrl: false, meta: false, code: KeyCode::Char('[') })
        );
        assert_eq!(parse_chord(""), None);
        assert_eq!(parse_chord("C-"), None);
        assert_eq!(parse_chord("xy"), None);
    }

    #[test]
    fn parses_key_sequences() {
        let seq = parse_key_seq("C-x C-x").expect("seq parses");
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0], Chord::ctrl('x'));
        assert_eq!(seq[1], Chord::ctrl('x'));
        assert_eq!(parse_key_seq("M-g g").map(|s| s.len()), Some(2));
        assert_eq!(parse_key_seq(""), None);
        assert_eq!(parse_key_seq("C-x nope"), None);
    }

    #[test]
    fn formats_sequences_for_echo() {
        let seq = parse_key_seq("C-x [").unwrap();
        assert_eq!(format_seq(&seq), "C-x [");
        assert_eq!(format_seq(&[parse_chord("C-SPC").unwrap()]), "C-SPC");
        assert_eq!(format_seq(&[parse_chord("M-<").unwrap()]), "M-<");
    }

    #[test]
    fn chord_from_terminal_key_ignores_shift_on_chars() {
        use crate::input::TerminalKey;
        let key = TerminalKey::new(
            KeyCode::Char('<'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        assert_eq!(Chord::from_key(&key), parse_chord("M-<"));
        let key = TerminalKey::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(Chord::from_key(&key), parse_chord("C-x"));
        let key = TerminalKey::new(KeyCode::Media(crossterm::event::MediaKeyCode::Play), KeyModifiers::empty());
        assert_eq!(Chord::from_key(&key), None);
    }

    #[test]
    fn keymap_lookup_distinguishes_bound_prefix_unbound() {
        let mut map: Keymap<u8> = Keymap::default();
        map.bind(parse_key_seq("C-x 2").unwrap(), 1);
        map.bind(parse_key_seq("C-q").unwrap(), 2);
        assert_eq!(map.lookup(&parse_key_seq("C-x 2").unwrap()), Lookup::Bound(1));
        assert_eq!(map.lookup(&parse_key_seq("C-x").unwrap()), Lookup::Prefix);
        assert_eq!(map.lookup(&parse_key_seq("C-q").unwrap()), Lookup::Bound(2));
        assert_eq!(map.lookup(&parse_key_seq("C-z").unwrap()), Lookup::Unbound);
        assert_eq!(map.lookup(&parse_key_seq("C-x 3").unwrap()), Lookup::Unbound);
        // rebinding replaces
        map.bind(parse_key_seq("C-x 2").unwrap(), 9);
        assert_eq!(map.lookup(&parse_key_seq("C-x 2").unwrap()), Lookup::Bound(9));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::keymap`
Expected: compile error — `Chord`, `parse_chord`, etc. not found

- [ ] **Step 4: Implement the engine.** Add above the test module in `src/emacs/keymap.rs`:

```rust
//! Emacs chord syntax (`C-x`, `M-<`, `C-SPC`, sequences like `C-x C-x`)
//! and prefix-aware keymap lookup.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::input::TerminalKey;

/// A single Emacs chord: modifiers + base key.
///
/// SHIFT is intentionally not modeled for `Char` keys — the character
/// itself already encodes it (`M-<` arrives as ALT+SHIFT+'<'), matching
/// Emacs key notation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub ctrl: bool,
    pub meta: bool,
    pub code: KeyCode,
}

impl Chord {
    pub fn ctrl(c: char) -> Self {
        Self { ctrl: true, meta: false, code: KeyCode::Char(c) }
    }

    /// Normalize a decoded terminal key into a chord. Returns `None` for
    /// keys the layer never binds (media keys, modifier-only events, ...).
    pub fn from_key(key: &TerminalKey) -> Option<Self> {
        match key.code {
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
            | KeyCode::F(_) => Some(Self {
                ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
                meta: key.modifiers.contains(KeyModifiers::ALT),
                code: key.code,
            }),
            _ => None,
        }
    }
}

fn named_key(name: &str) -> Option<KeyCode> {
    Some(match name {
        "SPC" => KeyCode::Char(' '),
        "RET" => KeyCode::Enter,
        "TAB" => KeyCode::Tab,
        "ESC" => KeyCode::Esc,
        "DEL" => KeyCode::Backspace,
        _ => {
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    })
}

/// Parse one chord in Emacs notation: optional `C-`/`M-` prefixes followed
/// by a single character or a named key (`SPC`, `RET`, `TAB`, `ESC`, `DEL`).
pub fn parse_chord(s: &str) -> Option<Chord> {
    let mut ctrl = false;
    let mut meta = false;
    let mut rest = s;
    loop {
        if let Some(r) = rest.strip_prefix("C-") {
            if r.is_empty() {
                return None;
            }
            ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("M-") {
            if r.is_empty() {
                return None;
            }
            meta = true;
            rest = r;
        } else {
            break;
        }
    }
    if rest.is_empty() {
        return None;
    }
    Some(Chord { ctrl, meta, code: named_key(rest)? })
}

/// Parse a whitespace-separated chord sequence, e.g. `"C-x C-x"`.
pub fn parse_key_seq(s: &str) -> Option<Vec<Chord>> {
    let chords: Option<Vec<Chord>> = s.split_whitespace().map(parse_chord).collect();
    match chords {
        Some(chords) if !chords.is_empty() => Some(chords),
        _ => None,
    }
}

fn format_chord(chord: &Chord) -> String {
    let mut out = String::new();
    if chord.ctrl {
        out.push_str("C-");
    }
    if chord.meta {
        out.push_str("M-");
    }
    match chord.code {
        KeyCode::Char(' ') => out.push_str("SPC"),
        KeyCode::Char(c) => out.push(c),
        KeyCode::Enter => out.push_str("RET"),
        KeyCode::Tab => out.push_str("TAB"),
        KeyCode::Esc => out.push_str("ESC"),
        KeyCode::Backspace => out.push_str("DEL"),
        other => out.push_str(&format!("{other:?}")),
    }
    out
}

/// Render a chord sequence for the echo area, e.g. `"C-x ["`.
pub fn format_seq(seq: &[Chord]) -> String {
    seq.iter().map(format_chord).collect::<Vec<_>>().join(" ")
}

/// Result of looking up an accumulated chord sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup<T> {
    /// The sequence exactly matches a binding.
    Bound(T),
    /// The sequence is a proper prefix of at least one binding.
    Prefix,
    /// Nothing matches.
    Unbound,
}

/// A flat keymap: chord sequences bound to values (command identifiers).
/// Linear scan — keymaps hold a few dozen bindings.
#[derive(Debug, Clone)]
pub struct Keymap<T> {
    bindings: Vec<(Vec<Chord>, T)>,
}

// Manual impl: `derive(Default)` would wrongly require `T: Default`
// (EmacsCommand has no Default), breaking `KeymapSet::default()`.
impl<T> Default for Keymap<T> {
    fn default() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }
}

impl<T: Copy> Keymap<T> {
    /// Bind `seq` to `value`, replacing any existing binding for `seq`.
    pub fn bind(&mut self, seq: Vec<Chord>, value: T) {
        if let Some(entry) = self.bindings.iter_mut().find(|(s, _)| *s == seq) {
            entry.1 = value;
        } else {
            self.bindings.push((seq, value));
        }
    }

    pub fn lookup(&self, seq: &[Chord]) -> Lookup<T> {
        let mut is_prefix = false;
        for (bound, value) in &self.bindings {
            if bound.as_slice() == seq {
                return Lookup::Bound(*value);
            }
            if bound.len() > seq.len() && bound.starts_with(seq) {
                is_prefix = true;
            }
        }
        if is_prefix {
            Lookup::Prefix
        } else {
            Lookup::Unbound
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::keymap`
Expected: `test result: ok. 5 passed`

- [ ] **Step 6: Verify the whole binary still builds warnings-free enough to commit**

Run: `cargo build --locked 2>&1 | tail -3`
Expected: `Finished \`dev\` profile` with no `warning:` lines mentioning `emacs` (the `#![allow(dead_code)]` covers unused items)

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add src/emacs src/main.rs
git commit -m "feat: emacs chord parser and keymap engine"
```

---

### Task 3: Command table (the spine)

**Files:**
- Create: `src/emacs/commands.rs`
- Modify: `src/emacs/mod.rs` (add `pub mod commands;`)
- Test: inline `#[cfg(test)]` tests in `src/emacs/commands.rs`

**Interfaces:**
- Consumes: `Keymap`, `Lookup`, `parse_key_seq` from Task 2.
- Produces (used by Tasks 4, 7–12):
  - `EmacsCommand` (Copy enum, all Phase 0+1 commands), `EmacsCommand::name(self) -> &'static str`, `EmacsCommand::from_name(&str) -> Option<EmacsCommand>`
  - `KeymapSet { pub global: Keymap<EmacsCommand>, pub text: Keymap<EmacsCommand> }`
  - `build_keymaps(&HashMap<String, String>) -> (KeymapSet, Vec<String>)` — defaults plus `[emacs.keys]` overrides; second element is warnings for invalid entries.

- [ ] **Step 1: Write the failing tests.** Create `src/emacs/commands.rs` starting with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::emacs::keymap::{parse_key_seq, Lookup};

    #[test]
    fn command_names_round_trip() {
        for (cmd, name) in COMMAND_NAMES.iter().copied() {
            assert_eq!(cmd.name(), name);
            assert_eq!(EmacsCommand::from_name(name), Some(cmd));
        }
        assert_eq!(EmacsCommand::from_name("no-such-command"), None);
    }

    #[test]
    fn default_global_keymap_binds_management_chords() {
        let (keymaps, warnings) = build_keymaps(&Default::default());
        assert!(warnings.is_empty());
        let cases = [
            ("C-x 2", EmacsCommand::SplitWindowBelow),
            ("C-x 3", EmacsCommand::SplitWindowRight),
            ("C-x o", EmacsCommand::OtherWindow),
            ("C-x 0", EmacsCommand::DeleteWindow),
            ("C-x 1", EmacsCommand::DeleteOtherWindows),
            ("C-x b", EmacsCommand::SwitchToBuffer),
            ("C-x c", EmacsCommand::NewTab),
            ("C-x n", EmacsCommand::NextTab),
            ("C-x p", EmacsCommand::PreviousTab),
            ("C-x k", EmacsCommand::KillTab),
            ("C-x w", EmacsCommand::WorkspacePicker),
            ("C-x [", EmacsCommand::TextMode),
            ("C-q", EmacsCommand::QuotedInsert),
            ("C-y", EmacsCommand::Yank),
            ("M-y", EmacsCommand::YankPop),
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
            ("C-f", EmacsCommand::ForwardChar),
            ("C-b", EmacsCommand::BackwardChar),
            ("C-n", EmacsCommand::NextLine),
            ("C-p", EmacsCommand::PreviousLine),
            ("M-f", EmacsCommand::ForwardWord),
            ("M-b", EmacsCommand::BackwardWord),
            ("C-a", EmacsCommand::MoveBeginningOfLine),
            ("C-e", EmacsCommand::MoveEndOfLine),
            ("C-v", EmacsCommand::ScrollUp),
            ("M-v", EmacsCommand::ScrollDown),
            ("M-<", EmacsCommand::BeginningOfBuffer),
            ("M->", EmacsCommand::EndOfBuffer),
            ("M-g g", EmacsCommand::GotoLine),
            ("C-SPC", EmacsCommand::SetMark),
            ("C-x C-x", EmacsCommand::ExchangePointAndMark),
            ("M-w", EmacsCommand::KillRingSave),
            ("C-w", EmacsCommand::KillRegion),
            ("C-y", EmacsCommand::Yank),
            ("M-y", EmacsCommand::YankPop),
            ("C-g", EmacsCommand::KeyboardQuit),
            ("q", EmacsCommand::ExitTextMode),
            ("ESC", EmacsCommand::ExitTextMode),
        ];
        for (seq, cmd) in cases {
            assert_eq!(
                keymaps.text.lookup(&parse_key_seq(seq).unwrap()),
                Lookup::Bound(cmd),
                "text {seq}"
            );
        }
        assert_eq!(
            keymaps.text.lookup(&parse_key_seq("M-g").unwrap()),
            Lookup::Prefix
        );
    }

    #[test]
    fn config_overrides_rebind_and_warn() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("C-x t".to_string(), "new-tab".to_string());
        overrides.insert("C-x c".to_string(), "other-window".to_string());
        overrides.insert("C-x z".to_string(), "no-such-command".to_string());
        overrides.insert("???".to_string(), "new-tab".to_string());
        let (keymaps, warnings) = build_keymaps(&overrides);
        assert_eq!(
            keymaps.global.lookup(&parse_key_seq("C-x t").unwrap()),
            Lookup::Bound(EmacsCommand::NewTab)
        );
        assert_eq!(
            keymaps.global.lookup(&parse_key_seq("C-x c").unwrap()),
            Lookup::Bound(EmacsCommand::OtherWindow)
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn text_command_overrides_land_in_text_keymap() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("C-j".to_string(), "next-line".to_string());
        let (keymaps, warnings) = build_keymaps(&overrides);
        assert!(warnings.is_empty());
        assert_eq!(
            keymaps.text.lookup(&parse_key_seq("C-j").unwrap()),
            Lookup::Bound(EmacsCommand::NextLine)
        );
        assert_eq!(
            keymaps.global.lookup(&parse_key_seq("C-j").unwrap()),
            Lookup::Unbound
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::commands`
Expected: compile error — `EmacsCommand` not found (also add `pub mod commands;` to `src/emacs/mod.rs` after `pub mod keymap;` or the module won't compile at all)

- [ ] **Step 3: Implement the command table.** Fill `src/emacs/commands.rs` above the tests:

```rust
//! The command table is the spine of the layer: every capability is a
//! named command. Keymaps bind chords to commands, M-x (Phase 3) invokes
//! them by name, and prefix args (Phase 4) are passed to every command via
//! `App::execute_emacs_command(cmd, prefix)`.

use std::collections::HashMap;

use super::keymap::{parse_key_seq, Keymap};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmacsCommand {
    // Phase 0: management layer
    SplitWindowBelow,
    SplitWindowRight,
    OtherWindow,
    DeleteWindow,
    DeleteOtherWindows,
    SwitchToBuffer,
    NewTab,
    NextTab,
    PreviousTab,
    KillTab,
    WorkspacePicker,
    QuotedInsert,
    KeyboardQuit,
    // Phase 1: TEXT mode
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
    // Phase 1: kill ring
    KillRingSave,
    KillRegion,
    Yank,
    YankPop,
}

/// Canonical name table (M-x namespace). Keep sorted by name.
pub const COMMAND_NAMES: &[(EmacsCommand, &str)] = &[
    (EmacsCommand::BackwardChar, "backward-char"),
    (EmacsCommand::BackwardWord, "backward-word"),
    (EmacsCommand::BeginningOfBuffer, "beginning-of-buffer"),
    (EmacsCommand::DeleteOtherWindows, "delete-other-windows"),
    (EmacsCommand::DeleteWindow, "delete-window"),
    (EmacsCommand::EndOfBuffer, "end-of-buffer"),
    (EmacsCommand::ExchangePointAndMark, "exchange-point-and-mark"),
    (EmacsCommand::ExitTextMode, "exit-text-mode"),
    (EmacsCommand::ForwardChar, "forward-char"),
    (EmacsCommand::ForwardWord, "forward-word"),
    (EmacsCommand::GotoLine, "goto-line"),
    (EmacsCommand::KeyboardQuit, "keyboard-quit"),
    (EmacsCommand::KillRegion, "kill-region"),
    (EmacsCommand::KillRingSave, "kill-ring-save"),
    (EmacsCommand::KillTab, "kill-tab"),
    (EmacsCommand::MoveBeginningOfLine, "move-beginning-of-line"),
    (EmacsCommand::MoveEndOfLine, "move-end-of-line"),
    (EmacsCommand::NewTab, "new-tab"),
    (EmacsCommand::NextLine, "next-line"),
    (EmacsCommand::NextTab, "next-tab"),
    (EmacsCommand::OtherWindow, "other-window"),
    (EmacsCommand::PreviousLine, "previous-line"),
    (EmacsCommand::PreviousTab, "previous-tab"),
    (EmacsCommand::QuotedInsert, "quoted-insert"),
    (EmacsCommand::ScrollDown, "scroll-down"),
    (EmacsCommand::ScrollUp, "scroll-up"),
    (EmacsCommand::SetMark, "set-mark-command"),
    (EmacsCommand::SplitWindowBelow, "split-window-below"),
    (EmacsCommand::SplitWindowRight, "split-window-right"),
    (EmacsCommand::SwitchToBuffer, "switch-to-buffer"),
    (EmacsCommand::TextMode, "text-mode"),
    (EmacsCommand::WorkspacePicker, "workspace-picker"),
    (EmacsCommand::Yank, "yank"),
    (EmacsCommand::YankPop, "yank-pop"),
];

impl EmacsCommand {
    pub fn name(self) -> &'static str {
        COMMAND_NAMES
            .iter()
            .find(|(cmd, _)| *cmd == self)
            .map(|(_, name)| *name)
            .expect("every command has a name")
    }

    pub fn from_name(name: &str) -> Option<Self> {
        COMMAND_NAMES
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(cmd, _)| *cmd)
    }

    /// TEXT-mode-only commands live in the text keymap; everything else in
    /// the global (live-mode) keymap. Yank/YankPop exist in both worlds.
    fn is_text_command(self) -> bool {
        matches!(
            self,
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
                | Self::KillRegion
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct KeymapSet {
    /// Consulted in live mode (pane focused, TEXT mode off).
    pub global: Keymap<EmacsCommand>,
    /// Consulted while TEXT mode is active.
    pub text: Keymap<EmacsCommand>,
}

const DEFAULT_GLOBAL_BINDINGS: &[(&str, EmacsCommand)] = &[
    ("C-x 2", EmacsCommand::SplitWindowBelow),
    ("C-x 3", EmacsCommand::SplitWindowRight),
    ("C-x o", EmacsCommand::OtherWindow),
    ("C-x 0", EmacsCommand::DeleteWindow),
    ("C-x 1", EmacsCommand::DeleteOtherWindows),
    ("C-x b", EmacsCommand::SwitchToBuffer),
    ("C-x c", EmacsCommand::NewTab),
    ("C-x n", EmacsCommand::NextTab),
    ("C-x p", EmacsCommand::PreviousTab),
    ("C-x k", EmacsCommand::KillTab),
    ("C-x w", EmacsCommand::WorkspacePicker),
    ("C-x [", EmacsCommand::TextMode),
    ("C-q", EmacsCommand::QuotedInsert),
    ("C-y", EmacsCommand::Yank),
    ("M-y", EmacsCommand::YankPop),
];

const DEFAULT_TEXT_BINDINGS: &[(&str, EmacsCommand)] = &[
    ("C-f", EmacsCommand::ForwardChar),
    ("C-b", EmacsCommand::BackwardChar),
    ("C-n", EmacsCommand::NextLine),
    ("C-p", EmacsCommand::PreviousLine),
    ("M-f", EmacsCommand::ForwardWord),
    ("M-b", EmacsCommand::BackwardWord),
    ("C-a", EmacsCommand::MoveBeginningOfLine),
    ("C-e", EmacsCommand::MoveEndOfLine),
    ("C-v", EmacsCommand::ScrollUp),
    ("M-v", EmacsCommand::ScrollDown),
    ("M-<", EmacsCommand::BeginningOfBuffer),
    ("M->", EmacsCommand::EndOfBuffer),
    ("M-g g", EmacsCommand::GotoLine),
    ("C-SPC", EmacsCommand::SetMark),
    ("C-x C-x", EmacsCommand::ExchangePointAndMark),
    ("M-w", EmacsCommand::KillRingSave),
    ("C-w", EmacsCommand::KillRegion),
    ("C-y", EmacsCommand::Yank),
    ("M-y", EmacsCommand::YankPop),
    ("C-g", EmacsCommand::KeyboardQuit),
    ("q", EmacsCommand::ExitTextMode),
    ("ESC", EmacsCommand::ExitTextMode),
];

/// Build the default keymaps and apply `[emacs.keys]` overrides.
/// Returns (keymaps, warnings) — invalid chord strings or unknown command
/// names become warnings, never hard errors.
pub fn build_keymaps(overrides: &HashMap<String, String>) -> (KeymapSet, Vec<String>) {
    let mut set = KeymapSet::default();
    for (seq, cmd) in DEFAULT_GLOBAL_BINDINGS {
        set.global
            .bind(parse_key_seq(seq).expect("default global binding parses"), *cmd);
    }
    for (seq, cmd) in DEFAULT_TEXT_BINDINGS {
        set.text
            .bind(parse_key_seq(seq).expect("default text binding parses"), *cmd);
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
        if cmd.is_text_command() {
            set.text.bind(seq, cmd);
        } else if matches!(cmd, EmacsCommand::Yank | EmacsCommand::YankPop) {
            set.global.bind(seq.clone(), cmd);
            set.text.bind(seq, cmd);
        } else {
            set.global.bind(seq, cmd);
        }
    }
    (set, warnings)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::commands`
Expected: `test result: ok. 5 passed`

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/emacs
git commit -m "feat: emacs command table with default keymaps and config overrides"
```

### Task 4: Input-interception seam, `EmacsState`, C-x chords, C-q

**Files:**
- Modify: `src/emacs/mod.rs` (add `EmacsState`)
- Create: `src/app/input/emacs.rs`
- Modify: `src/app/input/mod.rs` (add `mod emacs;` to the submodule declarations, alphabetically next to `mod copy_mode;`)
- Modify: `src/app/state.rs` (`AppState` field near :1487; `test_new()` literal near :1835)
- Modify: `src/app/mod.rs` (`App::new` state literal at :660; hook in `route_client_events` Key arm at :1557; `apply_live_config` at :1332)
- Test: inline `#[cfg(test)]` tests in `src/app/input/emacs.rs`

**Interfaces:**
- Consumes: `KeymapSet`/`build_keymaps` (Task 3), `Chord`/`Lookup`/`format_seq` (Task 2), `EmacsConfig` (Task 1); upstream: `App::execute_tui_navigate_action(NavigateAction, ActionContext)` (`src/app/input/navigate.rs:179`), `NavigateAction` variants `SplitHorizontal` (= stacked = Emacs split-below), `SplitVertical` (= side-by-side = Emacs split-right), `CyclePaneNext`, `ClosePane`, `Zoom`, `OpenNavigator`, `NewTab`, `NextTab`, `PreviousTab`, `CloseTab`, `WorkspacePicker` (`navigate.rs:1280`), `TerminalRuntime::encode_terminal_key(TerminalKey) -> Vec<u8>` and `try_send_bytes(bytes::Bytes)` (`src/terminal/runtime.rs`), `AppState::runtime_for_pane_in_workspace(&'a self, &'a TerminalRuntimeRegistry, usize, PaneId) -> Option<&'a TerminalRuntime>` (`src/app/state.rs:1583` — note the returned borrow ties to `&self`; scope it before mutating `self.state`).
- Produces: `EmacsState` on `app.state.emacs`; `App::emacs_intercept_key(&mut self, TerminalKey) -> bool`; `App::execute_emacs_command(&mut self, EmacsCommand, Option<i64>)` — Tasks 7–12 extend the command match; test fixture `emacs_app_with_channel(bytes) -> (App, PaneId, Receiver<Bytes>)` reused by later tasks.

- [ ] **Step 1: Add `EmacsState` to `src/emacs/mod.rs`** (below the `pub mod` lines):

```rust
pub mod commands;
pub mod keymap;

use crate::config::EmacsConfig;
use commands::KeymapSet;
use keymap::Chord;

/// All Emacs-layer state. Lives on `AppState` (pure data, no channels —
/// matching AppState's own contract).
#[derive(Debug)]
pub struct EmacsState {
    pub enabled: bool,
    pub clipboard_sync: bool,
    pub kill_ring_max: usize,
    pub mark_ring_max: usize,
    pub keymaps: KeymapSet,
    /// Chords accumulated toward a multi-chord binding (after `C-x`, ...).
    pub pending: Vec<Chord>,
    /// True after `C-q`: the next key is sent literally to the pane.
    pub quoted_insert: bool,
    /// Echo-area message; cleared at the start of the next handled key.
    pub echo: Option<String>,
}

impl EmacsState {
    pub fn from_config(config: &EmacsConfig) -> Self {
        let (keymaps, warnings) = commands::build_keymaps(&config.keys);
        for warning in &warnings {
            tracing::warn!("{warning}");
        }
        Self {
            enabled: config.enabled,
            clipboard_sync: config.clipboard_sync,
            kill_ring_max: config.kill_ring_max.max(1),
            mark_ring_max: config.mark_ring_max.max(1),
            keymaps,
            pending: Vec::new(),
            quoted_insert: false,
            echo: None,
        }
    }

    /// Live config reload: refresh config-derived fields, preserve runtime
    /// state (rings survive a reload); drop transient state when disabling.
    pub fn apply_config(&mut self, config: &EmacsConfig) {
        let (keymaps, warnings) = commands::build_keymaps(&config.keys);
        for warning in &warnings {
            tracing::warn!("{warning}");
        }
        self.enabled = config.enabled;
        self.clipboard_sync = config.clipboard_sync;
        self.kill_ring_max = config.kill_ring_max.max(1);
        self.mark_ring_max = config.mark_ring_max.max(1);
        self.keymaps = keymaps;
        if !self.enabled {
            self.pending.clear();
            self.quoted_insert = false;
            self.echo = None;
        }
    }
}
```

- [ ] **Step 2: Add the `AppState` field.** In `src/app/state.rs`, inside `pub struct AppState`, directly above `pub(crate) terminal_runtime_shutdowns: ...`:

```rust
    /// Emacs layer seam (fork): all Emacs-layer state.
    pub emacs: crate::emacs::EmacsState,
```

In `AppState::test_new()`, directly above `terminal_runtime_shutdowns: Vec::new(),`:

```rust
            emacs: crate::emacs::EmacsState::from_config(&crate::config::EmacsConfig::default()),
```

In `src/app/mod.rs` `App::new`, in the `AppState` literal directly above `terminal_runtime_shutdowns: Vec::new(),`:

```rust
            emacs: crate::emacs::EmacsState::from_config(&config.emacs),
```

In `App::apply_live_config` (`src/app/mod.rs:1321`), after the closing brace of the `if !invalid_section("keys") { ... }` block and before `if !invalid_section("ui") {`:

```rust
        // Emacs layer seam (fork).
        if !invalid_section("emacs") {
            self.state.emacs.apply_config(&config.emacs);
        }
```

- [ ] **Step 3: Verify it builds before wiring the hook**

Run: `cargo build --locked 2>&1 | tail -3`
Expected: `Finished \`dev\` profile` (if other `AppState { ... }` literals fail to compile, add the same `emacs:` init line there — the two literals above are the only ones as of upstream `3a8490f`)

- [ ] **Step 4: Write the failing interception tests.** Create `src/app/input/emacs.rs` containing only the test module for now (the `impl App` block comes in Step 6), and register it: in `src/app/input/mod.rs` add `mod emacs;` alphabetically among the existing `mod` declarations (next to `mod copy_mode;`).

```rust
#[cfg(test)]
mod tests {
    use super::super::app_for_mouse_test;
    use crate::app::{App, Mode};
    use crate::workspace::Workspace;

    /// App with the Emacs layer enabled and one focused pane whose PTY
    /// input is observable through the returned channel.
    /// `clipboard_sync` is off so tests never shell out to wl-paste.
    pub(crate) fn emacs_app_with_channel(
        bytes: &[u8],
    ) -> (
        App,
        crate::layout::PaneId,
        tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) {
        let mut app = app_for_mouse_test();
        app.state.emacs = crate::emacs::EmacsState::from_config(&crate::config::EmacsConfig {
            enabled: true,
            clipboard_sync: false,
            ..Default::default()
        });
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 40, 10));
        let info = pane_infos[0].clone();
        let (rt, rx) = crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            info.inner_rect.width,
            info.inner_rect.height,
            16 * 1024,
            bytes,
            8,
        );
        ws.tabs[0].runtimes.insert(pane_id, rt);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        (app, pane_id, rx)
    }

    fn sent_bytes(rx: &mut tokio::sync::mpsc::Receiver<bytes::Bytes>) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            out.extend_from_slice(&chunk);
        }
        out
    }

    #[tokio::test]
    async fn c_x_b_opens_the_navigator() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x18]); // C-x
        assert_eq!(app.state.emacs.pending.len(), 1);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-x-"));
        app.route_client_input(vec![b'b']);
        assert!(app.state.emacs.pending.is_empty());
        assert_eq!(app.state.mode, Mode::Navigator);
        assert!(sent_bytes(&mut rx).is_empty(), "chord must not reach the pane");
    }

    #[tokio::test]
    async fn plain_keys_pass_through_to_the_pane() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(b"a".to_vec());
        assert_eq!(sent_bytes(&mut rx), b"a".to_vec());
    }

    #[tokio::test]
    async fn quoted_insert_sends_raw_c_x() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x11]); // C-q
        assert!(app.state.emacs.quoted_insert);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-q-"));
        app.route_client_input(vec![0x18]); // C-x, sent literally
        assert!(!app.state.emacs.quoted_insert);
        assert_eq!(sent_bytes(&mut rx), vec![0x18]);
        assert!(app.state.emacs.pending.is_empty());
    }

    #[tokio::test]
    async fn unbound_chord_reports_undefined_and_is_swallowed() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x18, b'z']);
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("C-x z is undefined")
        );
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn c_g_cancels_a_pending_chord() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x18, 0x07]); // C-x C-g
        assert!(app.state.emacs.pending.is_empty());
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Quit"));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn disabled_layer_is_bit_for_bit_passthrough() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.state.emacs.apply_config(&crate::config::EmacsConfig::default());
        assert!(!app.state.emacs.enabled);
        app.route_client_input(vec![0x18]); // C-x is not herdr's prefix (C-b is)
        assert_eq!(sent_bytes(&mut rx), vec![0x18]);
        assert!(app.state.emacs.pending.is_empty());
    }

    #[tokio::test]
    async fn layer_leaves_non_terminal_modes_alone() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.state.mode = Mode::Navigate;
        app.route_client_input(vec![0x18]);
        assert!(app.state.emacs.pending.is_empty(), "no chord started");
    }
}
```

- [ ] **Step 5: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: FAILURES — `c_x_b_opens_the_navigator` etc. fail (mode stays `Terminal`, `pending` stays empty) because no hook exists yet. (`plain_keys_pass_through` and `disabled_layer...` may already pass — that's fine.)

- [ ] **Step 6: Implement the adapter.** Add above the tests in `src/app/input/emacs.rs`:

```rust
//! Glue between the pure Emacs engine (`crate::emacs`) and herdr's `App`.
//!
//! Emacs layer seam (fork): this file is new code registered with a single
//! `mod emacs;` line. It lives under `src/app/input/` because executing
//! commands needs `pub(super)` App internals (`execute_tui_navigate_action`,
//! `set_pane_scroll_offset`, ...).

use crossterm::event::KeyEventKind;

use super::navigate::{ActionContext, NavigateAction};
use crate::app::state::Mode;
use crate::app::App;
use crate::emacs::commands::EmacsCommand;
use crate::emacs::keymap::{format_seq, Chord, Lookup};
use crate::input::TerminalKey;

impl App {
    /// Emacs-layer interception hook, called before any herdr key
    /// dispatch in `route_client_events`. Returns true when the layer
    /// consumed the key.
    pub(crate) fn emacs_intercept_key(&mut self, key: TerminalKey) -> bool {
        if !self.state.emacs.enabled {
            return false;
        }
        // The layer only owns dispatch while a pane has focus; herdr's own
        // overlays (prefix, navigate, dialogs, copy mode) keep their keys.
        if self.state.mode != Mode::Terminal {
            return false;
        }
        match key.kind {
            KeyEventKind::Press => {}
            // Mirror the press decision for kitty repeat/release reports so
            // events for layer-owned keys never leak into the pane.
            KeyEventKind::Repeat | KeyEventKind::Release => {
                return self.emacs_would_consume(key);
            }
        }

        self.state.emacs.echo = None;

        if self.state.emacs.quoted_insert {
            self.state.emacs.quoted_insert = false;
            self.emacs_send_key_to_focused_pane(key);
            return true;
        }

        let Some(chord) = Chord::from_key(&key) else {
            return false;
        };

        // C-g always cancels an in-flight chord.
        if !self.state.emacs.pending.is_empty() && chord == Chord::ctrl('g') {
            self.state.emacs.pending.clear();
            self.state.emacs.echo = Some("Quit".to_string());
            return true;
        }

        let mut seq = self.state.emacs.pending.clone();
        seq.push(chord);
        match self.state.emacs.keymaps.global.lookup(&seq) {
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
                if self.state.emacs.pending.is_empty() {
                    // Plain unbound key: flows to the pane untouched.
                    false
                } else {
                    self.state.emacs.pending.clear();
                    self.state.emacs.echo =
                        Some(format!("{} is undefined", format_seq(&seq)));
                    true
                }
            }
        }
    }

    /// Press-equivalent consume decision, used for repeat/release events.
    fn emacs_would_consume(&self, key: TerminalKey) -> bool {
        let emacs = &self.state.emacs;
        if emacs.quoted_insert || !emacs.pending.is_empty() {
            return true;
        }
        match Chord::from_key(&key) {
            Some(chord) => {
                !matches!(emacs.keymaps.global.lookup(&[chord]), Lookup::Unbound)
            }
            None => false,
        }
    }

    /// Execute a named command. `prefix` is the universal-argument slot:
    /// always `None` until Phase 4 wires `C-u`, but part of the calling
    /// convention from day one (spec: "the command table is the spine").
    pub(crate) fn execute_emacs_command(&mut self, cmd: EmacsCommand, prefix: Option<i64>) {
        let _ = prefix; // consumed by motions and C-u C-SPC in Phase 4
        match cmd {
            EmacsCommand::SplitWindowBelow => {
                self.emacs_navigate(NavigateAction::SplitHorizontal)
            }
            EmacsCommand::SplitWindowRight => {
                self.emacs_navigate(NavigateAction::SplitVertical)
            }
            EmacsCommand::OtherWindow => self.emacs_navigate(NavigateAction::CyclePaneNext),
            EmacsCommand::DeleteWindow => self.emacs_navigate(NavigateAction::ClosePane),
            EmacsCommand::DeleteOtherWindows => self.emacs_navigate(NavigateAction::Zoom),
            EmacsCommand::SwitchToBuffer => self.emacs_navigate(NavigateAction::OpenNavigator),
            EmacsCommand::NewTab => self.emacs_navigate(NavigateAction::NewTab),
            EmacsCommand::NextTab => self.emacs_navigate(NavigateAction::NextTab),
            EmacsCommand::PreviousTab => self.emacs_navigate(NavigateAction::PreviousTab),
            EmacsCommand::KillTab => self.emacs_navigate(NavigateAction::CloseTab),
            EmacsCommand::WorkspacePicker => {
                self.emacs_navigate(NavigateAction::WorkspacePicker)
            }
            EmacsCommand::QuotedInsert => {
                self.state.emacs.quoted_insert = true;
                self.state.emacs.echo = Some("C-q-".to_string());
            }
            EmacsCommand::KeyboardQuit => {
                self.state.emacs.pending.clear();
                self.state.emacs.echo = Some("Quit".to_string());
            }
            // Implemented by later tasks of this plan (TEXT mode, rings):
            EmacsCommand::TextMode
            | EmacsCommand::ExitTextMode
            | EmacsCommand::ForwardChar
            | EmacsCommand::BackwardChar
            | EmacsCommand::NextLine
            | EmacsCommand::PreviousLine
            | EmacsCommand::ForwardWord
            | EmacsCommand::BackwardWord
            | EmacsCommand::MoveBeginningOfLine
            | EmacsCommand::MoveEndOfLine
            | EmacsCommand::ScrollUp
            | EmacsCommand::ScrollDown
            | EmacsCommand::BeginningOfBuffer
            | EmacsCommand::EndOfBuffer
            | EmacsCommand::GotoLine
            | EmacsCommand::SetMark
            | EmacsCommand::ExchangePointAndMark
            | EmacsCommand::KillRingSave
            | EmacsCommand::KillRegion
            | EmacsCommand::Yank
            | EmacsCommand::YankPop => {}
        }
    }

    fn emacs_navigate(&mut self, action: NavigateAction) {
        self.execute_tui_navigate_action(action, ActionContext::Prefix);
    }

    fn emacs_send_key_to_focused_pane(&mut self, key: TerminalKey) {
        let Some((ws_idx, pane_id)) = self.emacs_focused_pane() else {
            return;
        };
        let Some(rt) = self.state.runtime_for_pane_in_workspace(
            &self.terminal_runtimes,
            ws_idx,
            pane_id,
        ) else {
            return;
        };
        let bytes = rt.encode_terminal_key(key);
        if !bytes.is_empty() {
            let _ = rt.try_send_bytes(bytes::Bytes::from(bytes));
        }
    }

    fn emacs_focused_pane(&self) -> Option<(usize, crate::layout::PaneId)> {
        let ws_idx = self.state.active?;
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        Some((ws_idx, pane_id))
    }
}
```

- [ ] **Step 7: Wire the hook.** In `src/app/mod.rs` `route_client_events` (:1557), at the very top of the `RawInputEvent::Key(key)` arm, before `let key_id = repeat_key_identity(&key);`:

```rust
                crate::raw_input::RawInputEvent::Key(key) => {
                    // Emacs layer seam (fork): the layer may consume the key
                    // before any herdr mode dispatch or keybind matching.
                    if self.emacs_intercept_key(key) {
                        self.sync_prefix_input_source(previous_mode);
                        continue;
                    }
                    let key_id = repeat_key_identity(&key);
```

(Only the `if ... { continue; }` block and its comment are new.)

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `test result: ok. 7 passed`

- [ ] **Step 9: Regression check on upstream key handling**

Run: `cargo test --locked --bin herdr app::`
Expected: `test result: ok.` (several hundred tests; the disabled layer must not disturb any of them)

- [ ] **Step 10: Commit**

```bash
cargo fmt
git add src/emacs/mod.rs src/app/input/emacs.rs src/app/input/mod.rs \
  src/app/state.rs src/app/mod.rs
git commit -m "feat: emacs input-interception seam with C-x chords and C-q quoted-insert"
```

---

### Task 5: Fork guards — disable self-update and background version checks

**Files:**
- Modify: `src/app/mod.rs` (`auto_updates_enabled` at :204)
- Modify: `src/update.rs` (`self_update` at :1946)
- Test: inline test in `src/app/mod.rs` `mod tests`

**Interfaces:**
- Consumes: `background_update_check_enabled(no_session, check_enabled)` (`src/app/mod.rs:208`) — gates both the version-check and agent-manifest background threads in `App::new`.
- Produces: a build that never mutates its own binary. `herdr update` prints the fork message (`main.rs` already special-cases `Err` strings starting with `"self-update is disabled"`).

- [ ] **Step 1: Write the failing test** — in `src/app/mod.rs` `mod tests` (near `test_app()` at :1752):

```rust
    #[test]
    fn fork_guard_disables_background_update_checks() {
        // Fork guard: this build must never self-update or phone home.
        // (Debug builds were already off; this pins the invariant for all
        // profiles — run with --release to prove the release path.)
        assert!(!background_update_check_enabled(false, true));
        assert!(!background_update_check_enabled(true, true));
    }
```

- [ ] **Step 2: Run test — it passes in debug (documenting why), then make the real change.** Debug builds already return false via `cfg!(debug_assertions)`, so this test alone cannot fail in the dev profile. **The guard is therefore verified by (a) this test under `--release`, and (b) code inspection.** This is the explicitly-acknowledged non-TDD step of this plan.

Replace `auto_updates_enabled` in `src/app/mod.rs`:

```rust
fn auto_updates_enabled(_no_session: bool) -> bool {
    // Fork guard (emacs branch): this build must never replace itself or
    // run background version/manifest checks; updates come from rebasing
    // onto upstream. Upstream logic: `!no_session && !cfg!(debug_assertions)`.
    false
}
```

- [ ] **Step 3: Guard `self_update`.** In `src/update.rs`, insert at the very top of `pub fn self_update(options: SelfUpdateOptions) -> Result<Version, String>` (:1946), before `let channel = ...`:

```rust
    // Fork guard (emacs branch): never overwrite this binary. The
    // `cfg!(test)` escape keeps upstream's own self-update unit tests
    // exercising the real logic.
    if !cfg!(test) {
        return Err(
            "self-update is disabled in this fork; rebase the emacs branch onto upstream/master instead"
                .into(),
        );
    }
```

- [ ] **Step 4: Verify**

Run: `cargo test --locked --bin herdr fork_guard`
Expected: `test result: ok. 1 passed`

Run: `cargo test --locked --bin herdr update::`
Expected: `test result: ok.` (upstream update tests still pass thanks to the `cfg!(test)` escape)

Run: `cargo test --locked --release --bin herdr fork_guard_disables_background_update_checks`
Expected: `test result: ok. 1 passed` (release profile — this is the profile where the guard actually changes behavior; expect a long compile the first time)

Manual smoke: `cargo run --locked -- update; echo "exit=$?"`
Expected output contains: `self-update is disabled in this fork; rebase the emacs branch onto upstream/master instead` and a non-zero exit.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/app/mod.rs src/update.rs
git commit -m "feat: fork guards - disable self-update and background version checks"
```

### Task 6: Scrollback read seam + pure motion engine

**Files:**
- Modify: `src/pane/terminal.rs` (new methods in `impl GhosttyPaneTerminal` at :878 block, next to `extract_selection` at :1708; and delegates in `impl PaneTerminal` at :178 block, next to `extract_selection` at :404)
- Modify: `src/pane.rs` (delegates in `impl PaneRuntime` at :1412 block, next to `extract_selection` at :2489)
- Modify: `src/terminal/runtime.rs` (delegates next to `extract_selection`)
- Create: `src/emacs/text_mode.rs`
- Modify: `src/emacs/mod.rs` (add `pub mod text_mode;`)
- Test: inline tests in `src/emacs/text_mode.rs` (pure, fake buffer) and in `src/terminal/runtime.rs`'s existing `#[cfg(test)] mod tests` (real ghostty terminal)

**Interfaces:**
- Consumes: `ghostty::Terminal::total_rows() -> Result<usize, Error>` (`src/ghostty/mod.rs:752`), `cols() -> Result<u16, Error>` (:1070), `read_text_screen(&self, start: (u16, u32), end: (u16, u32), rectangle: bool) -> Result<String, Error>` (:882, **both endpoints inclusive**, coordinates are `(col, row)` with absolute scrollback rows), file-local `ghostty_screen_row(&Terminal, cols, y) -> Result<String, Error>` (`src/pane/terminal.rs:2392`).
- Produces (used by Tasks 7–10):
  - `TerminalRuntime::text_dims(&self) -> Option<(usize, u16)>` — (total rows, grid cols)
  - `TerminalRuntime::text_row(&self, row: u32) -> Option<String>` — one absolute row, trailing whitespace trimmed
  - `TerminalRuntime::read_text_range(&self, start: (u16, u32), end: (u16, u32)) -> Option<String>` — inclusive endpoints, `(col, row)`
  - `emacs::text_mode::{Pos, TextBuffer, clamp, forward_char, backward_char, next_line, previous_line, forward_word, backward_word, line_beginning, line_end, buffer_end}`

- [ ] **Step 1: Write the failing runtime-accessor test.** `src/terminal/runtime.rs` has no `mod tests` yet (it ends with a `#[cfg(test)] impl TerminalRuntime` block of test constructors at :454) — append a new module at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emacs_text_accessors_read_scrollback() {
        let rt = TerminalRuntime::test_with_scrollback_bytes(
            10,
            3,
            16 * 1024,
            b"alpha\r\nbravo six\r\ncharlie\r\ndelta\r\n",
        );
        let (total, cols) = rt.text_dims().expect("dims");
        assert_eq!(cols, 10);
        assert!(total >= 4, "screen + scrollback rows, got {total}");
        assert_eq!(rt.text_row(0).as_deref(), Some("alpha"));
        assert_eq!(rt.text_row(1).as_deref(), Some("bravo six"));
        assert_eq!(rt.text_row(u32::MAX), None);
        // Inclusive endpoints, (col, row) coordinates:
        assert_eq!(
            rt.read_text_range((0, 0), (4, 0)).as_deref(),
            Some("alpha")
        );
        let two_lines = rt.read_text_range((2, 0), (2, 1)).expect("range");
        assert!(two_lines.contains("pha"), "{two_lines:?}");
        assert!(two_lines.contains("bra"), "{two_lines:?}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --locked --bin herdr emacs_text_accessors`
Expected: compile error — `text_dims` not found

- [ ] **Step 3: Implement the four delegation layers.**

`src/pane/terminal.rs`, in `impl GhosttyPaneTerminal` directly after `extract_selection` (:1713):

```rust
    /// Emacs layer seam (fork): read-side scrollback access.
    /// (total rows including scrollback, grid columns).
    pub fn text_dims(&self) -> Option<(usize, u16)> {
        self.core.lock().ok().and_then(|core| {
            let total = core.terminal.total_rows().ok()?;
            let cols = core.terminal.cols().ok()?;
            Some((total, cols))
        })
    }

    /// Emacs layer seam (fork): one absolute row as plain text, trailing
    /// whitespace trimmed. `None` when the row is out of range.
    pub fn text_row(&self, row: u32) -> Option<String> {
        self.core.lock().ok().and_then(|core| {
            let total = core.terminal.total_rows().ok()?;
            if u64::from(row) >= total as u64 {
                return None;
            }
            let cols = core.terminal.cols().ok()?;
            ghostty_screen_row(&core.terminal, cols, row)
                .ok()
                .map(|line| line.trim_end().to_string())
        })
    }

    /// Emacs layer seam (fork): plain text between two inclusive
    /// `(col, row)` endpoints (absolute scrollback rows).
    pub fn read_text_range(&self, start: (u16, u32), end: (u16, u32)) -> Option<String> {
        self.core
            .lock()
            .ok()
            .and_then(|core| core.terminal.read_text_screen(start, end, false).ok())
    }
```

`src/pane/terminal.rs`, in `impl PaneTerminal` directly after its `extract_selection` (:407):

```rust
    pub fn text_dims(&self) -> Option<(usize, u16)> {
        self.ghostty.text_dims()
    }

    pub fn text_row(&self, row: u32) -> Option<String> {
        self.ghostty.text_row(row)
    }

    pub fn read_text_range(&self, start: (u16, u32), end: (u16, u32)) -> Option<String> {
        self.ghostty.read_text_range(start, end)
    }
```

`src/pane.rs`, in `impl PaneRuntime` directly after its `extract_selection` (:2492):

```rust
    pub fn text_dims(&self) -> Option<(usize, u16)> {
        self.terminal.text_dims()
    }

    pub fn text_row(&self, row: u32) -> Option<String> {
        self.terminal.text_row(row)
    }

    pub fn read_text_range(&self, start: (u16, u32), end: (u16, u32)) -> Option<String> {
        self.terminal.read_text_range(start, end)
    }
```

`src/terminal/runtime.rs`, in `impl TerminalRuntime` directly after its `extract_selection`:

```rust
    pub fn text_dims(&self) -> Option<(usize, u16)> {
        self.0.text_dims()
    }

    pub fn text_row(&self, row: u32) -> Option<String> {
        self.0.text_row(row)
    }

    pub fn read_text_range(&self, start: (u16, u32), end: (u16, u32)) -> Option<String> {
        self.0.read_text_range(start, end)
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --locked --bin herdr emacs_text_accessors`
Expected: `test result: ok. 1 passed`. If the `read_text_range` assertions fail on exact content, print the actual values, adjust the assertions to the observed inclusive-endpoint behavior, and record the semantics in the doc comment — the point of this test is to pin ghostty's endpoint semantics before Task 10 builds region extraction on them.

- [ ] **Step 5: Write the failing motion tests.** Create `src/emacs/text_mode.rs` (and add `pub mod text_mode;` to `src/emacs/mod.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 0: "alpha one"
    /// 1: ""
    /// 2: "  bravo_2 charlie"
    /// 3: "delta"
    fn buf() -> FakeBuffer {
        FakeBuffer(vec![
            "alpha one".into(),
            "".into(),
            "  bravo_2 charlie".into(),
            "delta".into(),
        ])
    }

    struct FakeBuffer(Vec<String>);

    impl TextBuffer for FakeBuffer {
        fn total_rows(&self) -> u32 {
            self.0.len() as u32
        }
        fn line(&self, row: u32) -> String {
            self.0.get(row as usize).cloned().unwrap_or_default()
        }
    }

    fn p(row: u32, col: u16) -> Pos {
        Pos { row, col }
    }

    #[test]
    fn char_motions_wrap_lines_and_stop_at_buffer_ends() {
        let b = buf();
        assert_eq!(forward_char(&b, p(0, 0)), p(0, 1));
        // point may sit at end-of-line (col == line length)
        assert_eq!(forward_char(&b, p(0, 8)), p(0, 9));
        assert_eq!(forward_char(&b, p(0, 9)), p(1, 0));
        assert_eq!(forward_char(&b, p(1, 0)), p(2, 0));
        assert_eq!(forward_char(&b, p(3, 5)), p(3, 5), "buffer end sticks");
        assert_eq!(backward_char(&b, p(0, 0)), p(0, 0), "buffer start sticks");
        assert_eq!(backward_char(&b, p(1, 0)), p(0, 9));
        assert_eq!(backward_char(&b, p(2, 1)), p(2, 0));
    }

    #[test]
    fn line_motions_clamp_to_line_length() {
        let b = buf();
        assert_eq!(next_line(&b, p(0, 7)), p(1, 0), "clamped to empty line");
        assert_eq!(next_line(&b, p(2, 17)), p(3, 5));
        assert_eq!(next_line(&b, p(3, 2)), p(3, 2), "last line sticks");
        assert_eq!(previous_line(&b, p(2, 10)), p(1, 0));
        assert_eq!(previous_line(&b, p(0, 4)), p(0, 4), "first line sticks");
        assert_eq!(line_beginning(p(2, 9)), p(2, 0));
        assert_eq!(line_end(&b, p(2, 0)), p(2, 17));
    }

    #[test]
    fn word_motions_cross_lines() {
        let b = buf();
        // M-f from start of "alpha one" -> end of "alpha"
        assert_eq!(forward_word(&b, p(0, 0)), p(0, 5));
        assert_eq!(forward_word(&b, p(0, 5)), p(0, 9)); // end of "one"
        // crossing the empty line to "bravo_2" ('_' is a word char)
        assert_eq!(forward_word(&b, p(0, 9)), p(2, 9));
        assert_eq!(backward_word(&b, p(2, 9)), p(2, 2));
        assert_eq!(backward_word(&b, p(2, 2)), p(0, 6)); // back to start of "one"
        assert_eq!(forward_word(&b, p(3, 5)), p(3, 5), "buffer end sticks");
    }

    #[test]
    fn buffer_end_and_clamp() {
        let b = buf();
        assert_eq!(buffer_end(&b), p(3, 5));
        assert_eq!(clamp(&b, p(99, 99)), p(3, 5));
        assert_eq!(clamp(&b, p(2, 99)), p(2, 17));
        assert_eq!(clamp(&b, p(0, 3)), p(0, 3));
    }
}
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::text_mode`
Expected: compile error — `Pos`, `TextBuffer`, motion fns not found

- [ ] **Step 7: Implement the motion engine.** Above the tests in `src/emacs/text_mode.rs`:

```rust
//! TEXT mode: point/mark over a frozen, read-only view of pane scrollback.
//!
//! All coordinates are absolute scrollback positions: `row` counts from the
//! top of the scrollback (same addressing as `Selection::line_range` and
//! ghostty's `read_text_screen`), `col` is a character index in the row.
//! Known limitation (accepted): one char == one grid column, so wide/CJK
//! cells drift — the same class of limitation as upstream copy-mode math.

/// Absolute position in a pane's scrollback grid. Ordered row-major, so
/// `Ord` gives region ordering for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub row: u32,
    pub col: u16,
}

/// Read-only view of the frozen buffer. Implemented over a real
/// `TerminalRuntime` by the App adapter and over `Vec<String>` in tests.
pub trait TextBuffer {
    fn total_rows(&self) -> u32;
    /// Row content with trailing whitespace trimmed; empty for
    /// out-of-range rows.
    fn line(&self, row: u32) -> String;
}

fn line_len(buf: &dyn TextBuffer, row: u32) -> u16 {
    buf.line(row).chars().count().min(u16::MAX as usize) as u16
}

fn last_row(buf: &dyn TextBuffer) -> u32 {
    buf.total_rows().saturating_sub(1)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Clamp a position into the buffer (col may sit at end-of-line, one past
/// the last character — Emacs point semantics).
pub fn clamp(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let row = pos.row.min(last_row(buf));
    Pos { row, col: pos.col.min(line_len(buf, row)) }
}

pub fn forward_char(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    if pos.col < line_len(buf, pos.row) {
        Pos { row: pos.row, col: pos.col + 1 }
    } else if pos.row < last_row(buf) {
        Pos { row: pos.row + 1, col: 0 }
    } else {
        pos
    }
}

pub fn backward_char(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    if pos.col > 0 {
        Pos { row: pos.row, col: pos.col - 1 }
    } else if pos.row > 0 {
        let row = pos.row - 1;
        Pos { row, col: line_len(buf, row) }
    } else {
        pos
    }
}

pub fn next_line(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    if pos.row < last_row(buf) {
        clamp(buf, Pos { row: pos.row + 1, col: pos.col })
    } else {
        pos
    }
}

pub fn previous_line(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    if pos.row > 0 {
        clamp(buf, Pos { row: pos.row - 1, col: pos.col })
    } else {
        pos
    }
}

pub fn line_beginning(pos: Pos) -> Pos {
    Pos { row: pos.row, col: 0 }
}

pub fn line_end(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let pos = clamp(buf, pos);
    Pos { row: pos.row, col: line_len(buf, pos.row) }
}

pub fn buffer_beginning() -> Pos {
    Pos { row: 0, col: 0 }
}

pub fn buffer_end(buf: &dyn TextBuffer) -> Pos {
    let row = last_row(buf);
    Pos { row, col: line_len(buf, row) }
}

fn char_at(buf: &dyn TextBuffer, pos: Pos) -> Option<char> {
    buf.line(pos.row).chars().nth(usize::from(pos.col))
}

/// Emacs `forward-word`: move past any non-word chars, then to the end of
/// the next word, crossing line boundaries.
pub fn forward_word(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let mut pos = clamp(buf, pos);
    let end = buffer_end(buf);
    // skip separators
    while pos < end && !char_at(buf, pos).is_some_and(is_word_char) {
        pos = forward_char(buf, pos);
    }
    // skip the word
    while pos < end && char_at(buf, pos).is_some_and(is_word_char) {
        pos = forward_char(buf, pos);
    }
    pos
}

/// Emacs `backward-word`: move back over any non-word chars, then to the
/// start of the previous word, crossing line boundaries.
pub fn backward_word(buf: &dyn TextBuffer, pos: Pos) -> Pos {
    let mut pos = clamp(buf, pos);
    let start = buffer_beginning();
    while pos > start {
        let prev = backward_char(buf, pos);
        if char_at(buf, prev).is_some_and(is_word_char) {
            break;
        }
        pos = prev;
    }
    while pos > start {
        let prev = backward_char(buf, pos);
        if !char_at(buf, prev).is_some_and(is_word_char) {
            break;
        }
        pos = prev;
    }
    pos
}
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::text_mode`
Expected: `test result: ok. 4 passed`

- [ ] **Step 9: Commit**

```bash
cargo fmt
git add src/pane/terminal.rs src/pane.rs src/terminal/runtime.rs src/emacs
git commit -m "feat: scrollback read seam and emacs motion engine"
```

---

### Task 7: TEXT mode — enter/exit, point, motions over a real pane

**Files:**
- Modify: `src/emacs/text_mode.rs` (add `TextModeState`)
- Modify: `src/emacs/mod.rs` (add `text_mode` field to `EmacsState`)
- Create: `src/emacs/render.rs` (point overlay)
- Modify: `src/emacs/mod.rs` (add `pub mod render;`)
- Modify: `src/app/input/emacs.rs` (TEXT-mode dispatch + command execution)
- Modify: `src/ui/panes.rs` (`render_panes` loop at :313-365: one overlay call + one cursor-gate condition)
- Test: inline tests in `src/app/input/emacs.rs`

**Interfaces:**
- Consumes: motions + `Pos`/`TextBuffer` (Task 6), `TerminalRuntime::{text_dims, text_row}` (Task 6), `AppState::pane_info_by_id(PaneId) -> Option<&PaneInfo>` (`src/app/input/mouse.rs:1407`), `AppState::pane_scroll_metrics(&TerminalRuntimeRegistry, PaneId) -> Option<ScrollMetrics>` (`src/app/input/mouse.rs:1480`), `AppState::set_pane_scroll_offset(&self, &TerminalRuntimeRegistry, PaneId, offset_from_bottom: usize)` (`src/app/input/mouse.rs:1735`, `pub(super)` — visible here), `TerminalRuntime::cursor_state(Rect, bool) -> Option<TerminalCursorState { x, y, visible, shape }>`, `ScrollMetrics { offset_from_bottom, max_offset_from_bottom, .. }` (viewport top row = `max_offset_from_bottom - offset_from_bottom`, verified against `copy_mode_viewport_top_row` in `src/app/input/copy_mode.rs:1112`).
- Produces: `TextModeState { pane_id, point: Pos, mark: Option<Pos>, mark_active: bool, entry_offset_from_bottom: usize, goto_line: Option<String> }` at `app.state.emacs.text_mode`; `EmacsState::owns_pane_cursor(&self, PaneId) -> bool`; `render::render_text_mode_overlay(app, frame, info, rt)`; adapter helpers `RuntimeBuffer`, `App::emacs_enter_text_mode()`, `App::emacs_exit_text_mode()`, `App::emacs_scroll_point_into_view(PaneId)` — Tasks 8–12 build on all of these.

- [ ] **Step 1: Write the failing tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    const FIVE_LINES: &[u8] = b"alpha\r\nbravo six\r\ncharlie\r\ndelta\r\necho\r\n";

    fn enter_text_mode(app: &mut App) {
        app.route_client_input(vec![0x18, b'[']); // C-x [
    }

    #[tokio::test]
    async fn c_x_bracket_enters_text_mode_and_q_exits() {
        let (mut app, pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        let text = app.state.emacs.text_mode.as_ref().expect("text mode on");
        assert_eq!(text.pane_id, pane);
        assert!(app.state.emacs.owns_pane_cursor(pane));
        app.route_client_input(vec![b'q']);
        assert!(app.state.emacs.text_mode.is_none());
        assert!(
            sent_bytes(&mut rx).is_empty(),
            "TEXT mode keys never reach the pane"
        );
    }

    #[tokio::test]
    async fn motions_move_point_over_scrollback() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        // M-< : beginning of buffer
        app.route_client_input(vec![0x1b, b'<']);
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!((point.row, point.col), (0, 0));
        // C-f C-f -> col 2
        app.route_client_input(vec![0x06, 0x06]);
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!((point.row, point.col), (0, 2));
        // C-n -> row 1 (col clamps within "bravo six")
        app.route_client_input(vec![0x0e]);
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!((point.row, point.col), (1, 2));
        // C-e -> end of "bravo six"
        app.route_client_input(vec![0x05]);
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!((point.row, point.col), (1, 9));
        // M-b -> start of "six"
        app.route_client_input(vec![0x1b, b'b']);
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!((point.row, point.col), (1, 6));
    }

    /// Enough content to guarantee real scrollback in the test viewport.
    fn fifty_lines() -> Vec<u8> {
        (1..=50)
            .flat_map(|i| format!("line {i}\r\n").into_bytes())
            .collect()
    }

    #[tokio::test]
    async fn beginning_of_buffer_scrolls_viewport_and_exit_restores_it() {
        let (mut app, pane, _rx) = emacs_app_with_channel(&fifty_lines());
        let entry_offset = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane)
            .expect("metrics")
            .offset_from_bottom;
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        let metrics = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane)
            .expect("metrics");
        assert!(
            metrics.max_offset_from_bottom > 0,
            "fixture must actually have scrollback"
        );
        assert_eq!(
            metrics.offset_from_bottom, metrics.max_offset_from_bottom,
            "viewport followed point to the top"
        );
        app.route_client_input(vec![0x1b]); // ESC exits
        assert!(app.state.emacs.text_mode.is_none());
        let metrics = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane)
            .expect("metrics");
        assert_eq!(metrics.offset_from_bottom, entry_offset, "scroll restored");
    }

    #[tokio::test]
    async fn unbound_printable_keys_report_read_only() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![b'x']);
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("Buffer is read-only")
        );
        assert!(sent_bytes(&mut rx).is_empty());
        assert!(app.state.emacs.text_mode.is_some(), "still in TEXT mode");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: compile error — `text_mode` field and `owns_pane_cursor` don't exist yet

- [ ] **Step 3: Add the state.** In `src/emacs/text_mode.rs` (above the tests):

```rust
/// State of an active TEXT-mode session over one pane.
#[derive(Debug)]
pub struct TextModeState {
    pub pane_id: crate::layout::PaneId,
    pub point: Pos,
    /// Last mark set in this session (survives deactivation, like Emacs).
    pub mark: Option<Pos>,
    /// Transient-mark: whether the region is currently active.
    pub mark_active: bool,
    /// Scroll offset when entering; restored on exit.
    pub entry_offset_from_bottom: usize,
    /// Digits typed after `M-g g` (goto-line prompt; wired in Task 12).
    pub goto_line: Option<String>,
}
```

In `src/emacs/mod.rs`: add `pub mod render;` + `pub mod text_mode;` to the module list, and extend `EmacsState`:

```rust
    /// Active TEXT-mode session, if any (`C-x [`). `mode` stays
    /// `Mode::Terminal`; the interception hook owns all keys while `Some`.
    pub text_mode: Option<text_mode::TextModeState>,
```

Initialize `text_mode: None,` in `EmacsState::from_config`, and in `apply_config`'s `if !self.enabled` block add `self.text_mode = None;`. Add the render/cursor helper to `impl EmacsState`:

```rust
    /// True when TEXT mode owns the cursor for this pane (suppresses the
    /// host cursor in the pane renderer).
    pub fn owns_pane_cursor(&self, pane_id: crate::layout::PaneId) -> bool {
        self.text_mode
            .as_ref()
            .is_some_and(|text| text.pane_id == pane_id)
    }
```

- [ ] **Step 4: Extend the adapter.** In `src/app/input/emacs.rs`:

Add imports:

```rust
use crate::emacs::text_mode::{self, Pos, TextBuffer, TextModeState};
```

Add the buffer adapter (file scope, below the `impl App` block):

```rust
/// `TextBuffer` over a live pane runtime.
struct RuntimeBuffer<'a> {
    rt: &'a crate::terminal::TerminalRuntime,
}

impl TextBuffer for RuntimeBuffer<'_> {
    fn total_rows(&self) -> u32 {
        self.rt
            .text_dims()
            .map_or(0, |(rows, _)| rows.min(u32::MAX as usize) as u32)
    }
    fn line(&self, row: u32) -> String {
        self.rt.text_row(row).unwrap_or_default()
    }
}
```

In `emacs_intercept_key`, replace the block from `let Some(chord) = ...` through the final `match ... }` with a dispatch that picks the right keymap (the quoted-insert block above it stays unchanged):

```rust
        let text_active = self.state.emacs.text_mode.is_some();

        let Some(chord) = Chord::from_key(&key) else {
            return text_active;
        };

        // C-g always cancels an in-flight chord (and, in TEXT mode,
        // deactivates the mark — Task 8).
        if !self.state.emacs.pending.is_empty() && chord == Chord::ctrl('g') {
            self.state.emacs.pending.clear();
            self.state.emacs.echo = Some("Quit".to_string());
            return true;
        }

        let mut seq = self.state.emacs.pending.clone();
        seq.push(chord);
        let lookup = if text_active {
            self.state.emacs.keymaps.text.lookup(&seq)
        } else {
            self.state.emacs.keymaps.global.lookup(&seq)
        };
        match lookup {
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
                    false
                } else {
                    self.state.emacs.echo =
                        Some(format!("{} is undefined", format_seq(&seq)));
                    true
                }
            }
        }
```

Also extend `emacs_would_consume` so TEXT mode swallows repeat/release events, and repeats drive motions (kitty terminals send `Repeat`, legacy terminals send repeated `Press` — treat them the same in TEXT mode). Replace the `match key.kind` block in `emacs_intercept_key` with:

```rust
        match key.kind {
            KeyEventKind::Press => {}
            KeyEventKind::Repeat => {
                if self.state.emacs.text_mode.is_none() {
                    return self.emacs_would_consume(key);
                }
                // fall through: repeats behave like presses in TEXT mode
            }
            KeyEventKind::Release => {
                return self.emacs_would_consume(key);
            }
        }
```

and add as the first line of `emacs_would_consume`:

```rust
        if self.state.emacs.text_mode.is_some() {
            return true;
        }
```

In `execute_emacs_command`, replace the grouped placeholder arm: `TextMode`, `ExitTextMode`, and all motion commands get real arms (kill-ring commands stay grouped until Tasks 10–11):

```rust
            EmacsCommand::TextMode => self.emacs_enter_text_mode(),
            EmacsCommand::ExitTextMode => self.emacs_exit_text_mode(),
            EmacsCommand::ForwardChar
            | EmacsCommand::BackwardChar
            | EmacsCommand::NextLine
            | EmacsCommand::PreviousLine
            | EmacsCommand::ForwardWord
            | EmacsCommand::BackwardWord
            | EmacsCommand::MoveBeginningOfLine
            | EmacsCommand::MoveEndOfLine
            | EmacsCommand::ScrollUp
            | EmacsCommand::ScrollDown
            | EmacsCommand::BeginningOfBuffer
            | EmacsCommand::EndOfBuffer => self.emacs_text_motion(cmd),
            // Implemented by later tasks of this plan:
            EmacsCommand::GotoLine
            | EmacsCommand::SetMark
            | EmacsCommand::ExchangePointAndMark
            | EmacsCommand::KillRingSave
            | EmacsCommand::KillRegion
            | EmacsCommand::Yank
            | EmacsCommand::YankPop => {}
```

And add the TEXT-mode methods to the `impl App` block:

```rust
    /// `C-x [` — freeze the focused pane into TEXT mode. Mirrors
    /// `AppState::enter_copy_mode` (src/app/input/copy_mode.rs:38): the
    /// point seeds from the visible host cursor, else the viewport's
    /// bottom-left.
    fn emacs_enter_text_mode(&mut self) {
        let Some((ws_idx, pane_id)) = self.emacs_focused_pane() else {
            return;
        };
        let Some(info) = self.state.pane_info_by_id(pane_id).cloned() else {
            return;
        };
        if info.inner_rect.width == 0 || info.inner_rect.height == 0 {
            return;
        }
        let Some(metrics) = self
            .state
            .pane_scroll_metrics(&self.terminal_runtimes, pane_id)
        else {
            return;
        };
        let viewport_top =
            (metrics.max_offset_from_bottom - metrics.offset_from_bottom) as u32;
        let point = {
            let Some(rt) = self.state.runtime_for_pane_in_workspace(
                &self.terminal_runtimes,
                ws_idx,
                pane_id,
            ) else {
                return;
            };
            let (row_in_view, col) = rt
                .cursor_state(info.inner_rect, true)
                .filter(|cursor| cursor.visible)
                .map(|cursor| {
                    (
                        cursor.y.saturating_sub(info.inner_rect.y),
                        cursor.x.saturating_sub(info.inner_rect.x),
                    )
                })
                .unwrap_or((info.inner_rect.height.saturating_sub(1), 0));
            let buf = RuntimeBuffer { rt };
            text_mode::clamp(
                &buf,
                Pos { row: viewport_top + u32::from(row_in_view), col },
            )
        };
        self.state.emacs.text_mode = Some(TextModeState {
            pane_id,
            point,
            mark: None,
            mark_active: false,
            entry_offset_from_bottom: metrics.offset_from_bottom,
            goto_line: None,
        });
    }

    /// `q` / `ESC` — back to the live cursor; restore the entry scroll.
    fn emacs_exit_text_mode(&mut self) {
        let Some(text) = self.state.emacs.text_mode.take() else {
            return;
        };
        self.state.set_pane_scroll_offset(
            &self.terminal_runtimes,
            text.pane_id,
            text.entry_offset_from_bottom,
        );
    }

    /// Run one motion command against the frozen buffer, then keep the
    /// point visible.
    fn emacs_text_motion(&mut self, cmd: EmacsCommand) {
        let Some(text) = self.state.emacs.text_mode.as_ref() else {
            return;
        };
        let (pane_id, point) = (text.pane_id, text.point);
        let Some((ws_idx, _)) = self.emacs_focused_pane() else {
            return;
        };
        let page = self
            .state
            .pane_info_by_id(pane_id)
            .map_or(1, |info| {
                u32::from(info.inner_rect.height.saturating_sub(2).max(1))
            });
        let new_point = {
            let Some(rt) = self.state.runtime_for_pane_in_workspace(
                &self.terminal_runtimes,
                ws_idx,
                pane_id,
            ) else {
                return;
            };
            let buf = RuntimeBuffer { rt };
            match cmd {
                EmacsCommand::ForwardChar => text_mode::forward_char(&buf, point),
                EmacsCommand::BackwardChar => text_mode::backward_char(&buf, point),
                EmacsCommand::NextLine => text_mode::next_line(&buf, point),
                EmacsCommand::PreviousLine => text_mode::previous_line(&buf, point),
                EmacsCommand::ForwardWord => text_mode::forward_word(&buf, point),
                EmacsCommand::BackwardWord => text_mode::backward_word(&buf, point),
                EmacsCommand::MoveBeginningOfLine => text_mode::line_beginning(point),
                EmacsCommand::MoveEndOfLine => text_mode::line_end(&buf, point),
                EmacsCommand::ScrollUp => text_mode::clamp(
                    &buf,
                    Pos { row: point.row.saturating_add(page), col: point.col },
                ),
                EmacsCommand::ScrollDown => text_mode::clamp(
                    &buf,
                    Pos { row: point.row.saturating_sub(page), col: point.col },
                ),
                EmacsCommand::BeginningOfBuffer => text_mode::buffer_beginning(),
                EmacsCommand::EndOfBuffer => text_mode::buffer_end(&buf),
                _ => point,
            }
        };
        if let Some(text) = self.state.emacs.text_mode.as_mut() {
            text.point = new_point;
        }
        self.emacs_scroll_point_into_view(pane_id);
    }

    /// Scroll the pane so the point is inside the viewport.
    fn emacs_scroll_point_into_view(&mut self, pane_id: crate::layout::PaneId) {
        let Some(point) = self
            .state
            .emacs
            .text_mode
            .as_ref()
            .filter(|text| text.pane_id == pane_id)
            .map(|text| text.point)
        else {
            return;
        };
        let Some(info) = self.state.pane_info_by_id(pane_id) else {
            return;
        };
        let view_rows = u32::from(info.inner_rect.height.max(1));
        let Some(metrics) = self
            .state
            .pane_scroll_metrics(&self.terminal_runtimes, pane_id)
        else {
            return;
        };
        let top = (metrics.max_offset_from_bottom - metrics.offset_from_bottom) as u32;
        let new_top = if point.row < top {
            point.row
        } else if point.row >= top + view_rows {
            point.row + 1 - view_rows
        } else {
            return;
        };
        let offset = metrics
            .max_offset_from_bottom
            .saturating_sub(new_top as usize);
        self.state
            .set_pane_scroll_offset(&self.terminal_runtimes, pane_id, offset);
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `test result: ok. 11 passed`

- [ ] **Step 6: Render the point.** Create `src/emacs/render.rs` (add `pub mod render;` to `src/emacs/mod.rs` if not done in Step 3):

```rust
//! Ratatui overlays for the Emacs layer. Pure draw functions called from
//! the two render seams in `src/ui.rs` / `src/ui/panes.rs`.

use ratatui::style::{Modifier, Style};
use ratatui::Frame;

use crate::app::AppState;
use crate::layout::PaneInfo;
use crate::terminal::TerminalRuntime;

/// TEXT-mode overlay for one pane: block point (region highlight lands in
/// a later task). Drawn after the pane grid, like copy-mode's cursor.
pub fn render_text_mode_overlay(
    app: &AppState,
    frame: &mut Frame,
    info: &PaneInfo,
    rt: &TerminalRuntime,
) {
    let Some(text) = app
        .emacs
        .text_mode
        .as_ref()
        .filter(|text| text.pane_id == info.id)
    else {
        return;
    };
    let Some(metrics) = rt.scroll_metrics() else {
        return;
    };
    let inner = info.inner_rect;
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let top = (metrics.max_offset_from_bottom - metrics.offset_from_bottom) as u32;

    // Point: reversed+bold cell (theme-agnostic, like a block cursor).
    let Some(rel_row) = text.point.row.checked_sub(top) else {
        return;
    };
    let Ok(y) = u16::try_from(rel_row) else {
        return;
    };
    if y >= inner.height {
        return;
    }
    let x = text.point.col.min(inner.width.saturating_sub(1));
    let cell = &mut frame.buffer_mut()[(inner.x + x, inner.y + y)];
    cell.set_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD));
}
```

Wire the two-line seam in `src/ui/panes.rs` `render_panes` (:313-365). After `render_copy_mode_cursor(app, frame, info);` add:

```rust
            // Emacs layer seam (fork).
            crate::emacs::render::render_text_mode_overlay(app, frame, info, rt);
```

And extend the `show_cursor` condition (:315-319) with one line so the host cursor hides while TEXT mode owns the pane:

```rust
            let show_cursor = info.is_focused
                && terminal_active
                && !pane_is_scrolled_back(rt)
                && !app.emacs.owns_pane_cursor(info.id) // Emacs layer seam (fork)
                && app.pane_exposes_host_cursor(ws_idx, info.id);
```

- [ ] **Step 7: Build + full regression**

Run: `cargo build --locked 2>&1 | tail -3`
Expected: `Finished \`dev\` profile`

Run: `cargo test --locked --bin herdr app::`
Expected: `test result: ok.`

- [ ] **Step 8: Manual smoke (rendering is not unit-tested — say so, verify by eye).** In a Ghostty terminal:

```bash
cat > /tmp/herdr-emacs-smoke.toml <<'EOF'
[emacs]
enabled = true
EOF
HERDR_CONFIG_PATH=/tmp/herdr-emacs-smoke.toml cargo run --locked
```

Checklist: run `seq 1 200` in the pane; `C-x [` shows a reversed block point and hides the host cursor; `C-p`/`C-n`/`C-f`/`C-b` move it; `M-<` jumps to line 1 with the view following; `C-v`/`M-v` page; `q` returns to the live prompt at the pre-entry scroll position; typing letters in TEXT mode does nothing to the shell.

- [ ] **Step 9: Commit**

```bash
cargo fmt
git add src/emacs src/app/input/emacs.rs src/ui/panes.rs
git commit -m "feat: emacs TEXT mode with point, motions, and viewport tracking"
```

### Task 8: Mark, region rendering, C-x C-x

**Files:**
- Modify: `src/app/input/emacs.rs` (SetMark / ExchangePointAndMark / KeyboardQuit arms + tests)
- Modify: `src/emacs/render.rs` (region highlight)

**Interfaces:**
- Consumes: `TextModeState.mark`/`mark_active` (Task 7), `Pos: Ord` (row-major ordering, Task 6), `app.palette.surface1` (`Palette`, `src/app/state.rs:60`).
- Produces: transient-mark behavior relied on by Task 10 (`kill-ring-save` requires `mark_active`); region-ordering convention `(start, end) = min/max(point, mark)`, end-exclusive.

- [ ] **Step 1: Write the failing tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    #[tokio::test]
    async fn c_spc_sets_mark_and_c_g_deactivates_it() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        app.route_client_input(vec![0x00]); // C-SPC
        {
            let text = app.state.emacs.text_mode.as_ref().unwrap();
            assert_eq!(text.mark.map(|m| (m.row, m.col)), Some((0, 0)));
            assert!(text.mark_active);
        }
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Mark set"));
        app.route_client_input(vec![0x07]); // C-g
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert!(!text.mark_active, "deactivated");
        assert_eq!(
            text.mark.map(|m| (m.row, m.col)),
            Some((0, 0)),
            "mark position survives deactivation (Emacs behavior)"
        );
    }

    #[tokio::test]
    async fn c_x_c_x_exchanges_point_and_mark() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-< : point (0,0)
        app.route_client_input(vec![0x00]); // C-SPC : mark (0,0)
        app.route_client_input(vec![0x0e, 0x06]); // C-n C-f : point (1,1)
        app.route_client_input(vec![0x18, 0x18]); // C-x C-x
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!((text.point.row, text.point.col), (0, 0));
        assert_eq!(text.mark.map(|m| (m.row, m.col)), Some((1, 1)));
        assert!(text.mark_active);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: the two new tests FAIL (`mark` stays `None` — the commands are still no-ops)

- [ ] **Step 3: Implement.** In `src/app/input/emacs.rs`, replace the `SetMark`/`ExchangePointAndMark` entries in the grouped later-tasks arm of `execute_emacs_command` with real arms:

```rust
            EmacsCommand::SetMark => self.emacs_set_mark(),
            EmacsCommand::ExchangePointAndMark => self.emacs_exchange_point_and_mark(),
```

extend the `KeyboardQuit` arm to deactivate the mark:

```rust
            EmacsCommand::KeyboardQuit => {
                self.state.emacs.pending.clear();
                if let Some(text) = self.state.emacs.text_mode.as_mut() {
                    text.mark_active = false;
                }
                self.state.emacs.echo = Some("Quit".to_string());
            }
```

and add the methods to the `impl App` block:

```rust
    /// `C-SPC` — set the mark at point and activate the region
    /// (transient-mark). The mark-ring push lands in the next task.
    fn emacs_set_mark(&mut self) {
        let Some(text) = self.state.emacs.text_mode.as_mut() else {
            return;
        };
        text.mark = Some(text.point);
        text.mark_active = true;
        self.state.emacs.echo = Some("Mark set".to_string());
    }

    /// `C-x C-x` — exchange point and mark, reactivating the region.
    fn emacs_exchange_point_and_mark(&mut self) {
        let pane_id = {
            let Some(text) = self.state.emacs.text_mode.as_mut() else {
                return;
            };
            let Some(mark) = text.mark else {
                self.state.emacs.echo = Some("No mark set in this buffer".to_string());
                return;
            };
            text.mark = Some(text.point);
            text.point = mark;
            text.mark_active = true;
            text.pane_id
        };
        self.emacs_scroll_point_into_view(pane_id);
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `test result: ok. 13 passed`

- [ ] **Step 5: Render the region.** In `src/emacs/render.rs` `render_text_mode_overlay`, insert between the `let top = ...` line and the point-drawing block:

```rust
    // Region: min(point, mark)..max(point, mark), end-exclusive,
    // row-major (Pos derives Ord in that order).
    if text.mark_active {
        if let Some(mark) = text.mark {
            let (start, end) = if mark <= text.point {
                (mark, text.point)
            } else {
                (text.point, mark)
            };
            let style = Style::default().bg(app.palette.surface1);
            for rel_y in 0..inner.height {
                let row = top + u32::from(rel_y);
                if row < start.row || row > end.row {
                    continue;
                }
                let from = if row == start.row { start.col } else { 0 };
                let to = if row == end.row { end.col } else { inner.width };
                for x in from..to.min(inner.width) {
                    frame.buffer_mut()[(inner.x + x, inner.y + rel_y)].set_style(style);
                }
            }
        }
    }
```

- [ ] **Step 6: Build + manual smoke (region rendering is verified by eye, not unit tests — explicitly).**

Run: `cargo build --locked 2>&1 | tail -3`
Expected: `Finished \`dev\` profile`

Smoke (same config file as Task 7): in TEXT mode press `C-SPC`, move with `C-n`/`C-e` — the region shades with the theme's surface color and follows the point; `C-x C-x` jumps between the two ends; `C-g` clears the shading and echoes "Quit" (echo becomes visible on-screen in Task 12).

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add src/emacs/render.rs src/app/input/emacs.rs
git commit -m "feat: emacs mark, region highlight, and exchange-point-and-mark"
```

---

### Task 9: Kill ring + mark ring (`rings.rs`)

**Files:**
- Create: `src/emacs/rings.rs`
- Modify: `src/emacs/mod.rs` (`pub mod rings;` + `kill_ring`/`mark_rings` fields)
- Modify: `src/app/input/emacs.rs` (`emacs_set_mark` pushes the per-pane mark ring)
- Test: inline tests in `src/emacs/rings.rs`, one mark-ring test in `src/app/input/emacs.rs`

**Interfaces:**
- Consumes: `EmacsConfig.kill_ring_max` / `mark_ring_max` (Task 1).
- Produces (used by Tasks 10–11 and Phase 4):
  - `KillRing::{new(max), set_max, push(String), head() -> Option<&str>, yank() -> Option<String>, yank_pop() -> Option<String>, sync_from_system(Option<String>), len, is_empty}`
  - `MarkRing::{new(max), push((u32, u16)), pop_rotate() -> Option<(u32, u16)>, len, is_empty}` — `pop_rotate` is the Phase 4 `C-u C-SPC` primitive, implemented and tested now
  - `EmacsState.kill_ring: KillRing`, `EmacsState.mark_rings: HashMap<PaneId, MarkRing>`

- [ ] **Step 1: Write the failing ring tests.** Create `src/emacs/rings.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_ring_pushes_dedupes_and_truncates() {
        let mut ring = KillRing::new(3);
        assert!(ring.is_empty());
        assert_eq!(ring.head(), None);
        ring.push("one".into());
        ring.push("one".into()); // consecutive duplicate ignored
        ring.push(String::new()); // empty ignored
        assert_eq!(ring.len(), 1);
        ring.push("two".into());
        ring.push("three".into());
        ring.push("four".into()); // truncates to max 3
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.head(), Some("four"));
    }

    #[test]
    fn yank_and_yank_pop_rotate_the_ring() {
        let mut ring = KillRing::new(60);
        ring.push("a".into());
        ring.push("b".into());
        ring.push("c".into());
        assert_eq!(ring.yank().as_deref(), Some("c"));
        assert_eq!(ring.yank_pop().as_deref(), Some("b"));
        assert_eq!(ring.yank_pop().as_deref(), Some("a"));
        assert_eq!(ring.yank_pop().as_deref(), Some("c"), "wraps");
        // a fresh kill resets rotation
        ring.push("d".into());
        assert_eq!(ring.yank().as_deref(), Some("d"));
    }

    #[test]
    fn sync_from_system_adopts_fresh_clipboard_text() {
        let mut ring = KillRing::new(60);
        ring.push("kill".into());
        ring.sync_from_system(None);
        ring.sync_from_system(Some(String::new()));
        assert_eq!(ring.head(), Some("kill"));
        ring.sync_from_system(Some("kill".into())); // same as head: no-op
        assert_eq!(ring.len(), 1);
        ring.sync_from_system(Some("clip".into()));
        assert_eq!(ring.head(), Some("clip"));
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn set_max_shrinks_the_ring() {
        let mut ring = KillRing::new(10);
        for i in 0..5 {
            ring.push(format!("k{i}"));
        }
        ring.set_max(2);
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.head(), Some("k4"));
    }

    #[test]
    fn mark_ring_pushes_truncates_and_rotates() {
        let mut ring = MarkRing::new(2);
        ring.push((1, 0));
        ring.push((2, 3));
        ring.push((5, 1)); // truncates oldest
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.pop_rotate(), Some((5, 1)));
        assert_eq!(ring.pop_rotate(), Some((2, 3)));
        assert_eq!(ring.pop_rotate(), Some((5, 1)), "rotates, Emacs-style");
        assert!(MarkRing::new(4).pop_rotate().is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::rings`
Expected: compile error — `KillRing` not found (also add `pub mod rings;` to `src/emacs/mod.rs`)

- [ ] **Step 3: Implement the rings.** Above the tests in `src/emacs/rings.rs`:

```rust
//! Kill ring and mark ring (spec: depth 60 / 16, config-overridable).

use std::collections::VecDeque;

/// Emacs kill ring: front = most recent kill.
#[derive(Debug, Clone)]
pub struct KillRing {
    entries: VecDeque<String>,
    max: usize,
    /// Rotation cursor for `yank-pop`; 0 = head.
    yank_index: usize,
}

impl KillRing {
    pub fn new(max: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max: max.max(1),
            yank_index: 0,
        }
    }

    pub fn set_max(&mut self, max: usize) {
        self.max = max.max(1);
        self.entries.truncate(self.max);
        self.yank_index = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn head(&self) -> Option<&str> {
        self.entries.front().map(String::as_str)
    }

    /// `kill-new`: push as the newest kill. Empty strings and consecutive
    /// duplicates are ignored (Emacs `kill-do-not-save-duplicates` spirit).
    pub fn push(&mut self, text: String) {
        self.yank_index = 0;
        if text.is_empty() || self.head() == Some(text.as_str()) {
            return;
        }
        self.entries.push_front(text);
        self.entries.truncate(self.max);
    }

    /// `yank`: newest entry; resets the rotation cursor.
    pub fn yank(&mut self) -> Option<String> {
        self.yank_index = 0;
        self.entries.front().cloned()
    }

    /// `yank-pop` (M-y after a yank): next-older entry, wrapping.
    pub fn yank_pop(&mut self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        self.yank_index = (self.yank_index + 1) % self.entries.len();
        self.entries.get(self.yank_index).cloned()
    }

    /// Interprogram paste: adopt the system clipboard as the newest kill
    /// when it holds something new (bidirectional clipboard sync, read side).
    pub fn sync_from_system(&mut self, clipboard: Option<String>) {
        if let Some(clip) = clipboard {
            if !clip.is_empty() && self.head() != Some(clip.as_str()) {
                self.push(clip);
            }
        }
    }
}

/// Per-pane mark ring: front = most recent mark, `(row, col)` absolute.
#[derive(Debug, Clone)]
pub struct MarkRing {
    entries: VecDeque<(u32, u16)>,
    max: usize,
}

impl MarkRing {
    pub fn new(max: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max: max.max(1),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn push(&mut self, mark: (u32, u16)) {
        self.entries.push_front(mark);
        self.entries.truncate(self.max);
    }

    /// `C-u C-SPC` primitive (wired in Phase 4): pop the most recent mark
    /// and rotate it to the back.
    pub fn pop_rotate(&mut self) -> Option<(u32, u16)> {
        let front = self.entries.pop_front()?;
        self.entries.push_back(front);
        Some(front)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::rings`
Expected: `test result: ok. 5 passed`

- [ ] **Step 5: Wire the rings into `EmacsState`.** In `src/emacs/mod.rs` add fields to `EmacsState`:

```rust
    /// The kill ring (shared across panes, like Emacs).
    pub kill_ring: rings::KillRing,
    /// Per-pane mark rings (spec: per-pane, depth `mark_ring_max`).
    pub mark_rings: std::collections::HashMap<crate::layout::PaneId, rings::MarkRing>,
```

Initialize in `from_config`:

```rust
            kill_ring: rings::KillRing::new(config.kill_ring_max.max(1)),
            mark_rings: std::collections::HashMap::new(),
```

And in `apply_config`, after `self.kill_ring_max = ...`:

```rust
        self.kill_ring.set_max(config.kill_ring_max.max(1));
```

Then in `src/app/input/emacs.rs` replace `emacs_set_mark` so every mark set pushes the pane's ring (spec: "every mark set pushes"):

```rust
    /// `C-SPC` — set the mark at point, activate the region, and push the
    /// pane's mark ring.
    fn emacs_set_mark(&mut self) {
        let max = self.state.emacs.mark_ring_max;
        let Some(text) = self.state.emacs.text_mode.as_mut() else {
            return;
        };
        text.mark = Some(text.point);
        text.mark_active = true;
        let (pane_id, point) = (text.pane_id, text.point);
        self.state
            .emacs
            .mark_rings
            .entry(pane_id)
            .or_insert_with(|| crate::emacs::rings::MarkRing::new(max))
            .push((point.row, point.col));
        self.state.emacs.echo = Some("Mark set".to_string());
    }
```

- [ ] **Step 6: Write + run the mark-ring wiring test** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    #[tokio::test]
    async fn every_mark_set_pushes_the_pane_mark_ring() {
        let (mut app, pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x00]); // C-SPC
        app.route_client_input(vec![0x0e]); // C-n
        app.route_client_input(vec![0x00]); // C-SPC
        assert_eq!(app.state.emacs.mark_rings.get(&pane).map(|r| r.len()), Some(2));
    }
```

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `test result: ok. 14 passed`

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add src/emacs src/app/input/emacs.rs
git commit -m "feat: emacs kill ring and per-pane mark rings"
```

### Task 10: M-w / C-w — region to kill ring + system clipboard; C-y is read-only in TEXT mode

**Files:**
- Modify: `src/app/input/emacs.rs` (KillRingSave/KillRegion/Yank/YankPop arms, region extraction, clipboard event + tests)

**Interfaces:**
- Consumes: `TerminalRuntime::read_text_range` + `text_dims` (Task 6), `KillRing` (Task 9), region convention (Task 8), `AppEvent::ClipboardWrite { content: Vec<u8> }` (`src/events.rs`) via `self.event_tx.try_send(...)` — the exact pattern `App::handle_copy_mode_key` uses (`src/app/input/copy_mode.rs:26-34`); the event loop already routes it to OSC52/wl-copy.
- Produces: `App::emacs_region_text(ws_idx, pane_id, start, end) -> Option<String>` (end-exclusive `Pos` region → ghostty's inclusive-endpoint read); kill-ring content that Task 11 yanks.

- [ ] **Step 1: Write the failing tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    /// M-< C-SPC C-n C-e : region covers "alpha\nbravo six".
    fn select_first_two_lines(app: &mut App) {
        enter_text_mode(app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        app.route_client_input(vec![0x00]); // C-SPC
        app.route_client_input(vec![0x0e, 0x05]); // C-n C-e
    }

    fn clipboard_event_text(app: &mut App) -> String {
        match app.event_rx.try_recv().expect("clipboard event") {
            crate::events::AppEvent::ClipboardWrite { content } => {
                String::from_utf8(content).expect("utf8 clipboard")
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn m_w_saves_region_to_kill_ring_and_clipboard() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        app.state.emacs.clipboard_sync = true; // event only; no shelling out
        select_first_two_lines(&mut app);
        app.route_client_input(vec![0x1b, b'w']); // M-w
        assert_eq!(
            app.state.emacs.kill_ring.head(),
            Some("alpha\nbravo six")
        );
        assert_eq!(clipboard_event_text(&mut app), "alpha\nbravo six");
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert!(!text.mark_active, "region deactivates after save");
        assert!(app.state.emacs.text_mode.is_some(), "still in TEXT mode");
    }

    #[tokio::test]
    async fn c_w_in_read_only_buffer_saves_like_m_w() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        select_first_two_lines(&mut app);
        app.route_client_input(vec![0x17]); // C-w
        assert_eq!(
            app.state.emacs.kill_ring.head(),
            Some("alpha\nbravo six")
        );
        assert!(!app.state.emacs.text_mode.as_ref().unwrap().mark_active);
    }

    #[tokio::test]
    async fn m_w_without_active_mark_complains() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'w']); // M-w, no mark
        assert!(app.state.emacs.kill_ring.is_empty());
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("The mark is not active now")
        );
    }

    #[tokio::test]
    async fn c_y_in_text_mode_reports_read_only() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        app.state.emacs.kill_ring.push("something".into());
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x19]); // C-y
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("Buffer is read-only")
        );
        assert!(sent_bytes(&mut rx).is_empty(), "nothing typed into the PTY");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: the four new tests FAIL (kill ring stays empty, echo stays `None`)

- [ ] **Step 3: Implement.** In `execute_emacs_command`, replace the remaining grouped arm (`KillRingSave | KillRegion | Yank | YankPop => {}`) with:

```rust
            // In a read-only buffer C-w cannot delete, so kill-region
            // degrades to kill-ring-save (spec §Phase 1).
            EmacsCommand::KillRingSave | EmacsCommand::KillRegion => {
                self.emacs_kill_ring_save()
            }
            EmacsCommand::Yank | EmacsCommand::YankPop => {
                if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                }
                // Live-mode yank lands in the next task of this plan.
            }
```

Add the methods to the `impl App` block:

```rust
    /// `M-w` / `C-w` — push the region onto the kill ring, sync the
    /// system clipboard, deactivate the mark.
    fn emacs_kill_ring_save(&mut self) {
        let Some((ws_idx, _)) = self.emacs_focused_pane() else {
            return;
        };
        let Some(text) = self.state.emacs.text_mode.as_ref() else {
            return;
        };
        if !text.mark_active {
            self.state.emacs.echo = Some("The mark is not active now".to_string());
            return;
        }
        let Some(mark) = text.mark else {
            return;
        };
        let (pane_id, point) = (text.pane_id, text.point);
        let (start, end) = if mark <= point { (mark, point) } else { (point, mark) };
        let Some(content) = self.emacs_region_text(ws_idx, pane_id, start, end) else {
            self.state.emacs.echo = Some("Empty region".to_string());
            return;
        };
        self.state.emacs.kill_ring.push(content.clone());
        if self.state.emacs.clipboard_sync {
            if self
                .event_tx
                .try_send(crate::events::AppEvent::ClipboardWrite {
                    content: content.into_bytes(),
                })
                .is_err()
            {
                tracing::warn!("failed to queue emacs clipboard write event");
            }
        }
        if let Some(text) = self.state.emacs.text_mode.as_mut() {
            text.mark_active = false;
        }
    }

    /// Region text for `[start, end)` (end-exclusive, Emacs semantics),
    /// converted to ghostty's inclusive-endpoint `(col, row)` read.
    fn emacs_region_text(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        start: Pos,
        end: Pos,
    ) -> Option<String> {
        if start >= end {
            return None;
        }
        let rt = self.state.runtime_for_pane_in_workspace(
            &self.terminal_runtimes,
            ws_idx,
            pane_id,
        )?;
        let (_, cols) = rt.text_dims()?;
        let end_inclusive = if end.col > 0 {
            (end.col - 1, end.row)
        } else {
            (cols.saturating_sub(1), end.row.checked_sub(1)?)
        };
        rt.read_text_range((start.col, start.row), end_inclusive)
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `test result: ok. 18 passed`. If the extracted string differs in whitespace (ghostty formats the read), print the actual value, pin the assertion to it, and note the exact join/trim semantics in `emacs_region_text`'s doc comment.

- [ ] **Step 5: Manual smoke — the Wayland clipboard end of success criterion 2.**

Using the Task 7 smoke config: in a pane with output, `C-x [`, `C-SPC`, move, `M-w`, then in another window `wl-paste` — the region text is on the system clipboard (delivered through herdr's existing ClipboardWrite path).

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/app/input/emacs.rs
git commit -m "feat: emacs kill-ring-save with system clipboard sync"
```

---

### Task 11: Live-mode C-y / M-y — yank into the pane PTY

**Files:**
- Modify: `src/emacs/mod.rs` (`LastYank` + `last_yank` field)
- Modify: `src/app/input/emacs.rs` (live yank/yank-pop + last-yank bookkeeping + tests)

**Interfaces:**
- Consumes: `KillRing::{yank, yank_pop, sync_from_system}` (Task 9), `crate::platform::read_clipboard_text() -> Option<String>` (the clipboard-read path `App::handle_key` already uses for modal paste, `src/app/input/mod.rs:74-79`), `TerminalRuntime::{input_state, try_send_bytes}` — bracketed-paste framing copied from the `RawInputEvent::Paste` arm of `route_client_events` (`src/app/mod.rs:1591-1619`).
- Produces: `EmacsState.last_yank: Option<LastYank { pane_id, chars }>`; live `C-y`/`M-y` per spec §Phase 1.

- [ ] **Step 1: Write the failing tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    #[tokio::test]
    async fn c_y_types_kill_ring_head_into_the_pty() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.state.emacs.kill_ring.push("hello".into());
        app.route_client_input(vec![0x19]); // C-y in live mode
        // test runtime has bracketed paste off -> raw text
        assert_eq!(sent_bytes(&mut rx), b"hello".to_vec());
        let last = app.state.emacs.last_yank.as_ref().expect("yank recorded");
        assert_eq!(last.chars, 5);
    }

    #[tokio::test]
    async fn m_y_replaces_the_previous_yank_with_the_older_kill() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.state.emacs.kill_ring.push("older".into());
        app.state.emacs.kill_ring.push("newest".into());
        app.route_client_input(vec![0x19]); // C-y -> "newest"
        assert_eq!(sent_bytes(&mut rx), b"newest".to_vec());
        app.route_client_input(vec![0x1b, b'y']); // M-y
        let mut expected = vec![0x7f; 6]; // erase "newest"
        expected.extend_from_slice(b"older");
        assert_eq!(sent_bytes(&mut rx), expected);
    }

    #[tokio::test]
    async fn m_y_without_a_preceding_yank_complains() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.state.emacs.kill_ring.push("x".into());
        app.route_client_input(vec![0x1b, b'y']); // M-y cold
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("Previous command was not a yank")
        );
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn typing_between_yanks_breaks_the_yank_chain() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.state.emacs.kill_ring.push("x".into());
        app.route_client_input(vec![0x19]); // C-y
        app.route_client_input(b"a".to_vec()); // plain key passes through
        let _ = sent_bytes(&mut rx);
        app.route_client_input(vec![0x1b, b'y']); // M-y no longer chains
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("Previous command was not a yank")
        );
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn empty_kill_ring_yank_reports_and_sends_nothing() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x19]);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Kill ring is empty"));
        assert!(sent_bytes(&mut rx).is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: the five new tests FAIL (`C-y` is currently a no-op in live mode)

- [ ] **Step 3: Add `LastYank` state.** In `src/emacs/mod.rs`:

```rust
/// Bookkeeping for `M-y`: what the immediately preceding live-mode `C-y`
/// typed, and where.
#[derive(Debug, Clone)]
pub struct LastYank {
    pub pane_id: crate::layout::PaneId,
    pub chars: usize,
}
```

Add the field to `EmacsState` (+ `last_yank: None,` in `from_config`, and `self.last_yank = None;` in `apply_config`'s disable block):

```rust
    /// Set by a live-mode yank; cleared by any other key. `M-y` only
    /// chains while this is `Some` (Emacs: "immediately after a yank").
    pub last_yank: Option<LastYank>,
```

- [ ] **Step 4: Implement.** In `src/app/input/emacs.rs`:

Break the yank chain on every other interaction. At the top of `execute_emacs_command` (replacing the current `let _ = prefix;` line):

```rust
        let _ = prefix; // consumed by motions and C-u C-SPC in Phase 4
        if !matches!(cmd, EmacsCommand::Yank | EmacsCommand::YankPop) {
            self.state.emacs.last_yank = None;
        }
```

In `emacs_intercept_key`, in the `Lookup::Unbound` branch where a plain live-mode key returns `false` (passes through to the pane), clear the chain first:

```rust
                } else if seq.len() == 1 {
                    // Plain unbound key in live mode: flows to the pane.
                    self.state.emacs.last_yank = None;
                    false
                } else {
```

Replace the `Yank | YankPop` arm from Task 10 with:

```rust
            EmacsCommand::Yank => {
                if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                } else {
                    self.emacs_yank_live();
                }
            }
            EmacsCommand::YankPop => {
                if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                } else {
                    self.emacs_yank_pop_live();
                }
            }
```

Add the methods to the `impl App` block:

```rust
    /// Live-mode `C-y`: type the kill-ring head into the focused pane —
    /// the way scrollback text is handed to an agent. Syncs from the
    /// system clipboard first when `clipboard_sync` is on.
    fn emacs_yank_live(&mut self) {
        if self.state.emacs.clipboard_sync {
            self.state
                .emacs
                .kill_ring
                .sync_from_system(crate::platform::read_clipboard_text());
        }
        let Some(content) = self.state.emacs.kill_ring.yank() else {
            self.state.emacs.echo = Some("Kill ring is empty".to_string());
            return;
        };
        let Some((ws_idx, pane_id)) = self.emacs_focused_pane() else {
            return;
        };
        if self.emacs_send_text_to_pane(ws_idx, pane_id, &content, 0) {
            self.state.emacs.last_yank = Some(crate::emacs::LastYank {
                pane_id,
                chars: content.chars().count(),
            });
        }
    }

    /// Live-mode `M-y` immediately after a yank: erase the previous yank
    /// with backspaces and type the next-older kill. Known limitation
    /// (accepted): unreliable for multi-line yanks into line editors.
    fn emacs_yank_pop_live(&mut self) {
        let Some(last) = self.state.emacs.last_yank.take() else {
            self.state.emacs.echo =
                Some("Previous command was not a yank".to_string());
            return;
        };
        let Some((ws_idx, pane_id)) = self.emacs_focused_pane() else {
            return;
        };
        if pane_id != last.pane_id {
            self.state.emacs.echo =
                Some("Previous command was not a yank".to_string());
            return;
        }
        let Some(content) = self.state.emacs.kill_ring.yank_pop() else {
            return;
        };
        if self.emacs_send_text_to_pane(ws_idx, pane_id, &content, last.chars) {
            self.state.emacs.last_yank = Some(crate::emacs::LastYank {
                pane_id,
                chars: content.chars().count(),
            });
        }
    }

    /// Type text into a pane's PTY, preceded by `erase` DEL bytes.
    /// Bracketed-paste framing mirrors the `RawInputEvent::Paste` arm of
    /// `route_client_events`.
    fn emacs_send_text_to_pane(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        text: &str,
        erase: usize,
    ) -> bool {
        let Some(rt) = self.state.runtime_for_pane_in_workspace(
            &self.terminal_runtimes,
            ws_idx,
            pane_id,
        ) else {
            return false;
        };
        let mut bytes = vec![0x7f; erase];
        let bracketed = rt
            .input_state()
            .map(|state| state.bracketed_paste)
            .unwrap_or(false);
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(text.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
        } else {
            bytes.extend_from_slice(text.as_bytes());
        }
        rt.try_send_bytes(bytes::Bytes::from(bytes)).is_ok()
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `test result: ok. 23 passed`

- [ ] **Step 6: Manual smoke — the full loop of success criterion 2.** With the Task 7 smoke config, in a Claude Code (or any) pane: `C-x [`, `C-SPC`, `M->`, `M-w`, `q`, then `C-y` — the region text is typed into the prompt; `M-y` immediately after swaps it for the previous kill; copy something with `wl-copy "external"` first and `C-y` yanks that (clipboard-read sync).

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add src/emacs/mod.rs src/app/input/emacs.rs
git commit -m "feat: emacs live-mode yank and yank-pop into pane PTY"
```

### Task 12: Echo-area overlay, `M-g g` goto-line, final verification

**Files:**
- Modify: `src/emacs/render.rs` (`render_echo_area` + render test)
- Modify: `src/ui.rs` (one call in `render_with_runtime_registry` at :403)
- Modify: `src/app/input/emacs.rs` (goto-line prompt + tests)

**Interfaces:**
- Consumes: `EmacsState.echo` / `.pending` (Task 4), `TextModeState.goto_line` (Task 7 — field exists, unused until now), `app.palette.{surface0, text}`, `format_seq` (Task 2).
- Produces: the visible echo area ("Mark set", "Buffer is read-only", pending-chord display, "Goto line: N" prompt) — the surface Phase 3's minibuffer will grow out of; goto-line completes the spec's Phase 1 motion list.

- [ ] **Step 1: Write the failing goto-line tests** — append to `mod tests` in `src/app/input/emacs.rs`:

```rust
    #[tokio::test]
    async fn m_g_g_prompts_and_jumps_to_the_line() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'g', b'g']); // M-g g
        assert_eq!(
            app.state.emacs.text_mode.as_ref().unwrap().goto_line.as_deref(),
            Some("")
        );
        app.route_client_input(b"13".to_vec()); // prompt: "13"
        app.route_client_input(vec![0x7f]); // DEL -> "1"
        app.route_client_input(vec![0x7f]); // DEL -> ""
        app.route_client_input(b"3".to_vec()); // prompt: "3"
        assert_eq!(
            app.state.emacs.text_mode.as_ref().unwrap().goto_line.as_deref(),
            Some("3")
        );
        app.route_client_input(vec![0x0d]); // RET
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!(text.goto_line, None);
        assert_eq!((text.point.row, text.point.col), (2, 0), "line 3, 1-based");
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn c_g_cancels_the_goto_prompt() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'g', b'g']);
        let before = app.state.emacs.text_mode.as_ref().unwrap().point;
        app.route_client_input(vec![b'9', 0x07]); // digit, then C-g
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!(text.goto_line, None);
        assert_eq!(text.point, before, "point untouched");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: the two new tests FAIL — `goto_line` stays `None` (GotoLine command is a no-op, and digits in TEXT mode echo "Buffer is read-only")

- [ ] **Step 3: Implement the prompt.** In `src/app/input/emacs.rs`:

`execute_emacs_command` — replace the `GotoLine` entry in the grouped arm (if it is still grouped, ungroup it) with:

```rust
            EmacsCommand::GotoLine => {
                if let Some(text) = self.state.emacs.text_mode.as_mut() {
                    text.goto_line = Some(String::new());
                }
            }
```

`emacs_intercept_key` — insert directly after the quoted-insert block (before `let text_active = ...`):

```rust
        if self
            .state
            .emacs
            .text_mode
            .as_ref()
            .is_some_and(|text| text.goto_line.is_some())
        {
            return self.emacs_goto_line_key(key);
        }
```

New methods in the `impl App` block:

```rust
    /// Key handling while the `M-g g` digit prompt is open.
    fn emacs_goto_line_key(&mut self, key: TerminalKey) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut jump: Option<(crate::layout::PaneId, u32)> = None;
        {
            let Some(text) = self.state.emacs.text_mode.as_mut() else {
                return false;
            };
            let Some(input) = text.goto_line.as_mut() else {
                return false;
            };
            let plain = !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT);
            match key.code {
                KeyCode::Char(c @ '0'..='9') if plain => input.push(c),
                KeyCode::Backspace if plain => {
                    input.pop();
                }
                KeyCode::Enter => {
                    let line = input.parse::<u32>().ok().filter(|line| *line > 0);
                    text.goto_line = None;
                    if let Some(line) = line {
                        jump = Some((text.pane_id, line));
                    }
                }
                KeyCode::Esc => text.goto_line = None,
                KeyCode::Char('g')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    text.goto_line = None;
                }
                _ => {}
            }
        }
        if let Some((pane_id, line)) = jump {
            self.emacs_goto_line(pane_id, line);
        }
        true
    }

    /// Move point to the start of a 1-based line and follow with the view.
    fn emacs_goto_line(&mut self, pane_id: crate::layout::PaneId, line: u32) {
        let Some((ws_idx, _)) = self.emacs_focused_pane() else {
            return;
        };
        let new_point = {
            let Some(rt) = self.state.runtime_for_pane_in_workspace(
                &self.terminal_runtimes,
                ws_idx,
                pane_id,
            ) else {
                return;
            };
            let buf = RuntimeBuffer { rt };
            text_mode::clamp(&buf, Pos { row: line - 1, col: 0 })
        };
        if let Some(text) = self.state.emacs.text_mode.as_mut() {
            text.point = new_point;
        }
        self.emacs_scroll_point_into_view(pane_id);
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr app::input::emacs`
Expected: `test result: ok. 25 passed`

- [ ] **Step 5: Write the failing echo-render test.** In `src/emacs/render.rs` add a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn bottom_row_text(state: &AppState, area: Rect) -> String {
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_echo_area(state, frame, area))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..area.width)
            .map(|x| {
                buffer[(x, area.height - 1)]
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' ')
            })
            .collect()
    }

    #[test]
    fn echo_area_shows_message_on_the_bottom_row() {
        let mut state = crate::app::AppState::test_new();
        state.emacs.echo = Some("Mark set".to_string());
        let text = bottom_row_text(&state, Rect::new(0, 0, 20, 5));
        assert!(text.starts_with("Mark set"), "{text:?}");
    }

    #[test]
    fn echo_area_shows_pending_chord_and_stays_silent_when_idle() {
        let mut state = crate::app::AppState::test_new();
        let idle = bottom_row_text(&state, Rect::new(0, 0, 20, 5));
        assert_eq!(idle.trim(), "", "no overlay when there is nothing to say");
        state.emacs.pending =
            crate::emacs::keymap::parse_key_seq("C-x").unwrap();
        state.emacs.echo = None;
        let pending = bottom_row_text(&state, Rect::new(0, 0, 20, 5));
        assert!(pending.starts_with("C-x-"), "{pending:?}");
    }
}
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test --locked --bin herdr emacs::render`
Expected: compile error — `render_echo_area` not found

- [ ] **Step 7: Implement the echo area.** In `src/emacs/render.rs`, add to the imports:

```rust
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
```

and add the function:

```rust
/// One-line echo area drawn over the bottom row of the terminal area.
/// herdr has no persistent status line, so this is an overlay that only
/// appears when the layer has something to say (message, pending chord,
/// or the goto-line prompt). Phase 3's minibuffer takes over this surface.
pub fn render_echo_area(app: &AppState, frame: &mut Frame, terminal_area: Rect) {
    if terminal_area.height == 0 || terminal_area.width == 0 {
        return;
    }
    let content = if let Some(prompt) = app
        .emacs
        .text_mode
        .as_ref()
        .and_then(|text| text.goto_line.as_deref())
    {
        format!("Goto line: {prompt}")
    } else if let Some(echo) = app.emacs.echo.as_deref() {
        echo.to_string()
    } else if !app.emacs.pending.is_empty() {
        format!("{}-", crate::emacs::keymap::format_seq(&app.emacs.pending))
    } else {
        return;
    };
    let area = Rect {
        x: terminal_area.x,
        y: terminal_area.y + terminal_area.height - 1,
        width: terminal_area.width,
        height: 1,
    };
    let paragraph = Paragraph::new(Line::from(content))
        .style(Style::default().bg(app.palette.surface0).fg(app.palette.text));
    frame.render_widget(paragraph, area);
}
```

Wire the seam: in `src/ui.rs` `render_with_runtime_registry`, directly after the `render_notifications(app, frame, terminal_area);` line:

```rust
    // Emacs layer seam (fork).
    crate::emacs::render::render_echo_area(app, frame, terminal_area);
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --locked --bin herdr emacs::`
Expected: `test result: ok.` (all `emacs::` module tests: keymap, commands, rings, text_mode, render)

- [ ] **Step 9: Full verification.**

Run: `cargo test --locked --bin herdr`
Expected: `test result: ok.` (~2540 tests, 0 failed)

Run: `cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings`
Expected: no output from fmt; clippy finishes with `Finished` and zero warnings (fix any findings it names — they are typically mechanical: `&String` → `&str`, redundant clones)

Full manual smoke (success criteria 1, 2, 6 of the spec — 3–5 are Phases 2–4):

```bash
HERDR_CONFIG_PATH=/tmp/herdr-emacs-smoke.toml cargo run --locked
```

1. `C-x c` new tab; `C-x n`/`C-x p` cycle tabs; `C-x b` opens the picker; `C-x 2`/`C-x 3` split; `C-x o` cycles panes; `C-x 0` closes; `C-x 1` zooms; `C-x w` workspace picker; `C-q C-x` sends a literal `C-x` to the shell (verify with `cat -v`).
2. In a pane with scrollback: `C-x [`, `C-SPC`, `M->`, `M-w` → `wl-paste` shows the region; `q`, `C-y` types it back; echo area shows "Mark set", "Buffer is read-only" (on `C-y` inside TEXT mode), pending `C-x-`, and `M-g g` digits.
3. Set `enabled = false` in the smoke config, restart: `C-x`, `C-y`, `M-w` all reach the shell untouched; no echo area ever appears.

- [ ] **Step 10: Commit**

```bash
cargo fmt
git add src/emacs/render.rs src/ui.rs src/app/input/emacs.rs
git commit -m "feat: emacs echo area and goto-line prompt"
```

---

## Spec coverage checklist (self-review)

| Spec item (Phases 0–1) | Task |
|---|---|
| `[emacs] enabled` + `clipboard_sync`/`kill_ring_max`/`mark_ring_max` + `[emacs.keys]` | 1 |
| Emacs key syntax parser, keymap stack/dispatch | 2 |
| Command table as the spine, `Option<i64>` prefix-arg calling convention | 3, 4 |
| C-x 2/3/o/0/1/b/c/n/p/k/w, C-x [ | 3 (bindings), 4 + 7 (execution) |
| C-q quoted-insert | 4 |
| Fork guards: no self-update, no background version check | 5 |
| `enabled = false` → stock behavior | 4 (test) + 12 (smoke) |
| TEXT mode entry/exit, read-only buffer, point over scrollback | 6, 7 |
| Motions C-f/b/n/p, M-f/b, C-a/e, C-v/M-v, M-</M->, M-g g | 6, 7, 12 |
| C-SPC transient mark, region visual, C-g deactivate, C-x C-x | 8 |
| Kill ring (depth 60) + mark ring (per-pane, 16); `C-u C-SPC` primitive ready for Phase 4 | 9 |
| M-w / C-w (read-only kill = save), clipboard write sync | 10 |
| Live C-y / M-y into the PTY, clipboard read sync | 11 |
| Echo area ("Mark set", "Buffer is read-only", chord echo) | 4–11 (state) + 12 (render) |
| Sentence motions M-a/M-e | intentionally omitted (spec) |
| `M-y` cycling *inside TEXT mode after C-y* | n/a — C-y is read-only there (spec) |

## Execution notes

- Tasks must run in order; each leaves the tree green (`cargo test --locked --bin herdr` passes) and committed.
- Line numbers reference upstream `3a8490f` (branch `emacs` at plan time). If a rebase moved them, search for the quoted anchor code instead.
- Full first build is ~5 minutes; incremental builds are much faster. Test runs compile the test binary on first invocation.






