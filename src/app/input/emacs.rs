//! Glue between the pure Emacs engine (`crate::emacs`) and herdr's `App`.
//!
//! Emacs layer seam (fork): this file is new code registered with a single
//! `mod emacs;` line. It lives under `src/app/input/` because executing
//! commands needs `pub(super)` App internals (`execute_tui_navigate_action`,
//! `set_pane_scroll_offset`, ...).

use crossterm::event::KeyEventKind;

use super::navigate::{ActionContext, NavigateAction};
use crate::app::state::Mode;
use crate::app::text_input::{action_for_key as text_action_for_key, TextInputAction};
use crate::app::App;
use crate::emacs::commands::{herdr_action_is_indexed, EmacsBuiltin, EmacsCommand, MapContext};
use crate::emacs::isearch::{
    initial_selection, repeated_selection, IsearchState, SearchDirection, SearchSpan,
};
use crate::emacs::keymap::{format_seq, Chord, Lookup};
use crate::emacs::open_target::{self, OpenTarget};
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
        // A companion terminal Emacs frame owns its complete keyboard. The
        // ordinary terminal dispatcher has a matching bypass for Herdr's
        // direct bindings, prefix, and host PageUp/PageDown behavior.
        if self.focused_pane_has_exclusive_input() {
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

        let chord = Chord::from_key(&key);

        self.state.emacs.echo = None;

        // Emacs layer seam (fork): TEXT mode freezes the cursor on the pane
        // that was focused at `C-x [` time. If focus moved to another pane
        // (or that pane no longer resolves to a live runtime) since then,
        // auto-exit TEXT mode — running the entry_offset_from_bottom
        // scroll-restore — and fall through to normal live-mode handling of
        // this key instead of swallowing keystrokes typed at a pane the
        // user can no longer see a cursor in.
        if self.state.emacs.minibuffer.is_none() {
            if let Some(text_pane_id) = self.state.emacs.text_mode.as_ref().map(|text| text.pane_id)
            {
                let focused_and_live =
                    self.emacs_focused_pane().is_some_and(|(ws_idx, pane_id)| {
                        pane_id == text_pane_id
                            && self
                                .state
                                .runtime_for_pane_in_workspace(
                                    &self.terminal_runtimes,
                                    ws_idx,
                                    pane_id,
                                )
                                .is_some()
                    });
                if !focused_and_live {
                    self.emacs_exit_text_mode();
                }
            }
        }

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

        let ctx = self.state.emacs.map_context();
        let isearch_active = ctx == MapContext::Isearch;
        let text_active = matches!(ctx, MapContext::Text | MapContext::Isearch);

        let Some(chord) = chord else {
            return text_active || self.state.emacs.minibuffer.is_some();
        };

        // Host key auto-repeat can overlap the next chord: in particular a
        // trailing C-SPC Repeat may arrive after the first C-n Press. Treating
        // that stale repeat as another set-mark-command moves the mark to the
        // new point and makes an otherwise growing region appear stuck.
        // TEXT-mode motion commands repeat; one-shot state commands are
        // consumed without re-execution. Isearch/minibuffer retain their own
        // editing repeat behavior.
        if key.kind == KeyEventKind::Repeat && ctx == MapContext::Text {
            let repeatable = self.state.emacs.pending.is_empty()
                && matches!(
                    self.state.emacs.keymaps.lookup(ctx, &[chord]),
                    Lookup::Bound(command) if emacs_text_command_accepts_repeat(command)
                );
            if !repeatable {
                return true;
            }
        }

        // M-x and feedback are Herdr-owned text fields. Give their structural
        // editing keys to the same pure input engine used by rename/search
        // prompts. Live terminal-owned Codex/Claude drafts never enter here.
        if self.state.emacs.pending.is_empty() && self.state.emacs.minibuffer.is_some() {
            let action = text_action_for_key(key.as_key_event()).filter(|action| {
                matches!(
                    action,
                    TextInputAction::BackwardSexp
                        | TextInputAction::ForwardSexp
                        | TextInputAction::BackwardUpList
                        | TextInputAction::DownList
                        | TextInputAction::MarkSexp
                        | TextInputAction::KillSexp
                        | TextInputAction::UnwrapSelection
                        | TextInputAction::ShrinkSelection
                )
            });
            if let Some(action) = action {
                let yank = self
                    .state
                    .emacs
                    .kill_ring
                    .head()
                    .unwrap_or_default()
                    .to_string();
                let killed = self
                    .state
                    .emacs
                    .minibuffer
                    .as_mut()
                    .and_then(|minibuffer| minibuffer.apply_text_action(action, &yank));
                if let Some(killed) = killed {
                    self.state.emacs.kill_ring.push(killed);
                }
                return true;
            }
        }

        // C-g always cancels an in-flight chord (and, in TEXT mode,
        // deactivates the mark). Delegates to KeyboardQuit so mid-chord quit
        // and bound quit behave identically.
        if !self.state.emacs.pending.is_empty() && chord == Chord::ctrl('g') {
            self.execute_emacs_command(EmacsCommand::Builtin(EmacsBuiltin::KeyboardQuit), None);
            return true;
        }

        let mut seq = self.state.emacs.pending.clone();
        seq.push(chord);
        // Emacs layer seam (fork): the ordered keymap stack, not an
        // either/or choice — a sequence unbound in the local map falls
        // through to global. This is what makes C-x 3 work in TEXT mode.
        match self.state.emacs.keymaps.lookup(ctx, &seq) {
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
                self.state.emacs.last_yank = None;
                self.state.emacs.recenter_cycle = 0;
                let single = seq.len() == 1;
                if self.state.emacs.minibuffer.is_some() {
                    if let Some(c) = chord.self_insert_char().filter(|_| single) {
                        if let Some(minibuffer) = self.state.emacs.minibuffer.as_mut() {
                            minibuffer.insert_char(c);
                        }
                    } else {
                        self.state.emacs.echo = Some(format!("{} is undefined", format_seq(&seq)));
                    }
                    return true;
                }
                if isearch_active {
                    if let Some(c) = chord.self_insert_char().filter(|_| single) {
                        self.emacs_isearch_insert(c);
                    } else {
                        self.state.emacs.echo = Some(format!("{} is undefined", format_seq(&seq)));
                    }
                    return true;
                }
                // Spec §3.3. "Buffer is read-only" is ONLY for a key that
                // would insert; it is not the catch-all for unbound keys.
                if text_active && single && chord.is_self_insert() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                    true
                } else if !text_active && single {
                    // Live mode: a single unbound key belongs to the agent.
                    // Silence here is correct — see the term-char-mode
                    // contract in spec §2.
                    false
                } else {
                    self.state.emacs.echo = Some(format!("{} is undefined", format_seq(&seq)));
                    true
                }
            }
        }
    }

    /// Press-equivalent consume decision, used for repeat/release events.
    fn emacs_would_consume(&self, key: TerminalKey) -> bool {
        if self.state.emacs.text_mode.is_some() || self.state.emacs.minibuffer.is_some() {
            return true;
        }
        let emacs = &self.state.emacs;
        if emacs.quoted_insert || !emacs.pending.is_empty() {
            return true;
        }
        match Chord::from_key(&key) {
            Some(chord) => !matches!(
                emacs.keymaps.lookup(emacs.map_context(), &[chord]),
                Lookup::Unbound
            ),
            None => false,
        }
    }

    /// Execute a named command. `prefix` is the universal argument
    /// (`Option<i64>`): motions repeat, `C-u C-SPC` pops the mark ring, and
    /// the three indexed herdr actions take their index from it.
    pub(crate) fn execute_emacs_command(&mut self, cmd: EmacsCommand, prefix: Option<i64>) {
        if cmd != EmacsCommand::Builtin(EmacsBuiltin::RecenterTopBottom) {
            self.state.emacs.recenter_cycle = 0;
        }
        let is_search_command = matches!(
            cmd,
            EmacsCommand::Builtin(
                EmacsBuiltin::IsearchForward
                    | EmacsBuiltin::IsearchBackward
                    | EmacsBuiltin::IsearchExit
                    | EmacsBuiltin::IsearchDeleteChar
                    | EmacsBuiltin::IsearchPreviousHistory
                    | EmacsBuiltin::IsearchNextHistory
                    | EmacsBuiltin::KeyboardQuit
            )
        );
        if !is_search_command
            && self
                .state
                .emacs
                .text_mode
                .as_ref()
                .is_some_and(|text| text.isearch.is_some())
        {
            self.emacs_accept_isearch();
        }
        if !matches!(
            cmd,
            EmacsCommand::Builtin(EmacsBuiltin::Yank | EmacsBuiltin::YankPop)
        ) {
            self.state.emacs.last_yank = None;
        }
        match cmd {
            EmacsCommand::Herdr(action) => self.emacs_navigate(action, prefix),
            EmacsCommand::Builtin(builtin) => self.execute_emacs_builtin(builtin, prefix),
        }
    }

    fn execute_emacs_builtin(&mut self, builtin: EmacsBuiltin, prefix: Option<i64>) {
        match builtin {
            EmacsBuiltin::InterruptProcess => {
                self.emacs_send_key_to_focused_pane(TerminalKey::new(
                    crossterm::event::KeyCode::Char('c'),
                    crossterm::event::KeyModifiers::CONTROL,
                ));
            }
            EmacsBuiltin::QuotedInsert => {
                if self.state.emacs.minibuffer.is_some() {
                    self.state.emacs.echo =
                        Some("Quoted insert is not implemented here".to_string());
                } else if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                } else {
                    self.state.emacs.quoted_insert = true;
                    self.state.emacs.echo = Some("C-q-".to_string());
                }
            }
            EmacsBuiltin::KeyboardQuit => {
                if self
                    .state
                    .emacs
                    .text_mode
                    .as_ref()
                    .is_some_and(|text| text.isearch.is_some())
                {
                    self.emacs_abort_isearch();
                } else {
                    self.state.emacs.pending.clear();
                    self.state.emacs.minibuffer = None;
                    if let Some(text) = self.state.emacs.text_mode.as_mut() {
                        text.mark_active = false;
                    }
                    self.state.emacs.echo = Some("Quit".to_string());
                }
            }
            EmacsBuiltin::OpenAtPoint => self.emacs_open_at_point(),
            EmacsBuiltin::TextMode => {
                if self.state.emacs.text_mode.is_none() {
                    self.emacs_enter_text_mode();
                }
            }
            EmacsBuiltin::ExitTextMode => self.emacs_exit_text_mode(),
            EmacsBuiltin::RecenterTopBottom => self.emacs_recenter_top_bottom(),
            EmacsBuiltin::ForwardChar
            | EmacsBuiltin::BackwardChar
            | EmacsBuiltin::NextLine
            | EmacsBuiltin::PreviousLine
            | EmacsBuiltin::ForwardWord
            | EmacsBuiltin::BackwardWord
            | EmacsBuiltin::ForwardSexp
            | EmacsBuiltin::BackwardSexp
            | EmacsBuiltin::MoveBeginningOfLine
            | EmacsBuiltin::MoveEndOfLine
            | EmacsBuiltin::ScrollUp
            | EmacsBuiltin::ScrollDown
            | EmacsBuiltin::BeginningOfBuffer
            | EmacsBuiltin::EndOfBuffer => {
                if self.state.emacs.minibuffer.is_some() {
                    self.emacs_minibuffer_motion(builtin);
                } else {
                    self.emacs_text_motion(builtin, prefix);
                }
            }
            EmacsBuiltin::SetMark => self.emacs_set_mark(),
            EmacsBuiltin::ExchangePointAndMark => self.emacs_exchange_point_and_mark(),
            // In a read-only buffer C-w cannot delete, so kill-region
            // degrades to kill-ring-save.
            EmacsBuiltin::KillRingSave | EmacsBuiltin::KillRegion => self.emacs_kill_ring_save(),
            EmacsBuiltin::Yank => {
                if self.state.emacs.minibuffer.is_some() {
                    let content = self.state.emacs.kill_ring.head().map(str::to_owned);
                    if let (Some(content), Some(minibuffer)) =
                        (content, self.state.emacs.minibuffer.as_mut())
                    {
                        minibuffer.insert_str(&content);
                    }
                } else if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                } else {
                    self.emacs_yank_live();
                }
            }
            EmacsBuiltin::YankPop => {
                if self.state.emacs.text_mode.is_some() {
                    self.state.emacs.echo = Some("Buffer is read-only".to_string());
                } else {
                    self.emacs_yank_pop_live();
                }
            }
            EmacsBuiltin::GotoLine => {
                if let Some(text) = self.state.emacs.text_mode.as_mut() {
                    text.goto_line = Some(String::new());
                }
            }
            EmacsBuiltin::IsearchForward => {
                self.emacs_start_or_repeat_isearch(SearchDirection::Forward)
            }
            EmacsBuiltin::IsearchBackward => {
                self.emacs_start_or_repeat_isearch(SearchDirection::Backward)
            }
            EmacsBuiltin::IsearchExit => self.emacs_accept_isearch(),
            EmacsBuiltin::IsearchDeleteChar => self.emacs_isearch_delete_char(),
            EmacsBuiltin::IsearchPreviousHistory => self.emacs_isearch_history(true),
            EmacsBuiltin::IsearchNextHistory => self.emacs_isearch_history(false),
            EmacsBuiltin::MoveTabLeft => self.emacs_move_tab(-1),
            EmacsBuiltin::MoveTabRight => self.emacs_move_tab(1),
            EmacsBuiltin::ExecuteExtendedCommand => {
                self.state.emacs.minibuffer =
                    Some(crate::emacs::minibuffer::MinibufferState::command());
            }
            EmacsBuiltin::Feedback => {
                self.state.emacs.minibuffer =
                    Some(crate::emacs::minibuffer::MinibufferState::feedback());
            }
            EmacsBuiltin::HerdrOnboarding => super::modal::open_emacs_onboarding(&mut self.state),
            EmacsBuiltin::RefreshHerdr => {
                self.state.request_live_handoff = true;
                self.state.emacs.echo = Some("Refreshing Herdr…".to_string());
            }
            EmacsBuiltin::ExitMinibuffer => self.emacs_minibuffer_exit(),
            EmacsBuiltin::DeleteBackwardChar => {
                if let Some(minibuffer) = self.state.emacs.minibuffer.as_mut() {
                    minibuffer.delete_backward_char();
                }
            }
            EmacsBuiltin::DeleteForwardChar => {
                if let Some(minibuffer) = self.state.emacs.minibuffer.as_mut() {
                    minibuffer.delete_forward_char();
                }
            }
            EmacsBuiltin::KillBeginningOfLine => {
                let killed = self
                    .state
                    .emacs
                    .minibuffer
                    .as_mut()
                    .and_then(|minibuffer| minibuffer.kill_beginning_of_line());
                if let Some(killed) = killed {
                    self.state.emacs.kill_ring.push(killed);
                }
            }
            EmacsBuiltin::KillLine => {
                let killed = self
                    .state
                    .emacs
                    .minibuffer
                    .as_mut()
                    .and_then(|minibuffer| minibuffer.kill_line());
                if let Some(killed) = killed {
                    self.state.emacs.kill_ring.push(killed);
                }
            }
            EmacsBuiltin::BackwardKillWord => {
                let killed = self
                    .state
                    .emacs
                    .minibuffer
                    .as_mut()
                    .and_then(|minibuffer| minibuffer.backward_kill_word());
                if let Some(killed) = killed {
                    self.state.emacs.kill_ring.push(killed);
                }
            }
            EmacsBuiltin::MarkWholeInput => {
                if let Some(minibuffer) = self.state.emacs.minibuffer.as_mut() {
                    minibuffer.select_all();
                }
            }
            EmacsBuiltin::DescribeBindings => super::modal::open_keybind_help(&mut self.state),
            // Wired in later tasks; named and reachable from M-x now.
            EmacsBuiltin::UniversalArgument | EmacsBuiltin::DescribeKey => {
                self.state.emacs.echo = Some(format!(
                    "{} is not implemented yet",
                    EmacsCommand::Builtin(builtin).name()
                ));
            }
        }
    }

    /// Run a herdr action. The three indexed actions take their index from
    /// the prefix argument: `C-u 2 M-x switch-tab` is tab index 1 (the
    /// prefix arg is 1-based, herdr's index is 0-based).
    fn emacs_navigate(&mut self, action: NavigateAction, prefix: Option<i64>) {
        let action = if herdr_action_is_indexed(action) {
            let index = prefix.unwrap_or(1).max(1).saturating_sub(1) as usize;
            match action {
                NavigateAction::SwitchWorkspace(_) => NavigateAction::SwitchWorkspace(index),
                NavigateAction::SwitchTab(_) => NavigateAction::SwitchTab(index),
                NavigateAction::FocusAgent(_) => NavigateAction::FocusAgent(index),
                other => other,
            }
        } else {
            action
        };
        self.execute_tui_navigate_action(action, ActionContext::Prefix);
    }

    /// `M-[` / `M-]` — reorder the active tab. herdr exposes tab.move only
    /// through mouse drag (`move_tab_via_api`), so this is a builtin rather
    /// than a `NavigateAction` (spec §3.8).
    ///
    /// `Workspace::move_tab(source, insert)` takes a PRE-removal slot:
    /// `target = if source < insert { insert - 1 } else { insert }`. So
    /// left is `source - 1` and right is `source + 2`. Clamped at both ends
    /// — no wraparound.
    fn emacs_move_tab(&mut self, delta: i64) {
        let Some(ws_idx) = self.state.active else {
            return;
        };
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        let source = ws.active_tab;
        let last = ws.tabs.len().saturating_sub(1);
        let insert_idx = if delta < 0 {
            if source == 0 {
                return; // already leftmost: no-op, no wraparound
            }
            source - 1
        } else {
            if source >= last {
                return; // already rightmost: no-op, no wraparound
            }
            source + 2
        };
        self.move_tab_via_api(ws_idx, source, insert_idx);
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

    fn emacs_open_at_point(&mut self) {
        let Some((ws_idx, pane_id)) = self.emacs_focused_pane() else {
            self.state.emacs.echo = Some("No focused pane".to_string());
            return;
        };
        let Some(cwd) = self.follow_cwd_for_pane_in_workspace(ws_idx, pane_id) else {
            self.state.emacs.echo = Some("Pane working directory is unavailable".to_string());
            return;
        };
        let home = crate::integration::home_dir().ok();

        let selected_region = self.state.emacs.text_mode.as_ref().and_then(|text| {
            (text.pane_id == pane_id && text.mark_active)
                .then_some(text.mark)
                .flatten()
                .map(|mark| (mark, text.point))
        });
        if let Some((mark, point)) = selected_region {
            let (start, end) = if mark <= point {
                (mark, point)
            } else {
                (point, mark)
            };
            let target = self
                .emacs_region_text(ws_idx, pane_id, start, end)
                .and_then(|selection| {
                    open_target::resolve_selection(&selection, &cwd, home.as_deref())
                });
            match target {
                Some(target) => self.emacs_open_target(target, cwd),
                None => {
                    self.state.emacs.echo =
                        Some("Selected text is not an existing file or URL".to_string())
                }
            }
            return;
        }

        let Some(point) = self.emacs_terminal_point_at_focus(ws_idx, pane_id) else {
            return;
        };
        let target = {
            let Some(rt) =
                self.state
                    .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            else {
                self.state.emacs.echo = Some("Focused pane has no terminal".to_string());
                return;
            };

            let hyperlink = rt
                .hyperlink_uri_at(point)
                .or_else(|| {
                    point.col.checked_sub(1).and_then(|col| {
                        rt.hyperlink_uri_at(crate::pane::TerminalTextPoint {
                            row: point.row,
                            col,
                        })
                    })
                })
                .and_then(|uri| open_target::resolve_selection(&uri, &cwd, home.as_deref()));
            hyperlink.or_else(|| {
                let logical = rt.logical_text_at_point(point)?;
                open_target::resolve_at_point(
                    &logical.text,
                    logical.point_byte,
                    &cwd,
                    home.as_deref(),
                )
            })
        };

        match target {
            Some(target) => self.emacs_open_target(target, cwd),
            None => self.state.emacs.echo = Some("No existing file or URL at point".to_string()),
        }
    }

    fn emacs_terminal_point_at_focus(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::pane::TerminalTextPoint> {
        if let Some(text) = self
            .state
            .emacs
            .text_mode
            .as_ref()
            .filter(|text| text.pane_id == pane_id)
        {
            return Some(crate::pane::TerminalTextPoint {
                row: text.point.row,
                col: text.point.col,
            });
        }

        let info = self.state.pane_info_by_id(pane_id)?.clone();
        let metrics = self
            .state
            .pane_scroll_metrics(&self.terminal_runtimes, pane_id)?;
        if metrics.offset_from_bottom > 0 {
            self.state.emacs.echo =
                Some("Enter TEXT mode to open a target in scrollback".to_string());
            return None;
        }
        let rt =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;
        let cursor = rt
            .cursor_state(info.inner_rect, true)
            .filter(|cursor| cursor.visible)?;
        let viewport_row = cursor.y.saturating_sub(info.inner_rect.y);
        Some(crate::pane::TerminalTextPoint {
            row: crate::selection::Selection::absolute_row_for_viewport(
                viewport_row,
                Some(metrics),
            ),
            col: cursor.x.saturating_sub(info.inner_rect.x),
        })
    }

    fn emacs_open_target(&mut self, target: OpenTarget, cwd: std::path::PathBuf) {
        if let OpenTarget::Path { path, .. } = &target {
            if open_target::is_previewable_image_path(path) {
                if self.request_image_preview(path.clone()) {
                    self.emacs_leave_outer_interaction();
                }
                return;
            }
        }

        let argv = open_target::emacsclient_argv(&target);
        let new_pane =
            match self.spawn_overlay_argv_command(&argv, Some(cwd), Vec::new(), Vec::new()) {
                Ok((_ws_idx, new_pane)) => new_pane,
                Err(err) => {
                    self.state.emacs.echo = Some(format!("Could not start emacsclient: {err}"));
                    return;
                }
            };

        let pane_id = new_pane.pane_id;
        let terminal_id = new_pane.terminal.id.clone();
        self.terminal_runtimes
            .insert(terminal_id.clone(), new_pane.runtime);
        self.state.terminals.insert(terminal_id, new_pane.terminal);
        self.state.remove_alias_shadowed_by_new_pane(pane_id);
        if let Some(overlay) = self.overlay_panes.get_mut(&pane_id) {
            overlay.input_profile = crate::app::OverlayInputProfile::Exclusive;
        }
        self.emacs_leave_outer_interaction();
    }

    fn emacs_leave_outer_interaction(&mut self) {
        if self.state.emacs.text_mode.is_some() {
            self.emacs_exit_text_mode();
        }
        self.state.emacs.pending.clear();
        self.state.emacs.quoted_insert = false;
        self.state.emacs.minibuffer = None;
        self.state.emacs.echo = None;
        self.state.emacs.last_yank = None;
        self.state.emacs.recenter_cycle = 0;
    }

    fn emacs_minibuffer_motion(&mut self, builtin: EmacsBuiltin) {
        let Some(minibuffer) = self.state.emacs.minibuffer.as_mut() else {
            return;
        };
        match builtin {
            EmacsBuiltin::MoveBeginningOfLine => minibuffer.move_beginning_of_line(),
            EmacsBuiltin::MoveEndOfLine => minibuffer.move_end_of_line(),
            EmacsBuiltin::ForwardChar => minibuffer.forward_char(),
            EmacsBuiltin::BackwardChar => minibuffer.backward_char(),
            EmacsBuiltin::ForwardWord => minibuffer.forward_word(),
            EmacsBuiltin::BackwardWord => minibuffer.backward_word(),
            _ => {}
        }
    }

    fn emacs_minibuffer_exit(&mut self) {
        let is_empty_feedback = self
            .state
            .emacs
            .minibuffer
            .as_ref()
            .is_some_and(|minibuffer| {
                matches!(
                    minibuffer.kind,
                    crate::emacs::minibuffer::MinibufferKind::Feedback
                ) && minibuffer.input.trim().is_empty()
            });
        if is_empty_feedback {
            if let Some(minibuffer) = self.state.emacs.minibuffer.as_mut() {
                minibuffer.prompt = "Feedback is empty — type a comment: ".to_string();
            }
            return;
        }

        let Some(minibuffer) = self.state.emacs.minibuffer.take() else {
            return;
        };
        match minibuffer.kind {
            crate::emacs::minibuffer::MinibufferKind::ExecuteCommand => {
                let name = minibuffer.input.trim();
                match EmacsCommand::from_name(name) {
                    Some(command) => {
                        self.execute_emacs_command(command, None);
                    }
                    None => {
                        self.state.emacs.echo = Some(format!("No match: {name}"));
                    }
                }
            }
            crate::emacs::minibuffer::MinibufferKind::Feedback => {
                self.emacs_submit_feedback(minibuffer.input.trim())
            }
        }
    }

    fn emacs_submit_feedback(&mut self, comment: &str) {
        let Some((ws_idx, pane_id)) = self.emacs_focused_pane() else {
            self.state.emacs.echo = Some("No focused pane for feedback".to_string());
            return;
        };
        let message = format!("Herdr feedback:\n{comment}");
        if self.emacs_send_text_to_pane(ws_idx, pane_id, &message, 0) {
            self.emacs_send_key_to_focused_pane(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            ));
            self.state.emacs.echo = Some("Feedback sent".to_string());
        } else {
            self.state.emacs.echo = Some("Could not send feedback to pane".to_string());
        }
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
            isearch: None,
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

    fn emacs_start_or_repeat_isearch(&mut self, direction: SearchDirection) {
        if self.state.emacs.text_mode.is_none() {
            self.emacs_enter_text_mode();
        }
        let Some(text) = self.state.emacs.text_mode.as_mut() else {
            return;
        };
        if text.isearch.is_none() {
            text.mark_active = false;
            text.isearch = Some(IsearchState::new(direction, text.point));
            return;
        }

        let query_is_empty = text
            .isearch
            .as_ref()
            .is_none_or(|isearch| isearch.query.is_empty());
        if let Some(isearch) = text.isearch.as_mut() {
            isearch.direction = direction;
        }
        if query_is_empty {
            let previous = self.state.emacs.search_ring.get(0).map(str::to_owned);
            if let (Some(previous), Some(isearch)) = (
                previous,
                self.state
                    .emacs
                    .text_mode
                    .as_mut()
                    .and_then(|text| text.isearch.as_mut()),
            ) {
                isearch.query = previous;
                isearch.history_cursor = Some(0);
                self.emacs_refresh_isearch(false);
            }
        } else {
            self.emacs_refresh_isearch(true);
        }
    }

    fn emacs_isearch_insert(&mut self, c: char) {
        let Some(isearch) = self
            .state
            .emacs
            .text_mode
            .as_mut()
            .and_then(|text| text.isearch.as_mut())
        else {
            return;
        };
        isearch.query.push(c);
        isearch.history_cursor = None;
        isearch.history_draft.clear();
        self.emacs_refresh_isearch(false);
    }

    fn emacs_isearch_delete_char(&mut self) {
        let Some(isearch) = self
            .state
            .emacs
            .text_mode
            .as_mut()
            .and_then(|text| text.isearch.as_mut())
        else {
            return;
        };
        isearch.query.pop();
        isearch.history_cursor = None;
        isearch.history_draft.clear();
        self.emacs_refresh_isearch(false);
    }

    fn emacs_isearch_history(&mut self, previous: bool) {
        let Some(isearch) = self
            .state
            .emacs
            .text_mode
            .as_mut()
            .and_then(|text| text.isearch.as_mut())
        else {
            return;
        };

        let (next_cursor, query) = if previous {
            let next = match isearch.history_cursor {
                Some(index) if index + 1 < self.state.emacs.search_ring.len() => index + 1,
                Some(index) => index,
                None => {
                    isearch.history_draft = isearch.query.clone();
                    0
                }
            };
            (
                Some(next),
                self.state.emacs.search_ring.get(next).map(str::to_owned),
            )
        } else {
            match isearch.history_cursor {
                Some(0) => (None, Some(isearch.history_draft.clone())),
                Some(index) => {
                    let next = index - 1;
                    (
                        Some(next),
                        self.state.emacs.search_ring.get(next).map(str::to_owned),
                    )
                }
                None => return,
            }
        };
        let Some(query) = query else {
            return;
        };
        if let Some(isearch) = self
            .state
            .emacs
            .text_mode
            .as_mut()
            .and_then(|text| text.isearch.as_mut())
        {
            isearch.history_cursor = next_cursor;
            isearch.query = query;
        }
        self.emacs_refresh_isearch(false);
    }

    /// Re-scan the current terminal snapshot. Query edits restart at the
    /// search origin; repeated C-s/C-r advances from the current match.
    fn emacs_refresh_isearch(&mut self, repeat: bool) {
        let Some(text) = self.state.emacs.text_mode.as_ref() else {
            return;
        };
        let Some(isearch) = text.isearch.as_ref() else {
            return;
        };
        let (pane_id, current_point) = (text.pane_id, text.point);
        let (direction, origin, query) = (isearch.direction, isearch.origin, isearch.query.clone());
        let previous_span = isearch.current.and_then(|index| {
            isearch.matches.get(index).map(|text_match| SearchSpan {
                start: Pos {
                    row: text_match.start.row,
                    col: text_match.start.col,
                },
                end: Pos {
                    row: text_match.end.row,
                    col: text_match.end.col,
                },
            })
        });

        if query.is_empty() {
            if let Some(text) = self.state.emacs.text_mode.as_mut() {
                text.point = origin;
                if let Some(isearch) = text.isearch.as_mut() {
                    isearch.matches.clear();
                    isearch.current = None;
                    isearch.failing = false;
                    isearch.wrapped = false;
                }
            }
            self.emacs_scroll_point_into_view(pane_id);
            return;
        }

        let Some((ws_idx, focused_pane)) = self.emacs_focused_pane() else {
            return;
        };
        if focused_pane != pane_id {
            return;
        }
        let (matches, selection, new_point) = {
            let Some(rt) =
                self.state
                    .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            else {
                return;
            };
            let matches = rt.search_text_matches(&query, query.chars().any(char::is_uppercase));
            let spans = matches
                .iter()
                .map(|text_match| SearchSpan {
                    start: Pos {
                        row: text_match.start.row,
                        col: text_match.start.col,
                    },
                    end: Pos {
                        row: text_match.end.row,
                        col: text_match.end.col,
                    },
                })
                .collect::<Vec<_>>();
            let selection = if repeat {
                let refreshed_current = previous_span
                    .and_then(|previous| spans.iter().position(|span| *span == previous));
                match (refreshed_current, isearch.current) {
                    (Some(current), _) => repeated_selection(&spans, direction, Some(current)),
                    (None, None) => repeated_selection(&spans, direction, None),
                    (None, Some(_)) => initial_selection(&spans, direction, current_point),
                }
            } else {
                initial_selection(&spans, direction, origin)
            };
            let new_point = selection.and_then(|selection| {
                let span = spans.get(selection.index)?;
                Some(match direction {
                    SearchDirection::Forward => {
                        text_mode::forward_char(&RuntimeBuffer { rt }, span.end)
                    }
                    SearchDirection::Backward => span.start,
                })
            });
            (matches, selection, new_point)
        };

        if let Some(text) = self.state.emacs.text_mode.as_mut() {
            if let Some(new_point) = new_point {
                text.point = new_point;
            }
            if let Some(isearch) = text.isearch.as_mut() {
                isearch.matches = matches;
                isearch.current = selection.map(|selection| selection.index);
                isearch.failing = selection.is_none();
                isearch.wrapped = selection.is_some_and(|selection| selection.wrapped);
            }
        }
        self.emacs_scroll_point_into_view(pane_id);
    }

    fn emacs_accept_isearch(&mut self) {
        let Some(text) = self.state.emacs.text_mode.as_mut() else {
            return;
        };
        let Some(isearch) = text.isearch.take() else {
            return;
        };
        if isearch.query.is_empty() {
            return;
        }
        let (pane_id, origin) = (text.pane_id, isearch.origin);
        text.mark = Some(origin);
        text.mark_active = false;
        self.state.emacs.search_ring.push(isearch.query);
        self.state
            .emacs
            .mark_rings
            .entry(pane_id)
            .or_insert_with(|| crate::emacs::rings::MarkRing::new(self.state.emacs.mark_ring_max))
            .push((origin.row, origin.col));
    }

    fn emacs_abort_isearch(&mut self) {
        let Some(text) = self.state.emacs.text_mode.as_mut() else {
            return;
        };
        let Some(isearch) = text.isearch.take() else {
            return;
        };
        let pane_id = text.pane_id;
        text.point = isearch.origin;
        self.state.emacs.search_ring.push(isearch.query);
        self.state.emacs.pending.clear();
        self.state.emacs.echo = Some("Quit".to_string());
        self.emacs_scroll_point_into_view(pane_id);
    }

    /// Run one motion command against the frozen buffer, then keep the
    /// point visible.
    fn emacs_text_motion(&mut self, cmd: EmacsBuiltin, prefix: Option<i64>) {
        let _ = prefix; // Task 7 makes motions repeat.
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
                EmacsBuiltin::ForwardChar => text_mode::forward_char(&buf, point),
                EmacsBuiltin::BackwardChar => text_mode::backward_char(&buf, point),
                EmacsBuiltin::NextLine => text_mode::next_line(&buf, point),
                EmacsBuiltin::PreviousLine => text_mode::previous_line(&buf, point),
                EmacsBuiltin::ForwardWord => text_mode::forward_word(&buf, point),
                EmacsBuiltin::BackwardWord => text_mode::backward_word(&buf, point),
                // Fundamental-mode atoms in terminal scrollback use the same
                // word constituent rules as TEXT-mode word motion. Balanced
                // delimiter traversal can extend these commands when a trace
                // establishes the required syntax behavior.
                EmacsBuiltin::ForwardSexp => text_mode::forward_word(&buf, point),
                EmacsBuiltin::BackwardSexp => text_mode::backward_word(&buf, point),
                EmacsBuiltin::MoveBeginningOfLine => text_mode::line_beginning(point),
                EmacsBuiltin::MoveEndOfLine => text_mode::line_end(&buf, point),
                EmacsBuiltin::ScrollUp => text_mode::clamp(
                    &buf,
                    Pos {
                        row: point.row.saturating_add(page),
                        col: point.col,
                    },
                ),
                EmacsBuiltin::ScrollDown => text_mode::clamp(
                    &buf,
                    Pos {
                        row: point.row.saturating_sub(page),
                        col: point.col,
                    },
                ),
                EmacsBuiltin::BeginningOfBuffer => text_mode::buffer_beginning(),
                EmacsBuiltin::EndOfBuffer => text_mode::buffer_end(&buf),
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
        let search_active = self
            .state
            .emacs
            .text_mode
            .as_ref()
            .is_some_and(|text| text.pane_id == pane_id && text.isearch.is_some());
        let view_rows = u32::from(
            info.inner_rect
                .height
                .saturating_sub(u16::from(search_active))
                .max(1),
        );
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

    /// `C-l` / `recenter-top-bottom`: keep point fixed and cycle its screen
    /// position through middle, top, and bottom, as Emacs does by default.
    fn emacs_recenter_top_bottom(&mut self) {
        let Some(text) = self.state.emacs.text_mode.as_ref() else {
            self.state.emacs.recenter_cycle = 0;
            return;
        };
        let (pane_id, point_row) = (text.pane_id, text.point.row);
        let Some(metrics) = self
            .state
            .pane_scroll_metrics(&self.terminal_runtimes, pane_id)
        else {
            return;
        };
        let view_rows = metrics.viewport_rows.max(1);
        let target_row = match self.state.emacs.recenter_cycle {
            0 => view_rows / 2,
            1 => 0,
            _ => view_rows - 1,
        };
        let top = (point_row as usize)
            .saturating_sub(target_row)
            .min(metrics.max_offset_from_bottom);
        let offset = metrics.max_offset_from_bottom.saturating_sub(top);
        self.state
            .set_pane_scroll_offset(&self.terminal_runtimes, pane_id, offset);
        self.state.emacs.recenter_cycle = (self.state.emacs.recenter_cycle + 1) % 3;
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
        if self.state.emacs.clipboard_sync
            && self
                .event_tx
                .try_send(crate::events::AppEvent::ClipboardWrite {
                    content: content.into_bytes(),
                })
                .is_err()
        {
            tracing::warn!("failed to queue emacs clipboard write event");
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

fn emacs_text_command_accepts_repeat(command: EmacsCommand) -> bool {
    matches!(
        command,
        EmacsCommand::Builtin(
            EmacsBuiltin::ForwardChar
                | EmacsBuiltin::BackwardChar
                | EmacsBuiltin::NextLine
                | EmacsBuiltin::PreviousLine
                | EmacsBuiltin::ForwardWord
                | EmacsBuiltin::BackwardWord
                | EmacsBuiltin::ForwardSexp
                | EmacsBuiltin::BackwardSexp
                | EmacsBuiltin::MoveBeginningOfLine
                | EmacsBuiltin::MoveEndOfLine
                | EmacsBuiltin::RecenterTopBottom
                | EmacsBuiltin::ScrollUp
                | EmacsBuiltin::ScrollDown
                | EmacsBuiltin::BeginningOfBuffer
                | EmacsBuiltin::EndOfBuffer
        )
    )
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
    use crate::emacs::text_mode::Pos;
    use crate::input::TerminalKey;
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
        emacs_app_with_channel_at_size(bytes, 40, 10)
    }

    /// Sized variant used by the conformance corpus so recorded pane excerpts
    /// do not need to be padded or constrained to the default test viewport.
    pub(crate) fn emacs_app_with_channel_at_size(
        bytes: &[u8],
        width: u16,
        height: u16,
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
        let pane_infos = ws.tabs[0].layout.panes(ratatui::layout::Rect::new(
            0,
            0,
            width.max(1),
            height.max(1),
        ));
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
    async fn control_meta_o_opens_image_at_point_with_client_preview() {
        let path = std::env::temp_dir().join(format!(
            "herdr-meta-o-preview-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::write(&path, b"\x89PNG\r\n\x1a\npreview").expect("write image");
        let (mut app, _pane_id, _rx) =
            emacs_app_with_channel_at_size(path.to_string_lossy().as_bytes(), 160, 10);
        app.state.ensure_test_terminals();

        app.route_client_input(b"\x1b[111;7u".to_vec()); // C-M-o

        match app.event_rx.try_recv().expect("preview request") {
            crate::events::AppEvent::ImagePreviewRequested { path: requested } => {
                assert_eq!(requested, path)
            }
            event => panic!("unexpected event: {event:?}"),
        }
        let _ = std::fs::remove_file(path);
    }

    /// Three tabs (no PTYs), the middle one focused.
    fn emacs_app_with_three_tabs() -> (App, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let (mut app, _pane, rx) = emacs_app_with_channel(b"");
        let ws = &mut app.state.workspaces[0];
        ws.test_add_tab(Some("b"));
        ws.test_add_tab(Some("c"));
        assert_eq!(ws.tabs.len(), 3);
        ws.active_tab = 1;
        (app, rx)
    }

    fn tab_names(app: &App) -> Vec<String> {
        app.state.workspaces[0]
            .tabs
            .iter()
            .map(|tab| tab.custom_name.clone().unwrap_or_else(|| "a".to_string()))
            .collect()
    }

    /// M-[ = CSI 91;3u, M-] = CSI 93;3u.
    const KITTY_M_LBRACKET: &[u8] = b"\x1b[91;3u";
    const KITTY_M_RBRACKET: &[u8] = b"\x1b[93;3u";

    /// C-[ = CSI 91;5u, C-] = CSI 93;5u (kitty). Distinct from legacy ESC (0x1b).
    const KITTY_C_LBRACKET: &[u8] = b"\x1b[91;5u";
    const KITTY_C_RBRACKET: &[u8] = b"\x1b[93;5u";

    #[tokio::test]
    async fn c_bracket_moves_between_tabs() {
        let (mut app, mut rx) = emacs_app_with_three_tabs();
        assert_eq!(app.state.workspaces[0].active_tab, 1);
        app.route_client_input(KITTY_C_LBRACKET.to_vec()); // C-[ : previous-tab
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        app.route_client_input(KITTY_C_RBRACKET.to_vec()); // C-] : next-tab
        assert_eq!(app.state.workspaces[0].active_tab, 1);
        app.route_client_input(KITTY_C_RBRACKET.to_vec());
        assert_eq!(app.state.workspaces[0].active_tab, 2);
        assert!(
            sent_bytes(&mut rx).is_empty(),
            "chords never reach the pane"
        );
    }

    #[tokio::test]
    async fn m_bracket_reorders_tabs_and_the_moved_tab_stays_focused() {
        let (mut app, mut rx) = emacs_app_with_three_tabs();
        assert_eq!(tab_names(&app), vec!["a", "b", "c"]);

        app.route_client_input(KITTY_M_LBRACKET.to_vec()); // M-[ : move-tab-left
        assert_eq!(tab_names(&app), vec!["b", "a", "c"]);
        assert_eq!(
            app.state.workspaces[0].active_tab, 0,
            "the moved tab keeps focus"
        );

        app.route_client_input(KITTY_M_RBRACKET.to_vec()); // M-] : move-tab-right
        assert_eq!(tab_names(&app), vec!["a", "b", "c"]);
        assert_eq!(app.state.workspaces[0].active_tab, 1);
        assert!(sent_bytes(&mut rx).is_empty());
    }

    /// Spec §3.8: clamp at the ends, no wraparound.
    #[tokio::test]
    async fn move_tab_clamps_at_both_ends_without_wrapping() {
        let (mut app, _rx) = emacs_app_with_three_tabs();
        app.state.workspaces[0].active_tab = 0;
        app.route_client_input(KITTY_M_LBRACKET.to_vec()); // M-[ at the left edge
        assert_eq!(tab_names(&app), vec!["a", "b", "c"], "no wraparound");
        assert_eq!(app.state.workspaces[0].active_tab, 0);

        app.state.workspaces[0].active_tab = 2;
        app.route_client_input(KITTY_M_RBRACKET.to_vec()); // M-] at the right edge
        assert_eq!(tab_names(&app), vec!["a", "b", "c"], "no wraparound");
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    /// All four work from TEXT mode, by fallthrough to the global map.
    #[tokio::test]
    async fn tab_bindings_work_from_text_mode() {
        let (mut app, _rx) = emacs_app_with_three_tabs();
        // TEXT mode needs the focused pane's runtime, which lives in tab 0.
        app.state.workspaces[0].active_tab = 0;
        enter_text_mode(&mut app);
        assert!(app.state.emacs.text_mode.is_some());
        app.route_client_input(KITTY_M_RBRACKET.to_vec()); // M-]
        assert_eq!(tab_names(&app), vec!["b", "a", "c"]);
    }

    /// ESC is still exit-text-mode — a legacy byte 27 must NOT be read as C-[.
    #[tokio::test]
    async fn legacy_esc_still_exits_text_mode_and_does_not_switch_tabs() {
        let (mut app, _rx) = emacs_app_with_three_tabs();
        app.state.workspaces[0].active_tab = 0;
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b]); // legacy ESC
        assert!(app.state.emacs.text_mode.is_none(), "ESC exited TEXT mode");
        assert_eq!(
            app.state.workspaces[0].active_tab, 0,
            "ESC is not C-[ : no tab switch"
        );
    }

    /// Like `emacs_app_with_channel`, but with a second real pane (B) split
    /// into the same tab, focus left on the first pane (A). Lets tests move
    /// focus away from a TEXT-mode pane via the real `focus_pane_in_workspace`
    /// path (Finding 1: TEXT mode must not survive a focus change).
    fn emacs_app_with_two_panes(
        bytes: &[u8],
    ) -> (
        App,
        crate::layout::PaneId,
        crate::layout::PaneId,
        tokio::sync::mpsc::Receiver<bytes::Bytes>,
        tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) {
        let (mut app, pane_a, rx_a) = emacs_app_with_channel(bytes);
        let pane_b = crate::layout::PaneId::alloc();
        let terminal_b = crate::terminal::TerminalId::alloc();
        let (rt_b, rx_b) = crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            20,
            10,
            16 * 1024,
            b"",
            8,
        );
        let ws = &mut app.state.workspaces[0];
        ws.tabs[0]
            .panes
            .insert(pane_b, crate::pane::PaneState::new(terminal_b));
        ws.tabs[0].runtimes.insert(pane_b, rt_b);
        // insert_pane_near focuses the moved pane (B); reset focus to A so
        // callers can enter TEXT mode there first, as in real usage.
        ws.tabs[0].layout.insert_pane_near(
            pane_a,
            pane_b,
            ratatui::layout::Direction::Horizontal,
            0.5,
        );
        ws.tabs[0].layout.focus_pane(pane_a);
        let pane_infos = ws.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 40, 10));
        app.state.view.pane_infos = pane_infos;
        (app, pane_a, pane_b, rx_a, rx_b)
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
    async fn c_x_c_f_requests_the_workspace_location_picker() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.state.federation_member_id = "x1".into();
        app.route_client_input(vec![0x18]);
        app.route_client_input(vec![0x06]);

        assert!(app.state.request_new_workspace);
        app.open_workspace_create_dialog();
        let create = app
            .state
            .workspace_create
            .as_ref()
            .expect("workspace creation state");
        assert_eq!(create.step, crate::app::state::WorkspaceCreateStep::Server);
        assert_eq!(create.selected_server, 0);
        assert_eq!(create.servers[0].member_id, "x1");
        assert!(matches!(
            create.servers[0].kind,
            crate::app::state::WorkspaceCreateServerKind::Local
        ));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn c_x_t_opens_the_visible_tab_name_prompt() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x18, b't']);

        assert_eq!(app.state.mode, Mode::RenameTab);
        assert_eq!(app.state.name_input, "1");
        assert_eq!(app.state.rename_pane_target, None);
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn monolithic_input_route_uses_the_emacs_dispatcher() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");

        app.handle_raw_input_event(crate::raw_input::RawInputEvent::Key(TerminalKey::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::CONTROL,
        )))
        .await;

        assert_eq!(app.state.emacs.pending.len(), 1);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-x-"));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn c_c_t_opens_the_visible_tab_name_prompt() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x03, b't']);

        assert_eq!(app.state.mode, Mode::RenameTab);
        assert_eq!(app.state.name_input, "1");
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn c_c_w_opens_the_visible_workspace_name_prompt() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        let visible_name = app.state.workspaces[0].display_name().to_string();
        app.route_client_input(vec![0x03, b'w']);

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
        assert_eq!(app.state.name_input, visible_name);
        assert_eq!(app.state.rename_pane_target, None);
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn c_c_is_a_prefix_and_c_c_c_c_forwards_one_interrupt() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");

        app.route_client_input(vec![0x03]);
        assert_eq!(app.state.emacs.pending.len(), 1);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-c-"));
        assert!(sent_bytes(&mut rx).is_empty());

        app.route_client_input(vec![0x03]);
        assert!(app.state.emacs.pending.is_empty());
        assert_eq!(sent_bytes(&mut rx), vec![0x03]);
    }

    #[tokio::test]
    async fn meta_n_and_meta_p_cycle_detected_agents_without_pty_passthrough() {
        let (mut app, pane_a, mut rx_a) = emacs_app_with_channel(b"");
        let pane_b = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_a);
        app.state.ensure_test_terminals();
        for pane_id in [pane_a, pane_b] {
            let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.state
                .terminals
                .get_mut(&terminal_id)
                .expect("test terminal")
                .set_detected_state(
                    Some(crate::detect::Agent::Pi),
                    crate::detect::AgentState::Idle,
                );
        }

        app.route_client_input(vec![0x1b, b'n']);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_b));
        app.route_client_input(vec![0x1b, b'p']);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_a));
        assert!(sent_bytes(&mut rx_a).is_empty());
    }

    #[tokio::test]
    async fn control_meta_n_and_p_cycle_workspaces_without_pty_passthrough() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.state.workspaces.push(Workspace::test_new("other"));

        app.route_client_input(b"\x1b[110;7u".to_vec()); // C-M-n
        assert_eq!(app.state.active, Some(1));

        app.route_client_input(b"\x1b[112;7u".to_vec()); // C-M-p
        assert_eq!(app.state.active, Some(0));
        assert!(sent_bytes(&mut rx).is_empty());
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
        let _ = app
            .state
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
    async fn c_l_cycles_point_through_middle_top_and_bottom() {
        let (mut app, pane, _rx) = emacs_app_with_channel(&fifty_lines());
        enter_text_mode(&mut app);
        let point_row = 25;
        app.state.emacs.text_mode.as_mut().unwrap().point = Pos {
            row: point_row,
            col: 0,
        };

        let viewport_top = |app: &App| {
            let metrics = app
                .state
                .pane_scroll_metrics(&app.terminal_runtimes, pane)
                .expect("metrics");
            metrics.max_offset_from_bottom - metrics.offset_from_bottom
        };
        let viewport_rows = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane)
            .expect("metrics")
            .viewport_rows;

        app.route_client_input(vec![0x0c]);
        assert_eq!(viewport_top(&app), point_row as usize - viewport_rows / 2);
        app.route_client_input(vec![0x0c]);
        assert_eq!(viewport_top(&app), point_row as usize);
        app.route_client_input(vec![0x0c]);
        assert_eq!(viewport_top(&app), point_row as usize - (viewport_rows - 1));
        app.route_client_input(vec![0x0c]);
        assert_eq!(viewport_top(&app), point_row as usize - viewport_rows / 2);
        assert_eq!(
            app.state.emacs.text_mode.as_ref().unwrap().point.row,
            point_row,
            "recenter never moves point"
        );
    }

    #[tokio::test]
    async fn another_command_resets_c_l_cycle_to_middle() {
        let (mut app, pane, _rx) = emacs_app_with_channel(&fifty_lines());
        enter_text_mode(&mut app);
        app.state.emacs.text_mode.as_mut().unwrap().point = Pos { row: 25, col: 0 };

        app.route_client_input(vec![0x0c, 0x0c]); // middle, then top
        app.route_client_input(vec![0x0e]); // C-n
        let point_row = app.state.emacs.text_mode.as_ref().unwrap().point.row;
        app.route_client_input(vec![0x0c]);

        let metrics = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane)
            .expect("metrics");
        let top = metrics.max_offset_from_bottom - metrics.offset_from_bottom;
        assert_eq!(top, point_row as usize - metrics.viewport_rows / 2);
    }

    /// Finding 1: TEXT mode must not survive the user focusing a different
    /// pane. Enter TEXT mode on A, scroll its frozen view, then focus B (the
    /// same way a mouse click or `C-x o` would) and send a plain key: TEXT
    /// mode should auto-exit, A's scroll should restore to the entry offset,
    /// and the key should reach B's PTY as an ordinary live-mode keystroke.
    #[tokio::test]
    async fn focus_change_auto_exits_text_mode_and_routes_the_key_live() {
        let (mut app, pane_a, pane_b, _rx_a, mut rx_b) = emacs_app_with_two_panes(&fifty_lines());
        let entry_offset = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane_a)
            .expect("metrics")
            .offset_from_bottom;
        enter_text_mode(&mut app);
        assert_eq!(app.state.emacs.text_mode.as_ref().unwrap().pane_id, pane_a);
        app.route_client_input(vec![0x1b, b'<']); // M-<: scroll to the top
        let scrolled = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane_a)
            .expect("metrics")
            .offset_from_bottom;
        assert_ne!(scrolled, entry_offset, "TEXT mode moved A's viewport");

        // The user focuses pane B (mouse click / C-x o) while TEXT mode is
        // still nominally active on A.
        assert!(app.state.focus_pane_in_workspace(0, pane_b));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(pane_b));

        app.route_client_input(b"x".to_vec());

        assert!(
            app.state.emacs.text_mode.is_none(),
            "stale TEXT mode auto-exits on focus mismatch"
        );
        let restored = app
            .state
            .pane_scroll_metrics(&app.terminal_runtimes, pane_a)
            .expect("metrics")
            .offset_from_bottom;
        assert_eq!(restored, entry_offset, "exit restored A's scroll offset");
        assert_eq!(
            sent_bytes(&mut rx_b),
            b"x".to_vec(),
            "key reached B's PTY as a normal live-mode keystroke"
        );
    }

    /// Finding 1, second path: the TEXT-mode pane can also go stale by no
    /// longer resolving to a live runtime (closed/replaced) even while it's
    /// still nominally focused. That must auto-exit TEXT mode too.
    #[tokio::test]
    async fn text_mode_auto_exits_when_its_pane_runtime_is_gone() {
        let (mut app, pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        assert!(app.state.emacs.text_mode.is_some());
        app.state.workspaces[0].tabs[0].runtimes.remove(&pane);

        app.route_client_input(vec![b'x']);

        assert!(
            app.state.emacs.text_mode.is_none(),
            "TEXT mode exits when its pane no longer resolves to a live runtime"
        );
    }

    /// Spec §3.3: an unbound NON-self-inserting key in TEXT mode is
    /// "undefined", not "read-only". The old code said "Buffer is read-only"
    /// for every unbound single chord, which is wrong.
    #[tokio::test]
    async fn unbound_control_key_in_text_mode_is_undefined_not_read_only() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x14]); // C-t: bound nowhere
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-t is undefined"));
        assert!(sent_bytes(&mut rx).is_empty());
        assert!(app.state.emacs.text_mode.is_some(), "still in TEXT mode");
    }

    /// ...and an unbound META key likewise.
    #[tokio::test]
    async fn unbound_meta_key_in_text_mode_is_undefined() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'z']); // M-z
        assert_eq!(app.state.emacs.echo.as_deref(), Some("M-z is undefined"));
    }

    /// Read-only is reserved for keys that WOULD insert.
    #[tokio::test]
    async fn self_inserting_key_in_text_mode_reports_read_only() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![b'x']);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Buffer is read-only"));
        app.route_client_input(vec![b'5']);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Buffer is read-only"));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    /// An unbound multi-chord sequence in TEXT mode names the whole sequence.
    #[tokio::test]
    async fn unbound_sequence_in_text_mode_names_the_sequence() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x18, b'z']); // C-x z
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-x z is undefined"));
    }

    /// Live mode: a single unbound key belongs to the agent, silently.
    #[tokio::test]
    async fn unbound_single_key_in_live_mode_stays_silent_and_reaches_the_pane() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x14]); // C-t
        assert_eq!(app.state.emacs.echo, None, "no echo: the agent owns it");
        assert_eq!(sent_bytes(&mut rx), vec![0x14]);
    }

    /// Live mode: an unbound MULTI-chord sequence is the layer's own fault
    /// and must say so.
    #[tokio::test]
    async fn unbound_sequence_in_live_mode_is_undefined() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x18, b'z']); // C-x z
        assert_eq!(app.state.emacs.echo.as_deref(), Some("C-x z is undefined"));
        assert!(sent_bytes(&mut rx).is_empty());
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
    async fn repeated_c_n_extends_an_active_region_until_buffer_end() {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        app.route_client_input(vec![0x00]); // C-SPC

        let press = crate::input::TerminalKey::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        let repeat = press.with_kind(KeyEventKind::Repeat);
        app.route_client_events(
            vec![
                crate::raw_input::RawInputEvent::Key(press),
                crate::raw_input::RawInputEvent::Key(repeat),
                crate::raw_input::RawInputEvent::Key(repeat),
            ],
            false,
        );

        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!(text.point.row, 3, "press plus two repeats move three rows");
        assert_eq!(text.mark.map(|mark| mark.row), Some(0));
        assert!(text.mark_active);
    }

    #[tokio::test]
    async fn held_kitty_c_n_extends_an_active_region_through_raw_input() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        app.route_client_input(vec![0x00]); // C-SPC

        app.route_client_input(
            b"\x1b[110;5:1u\x1b[110;5:2u\x1b[110;5:2u\x1b[110;5:3u".to_vec(),
        );

        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!(text.point.row, 3, "press plus two repeats move three rows");
        assert_eq!(text.mark.map(|mark| mark.row), Some(0));
        assert!(text.mark_active);
    }

    #[tokio::test]
    async fn overlapping_c_spc_repeat_does_not_reset_a_moving_region() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<

        // A fast key roll can leave a Space auto-repeat queued after C-n has
        // already moved point. The late one-shot repeat must not set mark
        // again at row 1; C-n repeats must continue to row 3.
        app.route_client_input(
            b"\x1b[32;5:1u\x1b[110;5:1u\x1b[32;5:2u\x1b[110;5:2u\x1b[110;5:2u"
                .to_vec(),
        );

        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!(text.point.row, 3);
        assert_eq!(text.mark.map(|mark| mark.row), Some(0));
        assert!(text.mark_active);
    }

    #[tokio::test]
    async fn kitty_shifted_meta_comma_runs_beginning_of_buffer() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.state.emacs.text_mode.as_mut().unwrap().point = Pos { row: 3, col: 2 };

        // Ghostty REPORT_ALTERNATE_KEYS: physical comma, shifted `<`,
        // Shift+Alt modifiers, press event.
        app.route_client_input(b"\x1b[44:60;4:1u".to_vec());

        assert_eq!(
            app.state.emacs.text_mode.as_ref().unwrap().point,
            Pos { row: 0, col: 0 }
        );
    }

    #[tokio::test]
    async fn held_legacy_c_n_extends_an_active_region_through_raw_input() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        app.route_client_input(vec![0x00]); // C-SPC

        app.route_client_input(vec![0x0e, 0x0e, 0x0e]);

        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!(text.point.row, 3, "three legacy C-n bytes move three rows");
        assert_eq!(text.mark.map(|mark| mark.row), Some(0));
        assert!(text.mark_active);
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
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'g', b'g']);
        let before = app.state.emacs.text_mode.as_ref().unwrap().point;
        app.route_client_input(vec![b'9', 0x07]); // digit, then C-g
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!(text.goto_line, None);
        assert_eq!(text.point, before, "point untouched");
        assert!(
            sent_bytes(&mut rx).is_empty(),
            "prompt keys never reach the PTY"
        );
    }

    #[tokio::test]
    async fn goto_line_clamps_out_of_range_lines_into_the_buffer() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'>']); // M-> : learn the last row
        let last_row = app.state.emacs.text_mode.as_ref().unwrap().point.row;
        app.route_client_input(vec![0x1b, b'g', b'g']);
        app.route_client_input(b"1".to_vec());
        app.route_client_input(vec![0x0d]); // RET
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!((point.row, point.col), (0, 0), "line 1 is the first row");
        app.route_client_input(vec![0x1b, b'g', b'g']);
        app.route_client_input(b"9999".to_vec());
        app.route_client_input(vec![0x0d]); // RET
        let point = app.state.emacs.text_mode.as_ref().unwrap().point;
        assert_eq!(
            (point.row, point.col),
            (last_row, 0),
            "out-of-range line clamps to the last row"
        );
    }

    /// THE regression from the spec (§7.1): C-x [ then C-x 3 must split the
    /// window from inside TEXT mode. Before the keymap stack, TEXT mode
    /// consulted only the text keymap, so every global command was dead.
    #[tokio::test]
    async fn c_x_3_splits_the_window_from_inside_text_mode() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        assert_eq!(app.state.workspaces[0].tabs[0].panes.len(), 1);
        enter_text_mode(&mut app);
        assert!(app.state.emacs.text_mode.is_some());

        app.route_client_input(vec![0x18, b'3']); // C-x 3

        assert_eq!(
            app.state.workspaces[0].tabs[0].panes.len(),
            2,
            "global split-window-right fell through from the text keymap"
        );
        assert_ne!(
            app.state.emacs.echo.as_deref(),
            Some("C-x 3 is undefined"),
            "the sequence must not be reported undefined"
        );
    }

    /// Fallthrough for a pure-state action (no PTY spawn): C-x b in TEXT
    /// mode opens the navigator, the same way it does in live mode.
    #[tokio::test]
    async fn c_x_b_opens_the_navigator_from_text_mode() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x18, b'b']); // C-x b
        assert_eq!(app.state.mode, Mode::Navigator);
        assert!(sent_bytes(&mut rx).is_empty());
    }

    /// The text map still shadows the global one on the same sequence.
    #[tokio::test]
    async fn text_map_shadows_global_on_c_x_c_x() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<
        app.route_client_input(vec![0x00]); // C-SPC
        app.route_client_input(vec![0x0e]); // C-n
        app.route_client_input(vec![0x18, 0x18]); // C-x C-x
        let text = app.state.emacs.text_mode.as_ref().expect("still in TEXT");
        assert_eq!((text.point.row, text.point.col), (0, 0), "point <-> mark");
    }

    /// C-q is a global command, so the stack now reaches it in TEXT mode.
    /// A read-only buffer cannot quote-insert: say so instead of pushing a
    /// literal byte into the PTY behind the frozen view.
    #[tokio::test]
    async fn quoted_insert_in_text_mode_is_read_only() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(FIVE_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x11]); // C-q
        assert!(!app.state.emacs.quoted_insert);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Buffer is read-only"));
        app.route_client_input(vec![0x18]); // C-x: still a prefix, not a literal
        assert_eq!(app.state.emacs.pending.len(), 1);
        assert!(sent_bytes(&mut rx).is_empty());
    }

    /// C-x [ while TEXT mode is already on must not re-seed the session
    /// (it would clobber entry_offset_from_bottom and lose the point).
    #[tokio::test]
    async fn re_entering_text_mode_is_a_no_op() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(&fifty_lines());
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // M-<: point to row 0
        let before = app.state.emacs.text_mode.as_ref().unwrap().clone_for_test();
        app.route_client_input(vec![0x18, b'[']); // C-x [ again
        let after = app.state.emacs.text_mode.as_ref().unwrap().clone_for_test();
        assert_eq!(after, before, "TEXT mode session untouched");
    }

    /// A herdr action that has no default binding at all becomes reachable
    /// purely by naming it in config (spec §7.5).
    #[tokio::test]
    async fn a_config_bound_herdr_action_runs_without_a_code_change() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        let mut keys = std::collections::HashMap::new();
        keys.insert("C-x t".to_string(), "toggle-sidebar".to_string());
        app.state.emacs = crate::emacs::EmacsState::from_config(&crate::config::EmacsConfig {
            enabled: true,
            clipboard_sync: false,
            keys,
            ..Default::default()
        });
        let before = app.state.sidebar_collapsed;
        app.route_client_input(vec![0x18, b't']); // C-x t
        assert_ne!(app.state.sidebar_collapsed, before, "toggle-sidebar ran");
        assert!(sent_bytes(&mut rx).is_empty());
    }

    fn type_ascii(app: &mut App, text: &str) {
        app.route_client_input(text.as_bytes().to_vec());
    }

    fn open_feedback_prompt(app: &mut App) {
        app.route_client_input(vec![0x1b, b'x']); // M-x
        type_ascii(app, "feedback");
        app.route_client_input(vec![0x0d]); // RET
    }

    #[tokio::test]
    async fn m_x_feedback_sends_a_plain_comment() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(b"a".to_vec());
        assert_eq!(sent_bytes(&mut rx), b"a");

        open_feedback_prompt(&mut app);
        let prompt = app.state.emacs.minibuffer.as_ref().expect("feedback open");
        assert_eq!(
            prompt.kind,
            crate::emacs::minibuffer::MinibufferKind::Feedback
        );

        type_ascii(&mut app, "make tabs calmer");
        app.route_client_input(vec![0x0d]); // RET submits

        assert!(app.state.emacs.minibuffer.is_none());
        assert_eq!(sent_bytes(&mut rx), b"Herdr feedback:\nmake tabs calmer\r");
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Feedback sent"));
    }

    #[tokio::test]
    async fn feedback_dispatches_structural_editing_through_shared_text_input() {
        use crate::input::TerminalKey;
        use crossterm::event::{KeyCode, KeyModifiers};

        let (mut app, _pane, _rx) = emacs_app_with_channel(b"");
        app.state.emacs.minibuffer = Some(crate::emacs::minibuffer::MinibufferState::feedback());
        let minibuffer = app.state.emacs.minibuffer.as_mut().unwrap();
        minibuffer.insert_str("(héllo [world]) tail");
        minibuffer.move_beginning_of_line();

        let control_meta = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert!(app.emacs_intercept_key(TerminalKey::new(KeyCode::Char(' '), control_meta,)));
        assert!(app.emacs_intercept_key(TerminalKey::new(KeyCode::Char('k'), control_meta,)));

        assert_eq!(app.state.emacs.minibuffer.as_ref().unwrap().input, " tail");
        assert_eq!(app.state.emacs.kill_ring.head(), Some("(héllo [world])"));
    }

    #[tokio::test]
    async fn c_g_aborts_feedback_without_sending_anything() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        open_feedback_prompt(&mut app);
        type_ascii(&mut app, "never mind");
        app.route_client_input(vec![0x07]); // C-g
        assert!(app.state.emacs.minibuffer.is_none());
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Quit"));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn paste_edits_feedback_instead_of_reaching_the_pane() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        open_feedback_prompt(&mut app);
        app.handle_paste("pasted\ncomment".to_string()).await;
        assert_eq!(
            app.state.emacs.minibuffer.as_ref().unwrap().input,
            "pastedcomment",
            "the one-line minibuffer drops control characters"
        );
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn feedback_works_without_leaving_text_mode() {
        let (mut app, pane_id, mut rx) = emacs_app_with_channel(b"buffer text");
        app.emacs_enter_text_mode();
        assert_eq!(
            app.state.emacs.text_mode.as_ref().map(|text| text.pane_id),
            Some(pane_id)
        );

        open_feedback_prompt(&mut app);
        type_ascii(&mut app, "motion felt wrong");
        app.route_client_input(vec![0x0d]);

        assert_eq!(sent_bytes(&mut rx), b"Herdr feedback:\nmotion felt wrong\r");
        assert!(
            app.state.emacs.text_mode.is_some(),
            "submitting feedback preserves the frozen view"
        );
    }

    #[tokio::test]
    async fn empty_feedback_stays_open_with_a_visible_error() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        open_feedback_prompt(&mut app);
        app.route_client_input(vec![0x0d]);

        let prompt = app
            .state
            .emacs
            .minibuffer
            .as_ref()
            .expect("prompt remains open");
        assert!(prompt.prompt.starts_with("Feedback is empty"));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn c_x_question_mark_opens_the_bindings_cheat_sheet() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        app.route_client_input(vec![0x18, b'?']);

        assert_eq!(app.state.mode, crate::app::state::Mode::KeybindHelp);
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn m_x_refresh_herdr_requests_live_handoff_without_typing_into_pane() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");
        assert!(!app.state.request_live_handoff);

        app.route_client_input(vec![0x1b, b'x']); // M-x
        type_ascii(&mut app, "refresh-herdr");
        app.route_client_input(vec![0x0d]); // RET

        assert!(app.state.emacs.minibuffer.is_none());
        assert!(app.state.request_live_handoff);
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Refreshing Herdr…"));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn m_x_herdr_onboarding_opens_the_replayable_tour() {
        let (mut app, _pane, mut rx) = emacs_app_with_channel(b"");

        app.route_client_input(vec![0x1b, b'x']); // M-x
        type_ascii(&mut app, "herdr-onboarding");
        app.route_client_input(vec![0x0d]); // RET

        assert!(app.state.emacs.minibuffer.is_none());
        assert_eq!(app.state.mode, crate::app::state::Mode::Onboarding);
        assert_eq!(app.state.emacs_onboarding_page, Some(0));
        assert!(sent_bytes(&mut rx).is_empty());
    }

    const SEARCH_LINES: &[u8] = b"needle one\r\nmiddle\r\nneedle two\r\nbravo\r\nneedle three\r\n";

    #[tokio::test]
    async fn c_s_from_live_mode_incrementally_searches_and_repeats() {
        let (mut app, pane, mut rx) = emacs_app_with_channel(SEARCH_LINES);
        app.route_client_input(vec![0x13]); // C-s
        assert_eq!(
            app.state.emacs.text_mode.as_ref().map(|text| text.pane_id),
            Some(pane),
            "live search enters TEXT mode"
        );
        app.route_client_input(b"needle".to_vec());
        {
            let text = app.state.emacs.text_mode.as_ref().expect("TEXT mode");
            let search = text.isearch.as_ref().expect("isearch active");
            assert_eq!(search.matches.len(), 3);
            assert_eq!(search.current, None);
            assert!(search.failing, "forward search stops at the live bottom");
        }

        app.route_client_input(vec![0x13]); // repeat C-s wraps
        {
            let text = app.state.emacs.text_mode.as_ref().expect("TEXT mode");
            assert_eq!((text.point.row, text.point.col), (0, 6));
            assert!(text.isearch.as_ref().unwrap().wrapped);
        }
        app.route_client_input(vec![0x13]); // repeat C-s advances
        let text = app.state.emacs.text_mode.as_ref().expect("TEXT mode");
        assert_eq!((text.point.row, text.point.col), (2, 6));
        assert_eq!(
            text.isearch.as_ref().and_then(|search| search.current),
            Some(1)
        );
        assert!(
            sent_bytes(&mut rx).is_empty(),
            "search never reaches the PTY"
        );
    }

    #[tokio::test]
    async fn isearch_overlay_highlights_all_matches_and_the_current_one() {
        let (mut app, pane, _rx) = emacs_app_with_channel(SEARCH_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']);
        app.route_client_input(vec![0x13]);
        app.route_client_input(b"needle".to_vec());
        let inner = app.state.pane_info_by_id(pane).unwrap().inner_rect;
        let buffer = draw_text_mode_overlay(&app, pane);
        assert_eq!(
            buffer[(inner.x, inner.y)].style().bg,
            Some(app.state.palette.accent),
            "current match uses the accent"
        );
        assert_eq!(
            buffer[(inner.x, inner.y + 2)].style().bg,
            Some(app.state.palette.surface1),
            "other matches remain visible"
        );
    }

    #[tokio::test]
    async fn c_r_reverses_an_active_search_and_wraps() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(SEARCH_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']); // origin at buffer start
        app.route_client_input(vec![0x13]); // C-s
        app.route_client_input(b"needle".to_vec());
        app.route_client_input(vec![0x13]); // second match
        app.route_client_input(vec![0x12]); // C-r -> first match
        {
            let text = app.state.emacs.text_mode.as_ref().unwrap();
            assert_eq!((text.point.row, text.point.col), (0, 0));
            assert!(!text.isearch.as_ref().unwrap().wrapped);
        }
        app.route_client_input(vec![0x12]); // C-r -> wrap to last
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert_eq!((text.point.row, text.point.col), (4, 0));
        assert!(text.isearch.as_ref().unwrap().wrapped);
    }

    #[tokio::test]
    async fn failing_search_recovers_as_the_query_is_deleted() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(SEARCH_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']);
        let origin = app.state.emacs.text_mode.as_ref().unwrap().point;
        app.route_client_input(vec![0x13]);
        app.route_client_input(b"zzz".to_vec());
        {
            let text = app.state.emacs.text_mode.as_ref().unwrap();
            assert_eq!(text.point, origin);
            assert!(text.isearch.as_ref().unwrap().failing);
        }
        app.route_client_input(vec![0x7f, 0x7f, 0x7f]); // DEL x3
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        let search = text.isearch.as_ref().unwrap();
        assert_eq!(search.query, "");
        assert!(!search.failing);
        assert_eq!(text.point, origin);
    }

    #[tokio::test]
    async fn enter_accepts_search_and_c_g_aborts_to_the_origin() {
        let (mut app, pane, _rx) = emacs_app_with_channel(SEARCH_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']);
        let origin = app.state.emacs.text_mode.as_ref().unwrap().point;
        app.route_client_input(vec![0x13]);
        app.route_client_input(b"bravo".to_vec());
        app.route_client_input(vec![0x0d]); // RET
        {
            let text = app.state.emacs.text_mode.as_ref().unwrap();
            assert!(text.isearch.is_none());
            assert_eq!((text.point.row, text.point.col), (3, 5));
            assert_eq!(text.mark, Some(origin));
        }
        assert_eq!(app.state.emacs.search_ring.get(0), Some("bravo"));
        assert_eq!(
            app.state.emacs.mark_rings.get(&pane).map(|ring| ring.len()),
            Some(1)
        );

        app.route_client_input(vec![0x13]);
        app.route_client_input(b"needle".to_vec());
        assert_ne!(app.state.emacs.text_mode.as_ref().unwrap().point, origin);
        app.route_client_input(vec![0x07]); // C-g
        let text = app.state.emacs.text_mode.as_ref().unwrap();
        assert!(text.isearch.is_none());
        assert_eq!((text.point.row, text.point.col), (3, 5));
        assert_eq!(app.state.emacs.echo.as_deref(), Some("Quit"));
    }

    #[tokio::test]
    async fn c_s_c_s_reuses_the_last_search_and_history_can_be_browsed() {
        let (mut app, _pane, _rx) = emacs_app_with_channel(SEARCH_LINES);
        enter_text_mode(&mut app);
        app.route_client_input(vec![0x1b, b'<']);
        for query in ["needle", "bravo"] {
            app.route_client_input(vec![0x13]);
            app.route_client_input(query.as_bytes().to_vec());
            app.route_client_input(vec![0x0d]);
            app.route_client_input(vec![0x1b, b'<']);
        }

        app.route_client_input(vec![0x13, 0x13]); // start, then reuse last
        {
            let search = app
                .state
                .emacs
                .text_mode
                .as_ref()
                .and_then(|text| text.isearch.as_ref())
                .unwrap();
            assert_eq!(search.query, "bravo");
        }
        app.route_client_input(vec![0x07]); // leave the reused search
        app.route_client_input(vec![0x13]); // fresh empty search
        app.route_client_input(vec![0x1b, b'p']); // M-p: newest
        assert_eq!(
            app.state
                .emacs
                .text_mode
                .as_ref()
                .and_then(|text| text.isearch.as_ref())
                .map(|search| search.query.as_str()),
            Some("bravo")
        );
        app.route_client_input(vec![0x1b, b'p']); // M-p: older
        assert_eq!(
            app.state
                .emacs
                .text_mode
                .as_ref()
                .and_then(|text| text.isearch.as_ref())
                .map(|search| search.query.as_str()),
            Some("needle")
        );
        app.route_client_input(vec![0x1b, b'n']); // M-n: newer
        assert_eq!(
            app.state
                .emacs
                .text_mode
                .as_ref()
                .and_then(|text| text.isearch.as_ref())
                .map(|search| search.query.as_str()),
            Some("bravo")
        );
    }
}

#[cfg(test)]
mod conformance_tests;
