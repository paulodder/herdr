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
                    self.state.emacs.echo = Some(format!("{} is undefined", format_seq(&seq)));
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
            Some(chord) => !matches!(emacs.keymaps.global.lookup(&[chord]), Lookup::Unbound),
            None => false,
        }
    }

    /// Execute a named command. `prefix` is the universal-argument slot:
    /// always `None` until Phase 4 wires `C-u`, but part of the calling
    /// convention from day one (spec: "the command table is the spine").
    pub(crate) fn execute_emacs_command(&mut self, cmd: EmacsCommand, prefix: Option<i64>) {
        let _ = prefix; // consumed by motions and C-u C-SPC in Phase 4
        match cmd {
            EmacsCommand::SplitWindowBelow => self.emacs_navigate(NavigateAction::SplitHorizontal),
            EmacsCommand::SplitWindowRight => self.emacs_navigate(NavigateAction::SplitVertical),
            EmacsCommand::OtherWindow => self.emacs_navigate(NavigateAction::CyclePaneNext),
            EmacsCommand::DeleteWindow => self.emacs_navigate(NavigateAction::ClosePane),
            EmacsCommand::DeleteOtherWindows => self.emacs_navigate(NavigateAction::Zoom),
            EmacsCommand::SwitchToBuffer => self.emacs_navigate(NavigateAction::OpenNavigator),
            EmacsCommand::NewTab => self.emacs_navigate(NavigateAction::NewTab),
            EmacsCommand::NextTab => self.emacs_navigate(NavigateAction::NextTab),
            EmacsCommand::PreviousTab => self.emacs_navigate(NavigateAction::PreviousTab),
            EmacsCommand::KillTab => self.emacs_navigate(NavigateAction::CloseTab),
            EmacsCommand::WorkspacePicker => self.emacs_navigate(NavigateAction::WorkspacePicker),
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
        let Some(rt) =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
        else {
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
        assert!(
            sent_bytes(&mut rx).is_empty(),
            "chord must not reach the pane"
        );
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
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-x z is undefined"));
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
        app.state
            .emacs
            .apply_config(&crate::config::EmacsConfig::default());
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
