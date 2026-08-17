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
pub const NAVIGATE_ACTION_COUNT: usize = 46;

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
        ResetFederationConnections => "reset-federation-connections",
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
    Feedback,
    HerdrOnboarding,
    InterruptProcess,
    KeyboardQuit,
    OpenAtPoint,
    QuotedInsert,
    RefreshHerdr,
    // Tab reordering (spec §3.8): herdr has a tab.move API but no
    // NavigateAction for it, so these are builtins.
    MoveTabLeft,
    MoveTabRight,
    // TEXT mode
    TextMode,
    ExitTextMode,
    ForwardChar,
    BackwardChar,
    NextLine,
    PreviousLine,
    ForwardWord,
    BackwardWord,
    ForwardSexp,
    BackwardSexp,
    MoveBeginningOfLine,
    MoveEndOfLine,
    RecenterTopBottom,
    ScrollUp,
    ScrollDown,
    BeginningOfBuffer,
    EndOfBuffer,
    GotoLine,
    IsearchForward,
    IsearchBackward,
    SetMark,
    ExchangePointAndMark,
    // Rings
    KillRingSave,
    KillRegion,
    Yank,
    YankPop,
    // Minibuffer editing
    DeleteBackwardChar,
    DeleteForwardChar,
    KillBeginningOfLine,
    KillLine,
    BackwardKillWord,
    MarkWholeInput,
    ExitMinibuffer,
    // Incremental search editing
    IsearchExit,
    IsearchDeleteChar,
    IsearchPreviousHistory,
    IsearchNextHistory,
    // Help
    DescribeKey,
    DescribeBindings,
}

