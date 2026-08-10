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
pub mod isearch;
pub mod keymap;
pub mod minibuffer;
pub mod open_target;
pub mod render;
pub mod rings;
pub mod text_mode;

use crate::config::EmacsConfig;
use commands::{KeymapSet, MapContext};
use keymap::Chord;

/// Bookkeeping for `M-y`: what the immediately preceding live-mode `C-y`
/// typed, and where.
#[derive(Debug, Clone)]
pub struct LastYank {
    pub pane_id: crate::layout::PaneId,
    pub chars: usize,
}

/// All Emacs-layer state. Lives on `AppState` (pure data, no channels —
/// matching AppState's own contract).
#[derive(Debug)]
pub struct EmacsState {
    pub enabled: bool,
    pub claude_ax_screen_reader: bool,
    pub clipboard_sync: bool,
    pub kill_ring_max: usize,
    pub mark_ring_max: usize,
    pub keymaps: KeymapSet,
    /// Chords accumulated toward a multi-chord binding (after `C-x`, ...).
    pub pending: Vec<Chord>,
    /// True after `C-q`: the next key is sent literally to the pane.
    pub quoted_insert: bool,
    /// Open `M-x` or feedback minibuffer, if any.
    pub minibuffer: Option<minibuffer::MinibufferState>,
    /// Echo-area message; cleared at the start of the next handled key.
    pub echo: Option<String>,
    /// Active TEXT-mode session, if any (`C-x [`). `mode` stays
    /// `Mode::Terminal`; the interception hook owns all keys while `Some`.
    pub text_mode: Option<text_mode::TextModeState>,
    /// The kill ring (shared across panes, like Emacs).
    pub kill_ring: rings::KillRing,
    /// Per-pane mark rings (spec: per-pane, depth `mark_ring_max`).
    pub mark_rings: std::collections::HashMap<crate::layout::PaneId, rings::MarkRing>,
    /// Recent incremental-search queries, newest first.
    pub search_ring: isearch::SearchRing,
    /// Set by a live-mode yank; cleared by any other key. `M-y` only
    /// chains while this is `Some` (Emacs: "immediately after a yank").
    pub last_yank: Option<LastYank>,
    /// Next `recenter-top-bottom` position: middle, top, then bottom.
    /// Any intervening command resets this to middle.
    pub recenter_cycle: u8,
}

impl EmacsState {
    pub fn from_config(config: &EmacsConfig) -> Self {
        // Warnings are NOT logged here: they are surfaced by
        // `EmacsConfig::binding_diagnostics()` through the config
        // diagnostics pipeline (spec §4).
        let (keymaps, _warnings) = commands::build_keymaps(&config.keys);
        Self {
            enabled: config.enabled,
            claude_ax_screen_reader: config.enabled && config.claude_ax_screen_reader,
            clipboard_sync: config.clipboard_sync,
            kill_ring_max: config.kill_ring_max.max(1),
            mark_ring_max: config.mark_ring_max.max(1),
            keymaps,
            pending: Vec::new(),
            quoted_insert: false,
            minibuffer: None,
            echo: None,
            text_mode: None,
            kill_ring: rings::KillRing::new(config.kill_ring_max.max(1)),
            mark_rings: std::collections::HashMap::new(),
            search_ring: isearch::SearchRing::new(32),
            last_yank: None,
            recenter_cycle: 0,
        }
    }

    /// Live config reload: refresh config-derived fields, preserve runtime
    /// state (rings survive a reload); drop transient state when disabling.
    /// Returns binding warnings for the caller's diagnostics vec.
    pub fn apply_config(&mut self, config: &EmacsConfig) -> Vec<String> {
        let (keymaps, warnings) = commands::build_keymaps(&config.keys);
        self.enabled = config.enabled;
        self.claude_ax_screen_reader = config.enabled && config.claude_ax_screen_reader;
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
            self.minibuffer = None;
            self.echo = None;
            self.text_mode = None;
            self.last_yank = None;
            self.recenter_cycle = 0;
        }
        warnings
    }

    /// Which keymap stack is active right now (spec §3.1).
    pub fn map_context(&self) -> MapContext {
        if self.minibuffer.is_some() {
            MapContext::Minibuffer
        } else if self
            .text_mode
            .as_ref()
            .is_some_and(|text| text.isearch.is_some())
        {
            MapContext::Isearch
        } else if self.text_mode.is_some() {
            MapContext::Text
        } else {
            MapContext::Live
        }
    }

    /// True when TEXT mode owns the cursor for this pane (suppresses the
    /// host cursor in the pane renderer).
    pub fn owns_pane_cursor(&self, pane_id: crate::layout::PaneId) -> bool {
        self.text_mode
            .as_ref()
            .is_some_and(|text| text.pane_id == pane_id)
    }

    /// True while Emacs draws presentation state over the terminal surface.
    /// Retained PTY patches cannot safely bypass these overlays because they
    /// would repaint terminal cells without reapplying the Emacs layer.
    pub fn has_render_overlay(&self) -> bool {
        self.text_mode.is_some()
            || self.minibuffer.is_some()
            || self.echo.is_some()
            || !self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_ax_screen_reader_requires_enabled_emacs_layer() {
        let disabled = EmacsState::from_config(&EmacsConfig {
            claude_ax_screen_reader: true,
            ..Default::default()
        });
        assert!(!disabled.claude_ax_screen_reader);

        let enabled = EmacsState::from_config(&EmacsConfig {
            enabled: true,
            claude_ax_screen_reader: true,
            ..Default::default()
        });
        assert!(enabled.claude_ax_screen_reader);
    }

    #[test]
    fn apply_config_trims_existing_pane_mark_rings() {
        let mut state = EmacsState::from_config(&EmacsConfig {
            enabled: true,
            mark_ring_max: 10,
            ..Default::default()
        });
        let pane_id = crate::layout::PaneId::alloc();
        let ring = state
            .mark_rings
            .entry(pane_id)
            .or_insert_with(|| rings::MarkRing::new(10));
        for i in 0..5 {
            ring.push((i, 0));
        }
        assert_eq!(state.mark_rings[&pane_id].len(), 5);

        let _ = state.apply_config(&EmacsConfig {
            enabled: true,
            mark_ring_max: 2,
            ..Default::default()
        });

        assert_eq!(
            state.mark_rings[&pane_id].len(),
            2,
            "existing pane mark ring shrinks on live reload"
        );
    }
}
