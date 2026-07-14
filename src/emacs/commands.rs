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
    (
        EmacsCommand::ExchangePointAndMark,
        "exchange-point-and-mark",
    ),
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
    /// KeyboardQuit is text-side: its only default binding (C-g) lives in
    /// the text keymap, so overrides must land there too.
    fn is_text_command(self) -> bool {
        matches!(
            self,
            Self::KeyboardQuit
                | Self::ExitTextMode
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
    fn keyboard_quit_override_lands_in_text_keymap() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("C-]".to_string(), "keyboard-quit".to_string());
        let (keymaps, warnings) = build_keymaps(&overrides);
        assert!(warnings.is_empty());
        assert_eq!(
            keymaps.text.lookup(&parse_key_seq("C-]").unwrap()),
            Lookup::Bound(EmacsCommand::KeyboardQuit)
        );
        assert_eq!(
            keymaps.global.lookup(&parse_key_seq("C-]").unwrap()),
            Lookup::Unbound
        );
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