/// Canonical builtin name table (part of the M-x namespace). Keep sorted.
pub const BUILTIN_NAMES: &[(EmacsBuiltin, &str)] = &[
    (EmacsBuiltin::BackwardChar, "backward-char"),
    (EmacsBuiltin::BackwardKillWord, "backward-kill-word"),
    (EmacsBuiltin::BackwardSexp, "backward-sexp"),
    (EmacsBuiltin::BackwardWord, "backward-word"),
    (EmacsBuiltin::BeginningOfBuffer, "beginning-of-buffer"),
    (EmacsBuiltin::DeleteBackwardChar, "delete-backward-char"),
    (EmacsBuiltin::DeleteForwardChar, "delete-forward-char"),
    (EmacsBuiltin::DescribeBindings, "describe-bindings"),
    (EmacsBuiltin::DescribeKey, "describe-key"),
    (EmacsBuiltin::EndOfBuffer, "end-of-buffer"),
    (
        EmacsBuiltin::ExchangePointAndMark,
        "exchange-point-and-mark",
    ),
    (
        EmacsBuiltin::ExecuteExtendedCommand,
        "execute-extended-command",
    ),
    (EmacsBuiltin::ExitMinibuffer, "exit-minibuffer"),
    (EmacsBuiltin::ExitTextMode, "exit-text-mode"),
    (EmacsBuiltin::Feedback, "feedback"),
    (EmacsBuiltin::ForwardChar, "forward-char"),
    (EmacsBuiltin::ForwardSexp, "forward-sexp"),
    (EmacsBuiltin::ForwardWord, "forward-word"),
    (EmacsBuiltin::GotoLine, "goto-line"),
    (EmacsBuiltin::HerdrOnboarding, "herdr-onboarding"),
    (EmacsBuiltin::InterruptProcess, "interrupt-process"),
    (EmacsBuiltin::IsearchBackward, "isearch-backward"),
    (EmacsBuiltin::IsearchDeleteChar, "isearch-delete-char"),
    (EmacsBuiltin::IsearchExit, "isearch-exit"),
    (EmacsBuiltin::IsearchForward, "isearch-forward"),
    (EmacsBuiltin::IsearchNextHistory, "isearch-next-history"),
    (
        EmacsBuiltin::IsearchPreviousHistory,
        "isearch-previous-history",
    ),
    (EmacsBuiltin::KeyboardQuit, "keyboard-quit"),
    (EmacsBuiltin::KillBeginningOfLine, "kill-beginning-of-line"),
    (EmacsBuiltin::KillLine, "kill-line"),
    (EmacsBuiltin::KillRegion, "kill-region"),
    (EmacsBuiltin::KillRingSave, "kill-ring-save"),
    (EmacsBuiltin::MarkWholeInput, "mark-whole-input"),
    (EmacsBuiltin::MoveTabLeft, "move-tab-left"),
    (EmacsBuiltin::MoveTabRight, "move-tab-right"),
    (EmacsBuiltin::MoveBeginningOfLine, "move-beginning-of-line"),
    (EmacsBuiltin::MoveEndOfLine, "move-end-of-line"),
    (EmacsBuiltin::NextLine, "next-line"),
    (EmacsBuiltin::OpenAtPoint, "open-at-point"),
    (EmacsBuiltin::PreviousLine, "previous-line"),
    (EmacsBuiltin::QuotedInsert, "quoted-insert"),
    (EmacsBuiltin::RecenterTopBottom, "recenter-top-bottom"),
    (EmacsBuiltin::RefreshHerdr, "refresh-herdr"),
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
    /// Incremental-search editing only.
    Isearch,
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
            | Self::Feedback
            | Self::HerdrOnboarding
            | Self::InterruptProcess
            | Self::KeyboardQuit
            | Self::OpenAtPoint
            | Self::QuotedInsert
            | Self::RefreshHerdr
            | Self::MoveTabLeft
            | Self::MoveTabRight
            | Self::TextMode
            | Self::IsearchForward
            | Self::IsearchBackward => MapSlot::Global,
            Self::ExitTextMode
            | Self::ForwardChar
            | Self::BackwardChar
            | Self::NextLine
            | Self::PreviousLine
            | Self::ForwardWord
            | Self::BackwardWord
            | Self::ForwardSexp
            | Self::BackwardSexp
            | Self::MoveBeginningOfLine
            | Self::MoveEndOfLine
            | Self::RecenterTopBottom
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
            | Self::DeleteForwardChar
            | Self::KillBeginningOfLine
            | Self::KillLine
            | Self::BackwardKillWord
            | Self::MarkWholeInput
            | Self::ExitMinibuffer => MapSlot::Minibuffer,
            Self::IsearchExit
            | Self::IsearchDeleteChar
            | Self::IsearchPreviousHistory
            | Self::IsearchNextHistory => MapSlot::Isearch,
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
    /// Incremental search is reading a query over a TEXT-mode buffer.
    Isearch,
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
    /// Active while incremental search is reading a query (shadows `global`).
    pub isearch: Keymap<EmacsCommand>,
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
            MapContext::Isearch => vec![
                ActiveMap {
                    name: "isearch",
                    map: &self.isearch,
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
    (
        "C-x 2",
        EmacsCommand::Herdr(NavigateAction::SplitHorizontal),
    ),
    ("C-x 3", EmacsCommand::Herdr(NavigateAction::SplitVertical)),
    ("C-x o", EmacsCommand::Herdr(NavigateAction::CyclePaneNext)),
    ("C-x 0", EmacsCommand::Herdr(NavigateAction::ClosePane)),
    ("C-x 1", EmacsCommand::Herdr(NavigateAction::Zoom)),
    ("C-x b", EmacsCommand::Herdr(NavigateAction::OpenNavigator)),
    ("C-x C-f", EmacsCommand::Herdr(NavigateAction::NewWorkspace)),
    ("C-x c", EmacsCommand::Herdr(NavigateAction::NewTab)),
    ("C-x n", EmacsCommand::Herdr(NavigateAction::NextTab)),
    ("C-x p", EmacsCommand::Herdr(NavigateAction::PreviousTab)),
    ("C-x k", EmacsCommand::Herdr(NavigateAction::CloseTab)),
    ("C-x t", EmacsCommand::Herdr(NavigateAction::RenameTab)),
    ("C-c t", EmacsCommand::Herdr(NavigateAction::RenameTab)),
    (
        "C-c w",
        EmacsCommand::Herdr(NavigateAction::RenameWorkspace),
    ),
    ("M-n", EmacsCommand::Herdr(NavigateAction::NextAgent)),
    ("M-p", EmacsCommand::Herdr(NavigateAction::PreviousAgent)),
    ("C-M-n", EmacsCommand::Herdr(NavigateAction::NextWorkspace)),
    ("C-M-o", EmacsCommand::Builtin(EmacsBuiltin::OpenAtPoint)),
    (
        "C-M-p",
        EmacsCommand::Herdr(NavigateAction::PreviousWorkspace),
    ),
    (
        "C-x w",
        EmacsCommand::Herdr(NavigateAction::WorkspacePicker),
    ),
    // Spec §3.8. Kitty-protocol only: on a legacy terminal C-[ is byte 27
    // (ESC) and M-[ is the CSI introducer, so these never fire there.
    ("C-[", EmacsCommand::Herdr(NavigateAction::PreviousTab)),
    ("C-]", EmacsCommand::Herdr(NavigateAction::NextTab)),
    ("M-[", EmacsCommand::Builtin(EmacsBuiltin::MoveTabLeft)),
    ("M-]", EmacsCommand::Builtin(EmacsBuiltin::MoveTabRight)),
    ("C-x [", EmacsCommand::Builtin(EmacsBuiltin::TextMode)),
    (
        "C-c C-c",
        EmacsCommand::Builtin(EmacsBuiltin::InterruptProcess),
    ),
    ("C-q", EmacsCommand::Builtin(EmacsBuiltin::QuotedInsert)),
    ("C-g", EmacsCommand::Builtin(EmacsBuiltin::KeyboardQuit)),
    (
        "C-x ?",
        EmacsCommand::Builtin(EmacsBuiltin::DescribeBindings),
    ),
    (
        "M-x",
        EmacsCommand::Builtin(EmacsBuiltin::ExecuteExtendedCommand),
    ),
    ("C-y", EmacsCommand::Builtin(EmacsBuiltin::Yank)),
    ("M-y", EmacsCommand::Builtin(EmacsBuiltin::YankPop)),
    ("C-s", EmacsCommand::Builtin(EmacsBuiltin::IsearchForward)),
    ("C-r", EmacsCommand::Builtin(EmacsBuiltin::IsearchBackward)),
];

const DEFAULT_TEXT_BINDINGS: &[(&str, EmacsCommand)] = &[
    ("C-f", EmacsCommand::Builtin(EmacsBuiltin::ForwardChar)),
    ("C-b", EmacsCommand::Builtin(EmacsBuiltin::BackwardChar)),
    ("C-n", EmacsCommand::Builtin(EmacsBuiltin::NextLine)),
    ("C-p", EmacsCommand::Builtin(EmacsBuiltin::PreviousLine)),
    ("M-f", EmacsCommand::Builtin(EmacsBuiltin::ForwardWord)),
    ("M-b", EmacsCommand::Builtin(EmacsBuiltin::BackwardWord)),
    ("C-M-f", EmacsCommand::Builtin(EmacsBuiltin::ForwardSexp)),
    ("C-M-b", EmacsCommand::Builtin(EmacsBuiltin::BackwardSexp)),
    (
        "C-a",
        EmacsCommand::Builtin(EmacsBuiltin::MoveBeginningOfLine),
    ),
    ("C-e", EmacsCommand::Builtin(EmacsBuiltin::MoveEndOfLine)),
    (
        "C-l",
        EmacsCommand::Builtin(EmacsBuiltin::RecenterTopBottom),
    ),
    ("C-v", EmacsCommand::Builtin(EmacsBuiltin::ScrollUp)),
    ("M-v", EmacsCommand::Builtin(EmacsBuiltin::ScrollDown)),
    (
        "M-<",
        EmacsCommand::Builtin(EmacsBuiltin::BeginningOfBuffer),
    ),
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

/// The minibuffer's local map. `C-g` lives in the global map and reaches the
/// minibuffer by fallthrough.
const DEFAULT_MINIBUFFER_BINDINGS: &[(&str, EmacsCommand)] = &[
    ("RET", EmacsCommand::Builtin(EmacsBuiltin::ExitMinibuffer)),
    (
        "DEL",
        EmacsCommand::Builtin(EmacsBuiltin::DeleteBackwardChar),
    ),
    ("C-k", EmacsCommand::Builtin(EmacsBuiltin::KillLine)),
    (
        "C-u",
        EmacsCommand::Builtin(EmacsBuiltin::KillBeginningOfLine),
    ),
    (
        "C-d",
        EmacsCommand::Builtin(EmacsBuiltin::DeleteForwardChar),
    ),
    ("C-w", EmacsCommand::Builtin(EmacsBuiltin::BackwardKillWord)),
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
    ("M-f", EmacsCommand::Builtin(EmacsBuiltin::ForwardWord)),
    ("M-b", EmacsCommand::Builtin(EmacsBuiltin::BackwardWord)),
    ("M-h", EmacsCommand::Builtin(EmacsBuiltin::MarkWholeInput)),
    ("C-y", EmacsCommand::Builtin(EmacsBuiltin::Yank)),
];

const DEFAULT_ISEARCH_BINDINGS: &[(&str, EmacsCommand)] = &[
    ("RET", EmacsCommand::Builtin(EmacsBuiltin::IsearchExit)),
    ("ESC", EmacsCommand::Builtin(EmacsBuiltin::IsearchExit)),
    (
        "DEL",
        EmacsCommand::Builtin(EmacsBuiltin::IsearchDeleteChar),
    ),
    (
        "M-p",
        EmacsCommand::Builtin(EmacsBuiltin::IsearchPreviousHistory),
    ),
    (
        "M-n",
        EmacsCommand::Builtin(EmacsBuiltin::IsearchNextHistory),
    ),
];

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
    for (seq, cmd) in DEFAULT_ISEARCH_BINDINGS {
        set.isearch.bind(
            parse_key_seq(seq).expect("default isearch binding parses"),
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
            MapSlot::Isearch => set.isearch.bind(seq, cmd),
            MapSlot::Both => {
                set.global.bind(seq.clone(), cmd);
                set.text.bind(seq, cmd);
            }
        }
    }
    (set, warnings)
}

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
            assert_eq!(
                EmacsCommand::from_name(name),
                Some(EmacsCommand::Herdr(action))
            );
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
            (
                "open-navigator-notification-target",
                NavigateAction::OpenNotificationTarget,
            ),
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
        assert_eq!(
            herdr_command_name(NavigateAction::SwitchTab(7)),
            "switch-tab"
        );
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
            ("C-x C-f", herdr(NavigateAction::NewWorkspace)),
            ("C-x c", herdr(NavigateAction::NewTab)),
            ("C-x n", herdr(NavigateAction::NextTab)),
            ("C-x p", herdr(NavigateAction::PreviousTab)),
            ("C-x k", herdr(NavigateAction::CloseTab)),
            ("C-x t", herdr(NavigateAction::RenameTab)),
            ("C-c t", herdr(NavigateAction::RenameTab)),
            ("C-c w", herdr(NavigateAction::RenameWorkspace)),
            ("C-c C-c", builtin(EmacsBuiltin::InterruptProcess)),
            ("M-n", herdr(NavigateAction::NextAgent)),
            ("M-p", herdr(NavigateAction::PreviousAgent)),
            ("C-M-n", herdr(NavigateAction::NextWorkspace)),
            ("C-M-o", builtin(EmacsBuiltin::OpenAtPoint)),
            ("C-M-p", herdr(NavigateAction::PreviousWorkspace)),
            ("C-x w", herdr(NavigateAction::WorkspacePicker)),
            ("C-x [", builtin(EmacsBuiltin::TextMode)),
            ("C-q", builtin(EmacsBuiltin::QuotedInsert)),
            ("C-g", builtin(EmacsBuiltin::KeyboardQuit)),
            ("M-x", builtin(EmacsBuiltin::ExecuteExtendedCommand)),
            ("C-x ?", builtin(EmacsBuiltin::DescribeBindings)),
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
    fn minibuffer_map_binds_editing_and_submission() {
        let (keymaps, _) = build_keymaps(&Default::default());
        for (seq, command) in [
            ("RET", EmacsBuiltin::ExitMinibuffer),
            ("DEL", EmacsBuiltin::DeleteBackwardChar),
            ("C-k", EmacsBuiltin::KillLine),
            ("C-w", EmacsBuiltin::BackwardKillWord),
            ("C-a", EmacsBuiltin::MoveBeginningOfLine),
            ("C-e", EmacsBuiltin::MoveEndOfLine),
        ] {
            assert_eq!(
                keymaps.lookup(MapContext::Minibuffer, &parse_key_seq(seq).unwrap()),
                Lookup::Bound(builtin(command)),
                "minibuffer {seq}"
            );
        }
        assert_eq!(
            EmacsCommand::from_name("feedback"),
            Some(builtin(EmacsBuiltin::Feedback))
        );
        assert_eq!(
            EmacsCommand::from_name("herdr-onboarding"),
            Some(builtin(EmacsBuiltin::HerdrOnboarding))
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
            ("C-M-f", EmacsBuiltin::ForwardSexp),
            ("C-M-b", EmacsBuiltin::BackwardSexp),
            ("C-a", EmacsBuiltin::MoveBeginningOfLine),
            ("C-e", EmacsBuiltin::MoveEndOfLine),
            ("C-l", EmacsBuiltin::RecenterTopBottom),
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

    #[test]
    fn incremental_search_has_global_entry_and_a_local_keymap() {
        let (keymaps, _) = build_keymaps(&Default::default());
        assert_eq!(
            keymaps.lookup(MapContext::Live, &parse_key_seq("C-s").unwrap()),
            Lookup::Bound(EmacsCommand::Builtin(EmacsBuiltin::IsearchForward))
        );
        assert_eq!(
            keymaps.lookup(MapContext::Live, &parse_key_seq("C-r").unwrap()),
            Lookup::Bound(EmacsCommand::Builtin(EmacsBuiltin::IsearchBackward))
        );
        assert_eq!(
            keymaps.lookup(MapContext::Isearch, &parse_key_seq("RET").unwrap()),
            Lookup::Bound(EmacsCommand::Builtin(EmacsBuiltin::IsearchExit))
        );
        assert_eq!(
            keymaps.lookup(MapContext::Isearch, &parse_key_seq("DEL").unwrap()),
            Lookup::Bound(EmacsCommand::Builtin(EmacsBuiltin::IsearchDeleteChar))
        );
        assert_eq!(
            keymaps
                .active_maps(MapContext::Isearch)
                .into_iter()
                .map(|active| active.name)
                .collect::<Vec<_>>(),
            vec!["isearch", "global"]
        );
    }
}
