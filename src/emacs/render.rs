//! Ratatui overlays for the Emacs layer. Pure draw functions called from
//! the two render seams in `src/ui.rs` / `src/ui/panes.rs`.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::AppState;
use crate::layout::PaneInfo;
use crate::terminal::TerminalRuntime;

/// TEXT-mode overlay for one pane: block point (region highlight lands in
/// a later task). Drawn after the pane grid, like copy-mode's cursor.
pub fn render_text_mode_overlay(
    app: &AppState,
    frame: &mut Frame,
    info: &PaneInfo,
    rt: &TerminalRuntime,
) {
    // Decision: gate on pane_id only, NOT focus. If focus moves to another
    // pane while TEXT mode is active, the TEXT pane keeps its point and the
    // focused pane shows its live cursor — one cursor per window, like Emacs.
    // (Emacs distinguishes them with filled-vs-hollow; possible later polish.)
    let Some(text) = app
        .emacs
        .text_mode
        .as_ref()
        .filter(|text| text.pane_id == info.id)
    else {
        return;
    };
    let Some(metrics) = rt.scroll_metrics() else {
        return;
    };
    let inner = info.inner_rect;
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let top = (metrics.max_offset_from_bottom - metrics.offset_from_bottom) as u32;

    // Region: min(point, mark)..max(point, mark), end-exclusive,
    // row-major (Pos derives Ord in that order).
    if text.mark_active {
        if let Some(mark) = text.mark {
            let (start, end) = if mark <= text.point {
                (mark, text.point)
            } else {
                (text.point, mark)
            };
            let style = Style::default().bg(app.palette.surface1);
            for rel_y in 0..inner.height {
                let row = top + u32::from(rel_y);
                if row < start.row || row > end.row {
                    continue;
                }
                let from = if row == start.row { start.col } else { 0 };
                let to = if row == end.row { end.col } else { inner.width };
                for x in from..to.min(inner.width) {
                    frame.buffer_mut()[(inner.x + x, inner.y + rel_y)].set_style(style);
                }
            }
        }
    }

    render_isearch_highlights(app, frame, info, rt, top, text);

    // Point: reversed+bold cell (theme-agnostic, like a block cursor).
    let Some(rel_row) = text.point.row.checked_sub(top) else {
        return;
    };
    let Ok(y) = u16::try_from(rel_row) else {
        return;
    };
    if y >= inner.height {
        return;
    }
    let x = text.point.col.min(inner.width.saturating_sub(1));
    let cell = &mut frame.buffer_mut()[(inner.x + x, inner.y + y)];
    cell.set_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD));
}

fn render_isearch_highlights(
    app: &AppState,
    frame: &mut Frame,
    info: &PaneInfo,
    rt: &TerminalRuntime,
    top: u32,
    text: &crate::emacs::text_mode::TextModeState,
) {
    let Some(search) = text
        .isearch
        .as_ref()
        .filter(|search| !search.query.is_empty())
    else {
        return;
    };
    let bottom = top.saturating_add(u32::from(info.inner_rect.height.saturating_sub(1)));
    let first_visible = search
        .matches
        .partition_point(|text_match| text_match.end.row < top);
    let visible = &search.matches[first_visible..];
    let visible_len = visible.partition_point(|text_match| text_match.start.row <= bottom);
    let candidates = &visible[..visible_len];
    let validity = rt.text_matches_are_current(candidates);

    for current_only in [false, true] {
        let style = if current_only {
            Style::default()
                .fg(app.palette.panel_bg)
                .bg(app.palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface1)
        };
        for (offset, (text_match, is_current)) in
            candidates.iter().zip(validity.iter().copied()).enumerate()
        {
            let index = first_visible + offset;
            if !is_current || (search.current == Some(index)) != current_only {
                continue;
            }
            let start_row = text_match.start.row.max(top);
            let end_row = text_match.end.row.min(bottom);
            for row in start_row..=end_row {
                let viewport_row = row.saturating_sub(top) as u16;
                let start_col = if row == text_match.start.row {
                    text_match.start.col
                } else {
                    0
                };
                let end_col = if row == text_match.end.row {
                    text_match.end.col
                } else {
                    info.inner_rect.width.saturating_sub(1)
                };
                for col in start_col..=end_col.min(info.inner_rect.width.saturating_sub(1)) {
                    frame.buffer_mut()[(info.inner_rect.x + col, info.inner_rect.y + viewport_row)]
                        .set_style(style);
                }
            }
        }
    }
}

