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

pub mod commands;
pub mod keymap;
pub mod render;
pub mod rings;
pub mod text_mode;

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
    /// Active TEXT-mode session, if any (`C-x [`). `mode` stays
    /// `Mode::Terminal`; the interception hook owns all keys while `Some`.
    pub text_mode: Option<text_mode::TextModeState>,
    /// The kill ring (shared across panes, like Emacs).
    pub kill_ring: rings::KillRing,
    /// Per-pane mark rings (spec: per-pane, depth `mark_ring_max`).
    pub mark_rings: std::collections::HashMap<crate::layout::PaneId, rings::MarkRing>,
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
            text_mode: None,
            kill_ring: rings::KillRing::new(config.kill_ring_max.max(1)),
            mark_rings: std::collections::HashMap::new(),
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
        self.kill_ring.set_max(config.kill_ring_max.max(1));
        self.keymaps = keymaps;
        if !self.enabled {
            self.pending.clear();
            self.quoted_insert = false;
            self.echo = None;
            self.text_mode = None;
        }
    }

    /// True when TEXT mode owns the cursor for this pane (suppresses the
    /// host cursor in the pane renderer).
    pub fn owns_pane_cursor(&self, pane_id: crate::layout::PaneId) -> bool {
        self.text_mode
            .as_ref()
            .is_some_and(|text| text.pane_id == pane_id)
    }
}
