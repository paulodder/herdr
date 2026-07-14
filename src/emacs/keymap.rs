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
    Some(Chord {
        ctrl,
        meta,
        code: named_key(rest)?,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

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
}