/// One-line echo area drawn over the bottom row of the terminal area.
/// herdr has no persistent status line, so this is an overlay that only
/// appears when the layer has something to say (message, pending chord,
/// or the goto-line prompt). Phase 3's minibuffer takes over this surface.
pub fn render_echo_area(app: &AppState, frame: &mut Frame, terminal_area: Rect) {
    if terminal_area.height == 0 || terminal_area.width == 0 {
        return;
    }
    let content = if let Some(minibuffer) = app.emacs.minibuffer.as_ref() {
        minibuffer.render_line()
    } else if let Some(search) = app
        .emacs
        .text_mode
        .as_ref()
        .and_then(|text| text.isearch.as_ref())
    {
        let status = if search.failing {
            "Failing "
        } else if search.wrapped {
            "Wrapped "
        } else {
            ""
        };
        let direction = match search.direction {
            crate::emacs::isearch::SearchDirection::Forward => "",
            crate::emacs::isearch::SearchDirection::Backward => " backward",
        };
        format!("{status}I-search{direction}: {}", search.query)
    } else if let Some(prompt) = app
        .emacs
        .text_mode
        .as_ref()
        .and_then(|text| text.goto_line.as_deref())
    {
        format!("Goto line: {prompt}")
    } else if let Some(echo) = app.emacs.echo.as_deref() {
        echo.to_string()
    } else if !app.emacs.pending.is_empty() {
        format!("{}-", crate::emacs::keymap::format_seq(&app.emacs.pending))
    } else {
        return;
    };
    let area = Rect {
        x: terminal_area.x,
        y: terminal_area.y + terminal_area.height - 1,
        width: terminal_area.width,
        height: 1,
    };
    let paragraph = Paragraph::new(Line::from(content)).style(
        Style::default()
            .bg(app.palette.surface0)
            .fg(app.palette.text),
    );
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn bottom_row_text(state: &AppState, area: Rect) -> String {
        let backend = ratatui::backend::TestBackend::new(area.width, area.height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| render_echo_area(state, frame, area))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..area.width)
            .map(|x| {
                buffer[(x, area.height - 1)]
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' ')
            })
            .collect()
    }

    #[test]
    fn echo_area_shows_message_on_the_bottom_row() {
        let mut state = crate::app::AppState::test_new();
        state.emacs.echo = Some("Mark set".to_string());
        let text = bottom_row_text(&state, Rect::new(0, 0, 20, 5));
        assert!(text.starts_with("Mark set"), "{text:?}");
    }

    #[test]
    fn echo_area_shows_pending_chord_and_stays_silent_when_idle() {
        let mut state = crate::app::AppState::test_new();
        let idle = bottom_row_text(&state, Rect::new(0, 0, 20, 5));
        assert_eq!(idle.trim(), "", "no overlay when there is nothing to say");
        state.emacs.pending = crate::emacs::keymap::parse_key_seq("C-x").unwrap();
        state.emacs.echo = None;
        let pending = bottom_row_text(&state, Rect::new(0, 0, 20, 5));
        assert!(pending.starts_with("C-x-"), "{pending:?}");
    }

    #[test]
    fn echo_area_shows_goto_line_prompt() {
        let mut state = crate::app::AppState::test_new();
        state.emacs.text_mode = Some(crate::emacs::text_mode::TextModeState {
            pane_id: crate::layout::PaneId::alloc(),
            point: crate::emacs::text_mode::Pos { row: 0, col: 0 },
            mark: None,
            mark_active: false,
            entry_offset_from_bottom: 0,
            goto_line: Some("12".to_string()),
            isearch: None,
        });
        // The prompt outranks a stale echo message.
        state.emacs.echo = Some("Mark set".to_string());
        let text = bottom_row_text(&state, Rect::new(0, 0, 20, 5));
        assert!(text.starts_with("Goto line: 12"), "{text:?}");
    }

    #[test]
    fn echo_area_shows_feedback_comment() {
        let mut state = crate::app::AppState::test_new();
        let mut minibuffer = crate::emacs::minibuffer::MinibufferState::feedback();
        minibuffer.insert_str("tabs surprised me");
        state.emacs.minibuffer = Some(minibuffer);
        let text = bottom_row_text(&state, Rect::new(0, 0, 70, 5));
        assert!(text.starts_with("Feedback tabs surprised me"), "{text:?}");
    }

    #[test]
    fn echo_area_shows_incremental_search_state() {
        let mut state = crate::app::AppState::test_new();
        let mut search = crate::emacs::isearch::IsearchState::new(
            crate::emacs::isearch::SearchDirection::Backward,
            crate::emacs::text_mode::Pos { row: 2, col: 3 },
        );
        search.query = "missing".to_string();
        search.failing = true;
        state.emacs.text_mode = Some(crate::emacs::text_mode::TextModeState {
            pane_id: crate::layout::PaneId::alloc(),
            point: crate::emacs::text_mode::Pos { row: 2, col: 3 },
            mark: None,
            mark_active: false,
            entry_offset_from_bottom: 0,
            goto_line: None,
            isearch: Some(search),
        });
        let text = bottom_row_text(&state, Rect::new(0, 0, 50, 5));
        assert!(
            text.starts_with("Failing I-search backward: missing"),
            "{text:?}"
        );
    }
}
