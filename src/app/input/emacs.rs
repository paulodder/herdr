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
use crate::emacs::text_mode::{self, Pos, TextBuffer, TextModeState};
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

        self.state.emacs.echo = None;

        if self.state.emacs.quoted_insert {
            self.state.emacs.quoted_insert = false;
            self.emacs_send_key_to_focused_pane(key);
            return true;
        }

        if self
            .state
            .emacs
            .text_mode
            .as_ref()
            .is_some_and(|text| text.goto_line.is_some())
        {
            return self.emacs_goto_line_key(key);
        }

        let text_active = self.state.emacs.text_mode.is_some();

        let Some(chord) = Chord::from_key(&key) else {
            return text_active;
        };

        // C-g always cancels an in-flight chord (and, in TEXT mode,
        // deactivates the mark — Task 8). Delegates to KeyboardQuit so
        // mid-chord quit and bound quit behave identically.
        if !self.state.emacs.pending.is_empty() && chord == Chord::ctrl('g') {
            self.execute_emacs_command(EmacsCommand::KeyboardQuit, None);
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
                    self.state.emacs.last_yank = None;
                    false
                } else {
                    self.state.emacs.last_yank = None;
                    self.state.emacs.echo = Some(format!("{} is undefined", format_seq(&seq)));
                    true
                }
            }
        }
    }

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
            Some(chord) => !matches!(emacs.keymaps.global.lookup(&[chord]), Lookup::Unbound),
            None => false,
        }
    }

    /// Execute a named command. `prefix` is the universal-argument slot:
    /// always `None` until Phase 4 wires `C-u`, but part of the calling
    /// convention from day one (spec: "the command table is the spine").
    pub(crate) fn execute_emacs_command(&mut self, cmd: EmacsCommand, prefix: Option<i64>) {
        let _ = prefix; // consumed by motions and C-u C-SPC in Phase 4
        if !matches!(cmd, EmacsCommand::Yank | EmacsCommand::YankPop) {
            self.state.emacs.last_yank = None;
        }
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
                if let Some(text) = self.state.emacs.text_mode.as_mut() {
                    text.mark_active = false;
                }
                self.state.emacs.echo = Some("Quit".to_string());
            }
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
            EmacsCommand::SetMark => self.emacs_set_mark(),
            EmacsCommand::ExchangePointAndMark => self.emacs_exchange_point_and_mark(),
            // In a read-only buffer C-w cannot delete, so kill-region
            // degrades to kill-ring-save (spec §Phase 1).
            EmacsCommand::KillRingSave | EmacsCommand::KillRegion => self.emacs_kill_ring_save(),
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
            EmacsCommand::GotoLine => {
                if let Some(text) = self.state.emacs.text_mode.as_mut() {
                    text.goto_line = Some(String::new());
                }
            }
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
        let viewport_top = (metrics.max_offset_from_bottom - metrics.offset_from_bottom) as u32;
        let point = {
            let Some(rt) =
                self.state
                    .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            else {
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
                Pos {
                    row: viewport_top + u32::from(row_in_view),
                    col,
                },
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
        let page = self.state.pane_info_by_id(pane_id).map_or(1, |info| {
            u32::from(info.inner_rect.height.saturating_sub(2).max(1))
        });
        let new_point = {
            let Some(rt) =
                self.state
                    .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            else {
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
                    Pos {
                        row: point.row.saturating_add(page),
                        col: point.col,
                    },
                ),
                EmacsCommand::ScrollDown => text_mode::clamp(
                    &buf,
                    Pos {
                        row: point.row.saturating_sub(page),
                        col: point.col,
                    },
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
                KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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
            let Some(rt) =
                self.state
                    .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            else {
                return;
            };
            let buf = RuntimeBuffer { rt };
            text_mode::clamp(
                &buf,
                Pos {
                    row: line - 1,
                    col: 0,
                },
            )
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
        let (start, end) = if mark <= point {
            (mark, point)
        } else {
            (point, mark)
        };
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
        let rt =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;
        let (_, cols) = rt.text_dims()?;
        let end_inclusive = if end.col > 0 {
            (end.col - 1, end.row)
        } else {
            (cols.saturating_sub(1), end.row.checked_sub(1)?)
        };
        rt.read_text_range((start.col, start.row), end_inclusive)
    }

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
            self.state.emacs.echo = Some("Previous command was not a yank".to_string());
            return;
        };
        let Some((ws_idx, pane_id)) = self.emacs_focused_pane() else {
            return;
        };
        if pane_id != last.pane_id {
            self.state.emacs.echo = Some("Previous command was not a yank".to_string());
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
        let Some(rt) =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
        else {
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
}

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
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Buffer is read-only"));
        assert!(sent_bytes(&mut rx).is_empty());
        assert!(app.state.emacs.text_mode.is_some(), "still in TEXT mode");
    }

    #[tokio::test]
    async fn owns_pane_cursor_only_for_the_text_mode_pane() {
        let (mut app, pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        assert!(!app.state.emacs.owns_pane_cursor(pane), "off before entry");
        enter_text_mode(&mut app);
        assert!(app.state.emacs.owns_pane_cursor(pane));
        assert!(
            !app.state
                .emacs
                .owns_pane_cursor(crate::layout::PaneId::alloc()),
            "other panes keep their host cursor"
        );
        app.route_client_input(vec![b'q']);
        assert!(!app.state.emacs.owns_pane_cursor(pane), "off after exit");
    }

    /// Renders only the TEXT-mode overlay for the app's single pane into a
    /// TestBackend buffer (same shape as the `src/ui/panes.rs` render tests).
    fn draw_text_mode_overlay(app: &App, pane: crate::layout::PaneId) -> ratatui::buffer::Buffer {
        let info = app.state.pane_info_by_id(pane).expect("pane info").clone();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|frame| {
                let rt = app
                    .state
                    .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane)
                    .expect("runtime");
                crate::emacs::render::render_text_mode_overlay(&app.state, frame, &info, rt);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// Cells inside `inner` carrying the overlay's REVERSED modifier.
    fn overlay_cells(
        buffer: &ratatui::buffer::Buffer,
        inner: ratatui::layout::Rect,
    ) -> Vec<(u16, u16)> {
        let mut cells = Vec::new();
        for y in inner.y..inner.y + inner.height {
            for x in inner.x..inner.x + inner.width {
                if buffer[(x, y)]
                    .style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::REVERSED)
                {
                    cells.push((x, y));
                }
            }
        }
        cells
    }

    #[tokio::test]
    async fn overlay_draws_reversed_bold_point_at_the_point_cell() {
        let (mut app, pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-< -> (0, 0)
        app.route_client_input(vec![0x06, 0x06]); // C-f C-f -> (0, 2)
        let inner = app.state.pane_info_by_id(pane).unwrap().inner_rect;
        let metrics = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane)
            .expect("metrics");
        assert_eq!(
            metrics.offset_from_bottom, metrics.max_offset_from_bottom,
            "viewport top is scrollback row 0"
        );
        let buffer = draw_text_mode_overlay(&app, pane);
        let style = buffer[(inner.x + 2, inner.y)].style();
        assert!(
            style
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED | ratatui::style::Modifier::BOLD),
            "point cell is reversed+bold, got {style:?}"
        );
        assert_eq!(
            overlay_cells(&buffer, inner),
            vec![(inner.x + 2, inner.y)],
            "exactly one overlay cell, at (inner.x + col, inner.y + rel_row)"
        );
    }

    #[tokio::test]
    async fn overlay_draws_nothing_when_point_is_off_screen() {
        let (mut app, pane, _rx) = emacs_app_with_channel(&fifty_lines());
        enter_text_mode(&mut app);
        let inner = app.state.pane_info_by_id(pane).unwrap().inner_rect;
        let metrics = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane)
            .expect("metrics");
        assert!(metrics.max_offset_from_bottom > 0, "fixture has scrollback");
        // Below the viewport: point stays at the bottom, view scrolled to top.
        app.state.set_pane_scroll_offset(
            &app.terminal_runtimes,
            pane,
            metrics.max_offset_from_bottom,
        );
        assert!(
            app.state.emacs.text_mode.as_ref().unwrap().point.row >= u32::from(inner.height),
            "point sits below the visible rows"
        );
        assert!(
            overlay_cells(&draw_text_mode_overlay(&app, pane), inner).is_empty(),
            "no overlay when the point is below the viewport"
        );
        // Above the viewport: point on row 0, view scrolled back to the bottom.
        app.state
            .set_pane_scroll_offset(&app.terminal_runtimes, pane, 0);
        app.state.emacs.text_mode.as_mut().unwrap().point =
            crate::emacs::text_mode::Pos { row: 0, col: 0 };
        assert!(
            overlay_cells(&draw_text_mode_overlay(&app, pane), inner).is_empty(),
            "no overlay when the point is above the viewport"
        );
    }

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
    async fn c_g_mid_chord_also_deactivates_the_mark() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x00]); // C-SPC
        assert!(app.state.emacs.text_mode.as_ref().unwrap().mark_active);
        app.route_client_input(vec![0x18, 0x07]); // C-x C-g: quit mid-chord
        assert!(app.state.emacs.pending.is_empty(), "chord cancelled");
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Quit"));
        assert!(
            !app.state.emacs.text_mode.as_ref().unwrap().mark_active,
            "keyboard-quit mid-chord deactivates the mark, like bound C-g"
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

    #[tokio::test]
    async fn every_mark_set_pushes_the_pane_mark_ring() {
        let (mut app, pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x00]); // C-SPC
        app.route_client_input(vec![0x0e]); // C-n
        app.route_client_input(vec![0x00]); // C-SPC
        assert_eq!(
            app.state.emacs.mark_rings.get(&pane).map(|r| r.len()),
            Some(2)
        );
    }

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
        assert_eq!(app.state.emacs.kill_ring.head(), Some("alpha\nbravo six"));
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
        assert_eq!(app.state.emacs.kill_ring.head(), Some("alpha\nbravo six"));
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
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Buffer is read-only"));
        assert!(sent_bytes(&mut rx).is_empty(), "nothing typed into the PTY");
    }

    #[tokio::test]
    async fn m_w_with_clipboard_sync_off_emits_no_clipboard_event() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        assert!(!app.state.emacs.clipboard_sync, "fixture default is off");
        select_first_two_lines(&mut app);
        app.route_client_input(vec![0x1b, b'w']); // M-w
        assert_eq!(app.state.emacs.kill_ring.head(), Some("alpha\nbravo six"));
        assert!(
            app.event_rx.try_recv().is_err(),
            "no ClipboardWrite when clipboard_sync is off"
        );
    }

    #[tokio::test]
    async fn c_y_types_kill_ring_head_into_the_pty() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.state.emacs.kill_ring.push("hello".into());
        // C-y in live mode; test runtime has bracketed paste off -> raw text
        app.route_client_input(vec![0x19]);
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

    #[tokio::test]
    async fn unbound_multi_chord_between_yanks_breaks_the_yank_chain() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.state.emacs.kill_ring.push("x".into());
        app.route_client_input(vec![0x19]); // C-y
        app.route_client_input(vec![0x18, b'z']); // C-x z: unbound, swallowed
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-x z is undefined"));
        let _ = sent_bytes(&mut rx);
        app.route_client_input(vec![0x1b, b'y']); // M-y no longer chains
        assert_eq!(
            app.state.emacs.echo.as_deref(),
            Some("Previous command was not a yank")
        );
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn m_y_in_text_mode_reports_read_only() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        app.state.emacs.kill_ring.push("something".into());
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'y']); // M-y
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Buffer is read-only"));
        assert!(sent_bytes(&mut rx).is_empty(), "nothing typed into the PTY");
    }

    #[tokio::test]
    async fn m_g_g_prompts_and_jumps_to_the_line() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'g', b'g']); // M-g g
        assert_eq!(
            app.state
                .emacs
                .text_mode
                .as_ref()
                .unwrap()
                .goto_line
                .as_deref(),
            Some("")
        );
        app.route_client_input(b"13".to_vec()); // prompt: "13"
        app.route_client_input(vec![0x7f]); // DEL -> "1"
        app.route_client_input(vec![0x7f]); // DEL -> ""
        app.route_client_input(b"3".to_vec()); // prompt: "3"
        assert_eq!(
            app.state
                .emacs
                .text_mode
                .as_ref()
                .unwrap()
                .goto_line
                .as_deref(),
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
}
