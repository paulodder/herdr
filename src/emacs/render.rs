//! Ratatui overlays for the Emacs layer. Pure draw functions called from
//! the two render seams in `src/ui.rs` / `src/ui/panes.rs`.

use ratatui::style::{Modifier, Style};
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
