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
