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
        !self.ctrl && !self.meta && matches!(self.code, KeyCode::Char(c) if !c.is_control())
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

/// Parse one chord in Emacs notation: optional `C-`/`M-` prefixes followed
/// by a single character or a named key (`SPC`, `RET`, `TAB`, `ESC`, `DEL`).
pub fn parse_chord(s: &str) -> Option<Chord> {
    let mut ctrl = false;
    let mut meta = false;
    let mut rest = s;
    loop {
        if let Some(r) = rest.strip_prefix("C-") {
            if r.is_empty() || ctrl {
                return None;
            }
            ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("M-") {
            if r.is_empty() || meta {
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
    // Same boundary as `Chord::from_key`, so a config binding and a decoded
    // key can never disagree about what a chord is (spec §3.2). `C-i` and
    // `TAB` remain DIFFERENT chords: `C-i` fires only on a kitty terminal.
    canonical_chord(ctrl, meta, named_key(rest)?)
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

    /// All bindings in insertion order. Used by `describe-bindings`.
    pub fn bindings(&self) -> &[(Vec<Chord>, T)] {
        &self.bindings
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::parse_terminal_key_sequence;
    use crossterm::event::{KeyCode, KeyModifiers};

    /// The one true assertion of §3.2: whatever the terminal sends for a
    /// key, `Chord::from_key` must produce the same chord that
    /// `parse_chord` produces for that key's Emacs name.
    fn assert_encodes_to(encoding: &str, chord_name: &str) {
        let key = parse_terminal_key_sequence(encoding)
            .unwrap_or_else(|| panic!("{encoding:?} must decode to a key"));
        let expected = parse_chord(chord_name).unwrap_or_else(|| panic!("{chord_name} must parse"));
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
            "C-x", "M-x", "C-SPC", "TAB", "RET", "ESC", "DEL", "M-DEL", "C-h", "C-i", "C-m", "C-[",
            "C-]", "M-[", "M-]", "F1", "M-<", "3",
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

    #[test]
    fn parses_single_chords() {
        assert_eq!(
            parse_chord("C-x"),
            Some(Chord {
                ctrl: true,
                meta: false,
                code: KeyCode::Char('x')
            })
        );
        assert_eq!(
            parse_chord("M-<"),
            Some(Chord {
                ctrl: false,
                meta: true,
                code: KeyCode::Char('<')
            })
        );
        assert_eq!(
            parse_chord("C-SPC"),
            Some(Chord {
                ctrl: true,
                meta: false,
                code: KeyCode::Char(' ')
            })
        );
        assert_eq!(
            parse_chord("RET"),
            Some(Chord {
                ctrl: false,
                meta: false,
                code: KeyCode::Enter
            })
        );
        assert_eq!(
            parse_chord("["),
            Some(Chord {
                ctrl: false,
                meta: false,
                code: KeyCode::Char('[')
            })
        );
        assert_eq!(parse_chord(""), None);
        assert_eq!(parse_chord("C-"), None);
        assert_eq!(parse_chord("xy"), None);
        // repeated modifiers must be rejected
        assert_eq!(parse_chord("C-C-x"), None);
        assert_eq!(parse_chord("M-M-x"), None);
        // named keys
        assert_eq!(
            parse_chord("TAB"),
            Some(Chord {
                ctrl: false,
                meta: false,
                code: KeyCode::Tab
            })
        );
        assert_eq!(
            parse_chord("ESC"),
            Some(Chord {
                ctrl: false,
                meta: false,
                code: KeyCode::Esc
            })
        );
        assert_eq!(
            parse_chord("DEL"),
            Some(Chord {
                ctrl: false,
                meta: false,
                code: KeyCode::Backspace
            })
        );
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
        let key = TerminalKey::new(KeyCode::Char('<'), KeyModifiers::ALT | KeyModifiers::SHIFT);
        assert_eq!(Chord::from_key(&key), parse_chord("M-<"));
        let key = TerminalKey::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(Chord::from_key(&key), parse_chord("C-x"));
        let key = TerminalKey::new(
            KeyCode::Media(crossterm::event::MediaKeyCode::Play),
            KeyModifiers::empty(),
        );
        assert_eq!(Chord::from_key(&key), None);
    }

    #[test]
    fn keymap_lookup_distinguishes_bound_prefix_unbound() {
        let mut map: Keymap<u8> = Keymap::default();
        map.bind(parse_key_seq("C-x 2").unwrap(), 1);
        map.bind(parse_key_seq("C-q").unwrap(), 2);
        assert_eq!(
            map.lookup(&parse_key_seq("C-x 2").unwrap()),
            Lookup::Bound(1)
        );
        assert_eq!(map.lookup(&parse_key_seq("C-x").unwrap()), Lookup::Prefix);
        assert_eq!(map.lookup(&parse_key_seq("C-q").unwrap()), Lookup::Bound(2));
        assert_eq!(map.lookup(&parse_key_seq("C-z").unwrap()), Lookup::Unbound);
        assert_eq!(
            map.lookup(&parse_key_seq("C-x 3").unwrap()),
            Lookup::Unbound
        );
        // rebinding replaces
        map.bind(parse_key_seq("C-x 2").unwrap(), 9);
        assert_eq!(
            map.lookup(&parse_key_seq("C-x 2").unwrap()),
            Lookup::Bound(9)
        );
    }

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
            stack_lookup(
                [&shadow, &global].into_iter(),
                &parse_key_seq("C-x").unwrap()
            ),
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
}
