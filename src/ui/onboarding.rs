use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::widgets::{
    action_button_row_rects, action_button_width, modal_stack_areas, panel_contrast_fg,
    render_action_button, render_modal_header, render_modal_shell, ActionButtonSpec,
};
use crate::app::AppState;

const ONBOARDING_PREFIX_LABEL: &str = "ctrl+b";
pub(crate) const EMACS_ONBOARDING_MODAL_SIZE: (u16, u16) = (78, 21);
pub(crate) const EMACS_ONBOARDING_PAGE_COUNT: usize = 3;

pub(super) fn render_onboarding_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    super::dim_background(frame, area);
    if let Some(page) = app.emacs_onboarding_page {
        render_emacs_onboarding(app, frame, area, page);
    } else {
        render_onboarding_welcome(app, frame, area);
    }
}

pub(crate) fn onboarding_welcome_continue_rect(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        action_button_width(Some("↵"), "continue"),
        1,
    )
}

pub(crate) fn emacs_onboarding_button_rects(area: Rect, page: usize) -> (Option<Rect>, Rect) {
    let final_page = page + 1 >= EMACS_ONBOARDING_PAGE_COUNT;
    let buttons = if page == 0 {
        vec![ActionButtonSpec {
            hint: Some("↵"),
            label: if final_page { "done" } else { "next" },
        }]
    } else {
        vec![
            ActionButtonSpec {
                hint: Some("←"),
                label: "back",
            },
            ActionButtonSpec {
                hint: Some("↵"),
                label: if final_page { "done" } else { "next" },
            },
        ]
    };
    let rects = action_button_row_rects(area, &buttons, 2, 0);
    if page == 0 {
        (None, rects[0])
    } else {
        (Some(rects[0]), rects[1])
    }
}

fn emacs_key_line<'a>(app: &AppState, label: &'a str, key: &'a str, detail: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {label:<12}"),
            Style::default().fg(app.palette.text),
        ),
        Span::styled(
            format!("{key:<17}"),
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(detail, Style::default().fg(app.palette.overlay1)),
    ])
}

fn render_emacs_onboarding(app: &AppState, frame: &mut Frame, area: Rect, page: usize) {
    let (popup_w, popup_h) = EMACS_ONBOARDING_MODAL_SIZE;
    let Some(inner) = render_modal_shell(frame, area, popup_w, popup_h, &app.palette) else {
        return;
    };
    if inner.height < 15 {
        return;
    }

    let page = page.min(EMACS_ONBOARDING_PAGE_COUNT - 1);
    let stack = modal_stack_areas(inner, 2, 1, 1, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);
    let (title, content): (&str, Vec<Line<'_>>) = match page {
        0 => (
            "the shape of herdr",
            vec![
                emacs_key_line(app, "WORKSPACES", "left sidebar", "projects and contexts"),
                Line::from("       │"),
                emacs_key_line(app, "└─ TABS", "top row", "tabs inside that workspace"),
                Line::from("            │"),
                emacs_key_line(
                    app,
                    "   └─ PANES",
                    "center",
                    "terminals inside the active tab",
                ),
                Line::from(""),
                emacs_key_line(
                    app,
                    "AGENTS",
                    "lower left",
                    "active agents across your session",
                ),
                Line::styled(
                    "  Select an agent to jump straight to its pane, wherever it lives.",
                    Style::default().fg(app.palette.overlay1),
                ),
            ],
        ),
        1 => (
            "move through the session",
            vec![
                emacs_key_line(app, "workspace", "C-M-p / C-M-n", "previous / next"),
                emacs_key_line(app, "tab", "C-x p / C-x n", "previous / next"),
                emacs_key_line(app, "agent", "M-p / M-n", "previous / next"),
                emacs_key_line(app, "pane", "C-x o", "cycle through panes"),
                Line::from(""),
                Line::styled(
                    "  C means Ctrl · M means Alt/Meta · C-x and C-c start prefix sequences.",
                    Style::default().fg(app.palette.overlay1),
                ),
                Line::styled(
                    "  C-x n means: press Ctrl+x, release it, then press n.",
                    Style::default().fg(app.palette.overlay1),
                ),
            ],
        ),
        _ => (
            "mouse and keyboard work together",
            vec![
                Line::styled(
                    "  Click any workspace, tab, agent, or pane whenever that is faster.",
                    Style::default().fg(app.palette.overlay1),
                ),
                Line::styled(
                    "  Drag to reorder or resize; right-click for context actions.",
                    Style::default().fg(app.palette.overlay1),
                ),
                Line::from(""),
                emacs_key_line(app, "M-x", "commands", "run any named Herdr command"),
                emacs_key_line(app, "C-x ?", "bindings", "see every active binding"),
                emacs_key_line(app, "C-g", "cancel", "stop a partial command and return"),
                Line::from(""),
                Line::styled(
                    "  Replay: M-x herdr-onboarding or open menu → emacs tour.",
                    Style::default().fg(app.palette.overlay1),
                ),
            ],
        ),
    };

    render_modal_header(frame, header_rows[0], title, &app.palette);
    frame.render_widget(
        Paragraph::new(format!(
            "emacs tour · {} / {}",
            page + 1,
            EMACS_ONBOARDING_PAGE_COUNT
        ))
        .style(Style::default().fg(app.palette.overlay0)),
        header_rows[1],
    );
    frame.render_widget(Paragraph::new(content), stack.content);
    frame.render_widget(
        Paragraph::new("←/→ move · enter next · C-g/esc close")
            .style(Style::default().fg(app.palette.overlay0)),
        stack.footer.unwrap_or_default(),
    );

    let actions = stack.actions.unwrap_or_default();
    let (back_rect, next_rect) = emacs_onboarding_button_rects(actions, page);
    if let Some(back_rect) = back_rect {
        render_action_button(
            frame,
            back_rect,
            Some("←"),
            "back",
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0)
                .add_modifier(Modifier::BOLD),
        );
    }
    render_action_button(
        frame,
        next_rect,
        Some("↵"),
        if page + 1 == EMACS_ONBOARDING_PAGE_COUNT {
            "done"
        } else {
            "next"
        },
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
}

fn render_onboarding_welcome(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(inner) = render_modal_shell(frame, area, 64, 16, &app.palette) else {
        return;
    };
    if inner.height < 11 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 0, 1, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);
    let content_rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<4>(stack.content);

    frame.render_widget(
        Paragraph::new("  herdr").style(
            Style::default()
                .fg(app.palette.text)
                .add_modifier(Modifier::BOLD),
        ),
        header_rows[0],
    );
    frame.render_widget(
        Paragraph::new("  terminal workspace manager for coding agents")
            .style(Style::default().fg(app.palette.overlay0)),
        header_rows[1],
    );

    frame.render_widget(
        Paragraph::new(
            "  this is a mouse-first terminal.\n  click the sidebar to switch workspaces, drag pane\n  borders to resize, right-click for context menus.",
        )
        .style(Style::default().fg(app.palette.overlay1)),
        content_rows[0],
    );

    let key_line = Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            ONBOARDING_PREFIX_LABEL,
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " enters prefix mode · ",
            Style::default().fg(app.palette.overlay1),
        ),
        Span::styled(
            "?",
            Style::default()
                .fg(app.palette.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " shows keybinds and settings",
            Style::default().fg(app.palette.overlay1),
        ),
    ]);
    frame.render_widget(Paragraph::new(key_line), content_rows[2]);

    frame.render_widget(
        Paragraph::new("  next: install optional agent integrations for more reliable state")
            .style(Style::default().fg(app.palette.overlay1)),
        content_rows[3],
    );

    let continue_rect = onboarding_welcome_continue_rect(stack.actions.unwrap_or_default());
    render_action_button(
        frame,
        continue_rect,
        Some("↵"),
        "continue",
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn render_page(page: usize) -> String {
        let mut app = AppState::test_new();
        app.emacs_onboarding_page = Some(page);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        terminal
            .draw(|frame| render_onboarding_overlay(&app, frame, frame.area()))
            .expect("emacs onboarding should render");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn emacs_tour_renders_hierarchy_navigation_and_mouse_guidance() {
        let hierarchy = render_page(0);
        assert!(hierarchy.contains("WORKSPACES"));
        assert!(hierarchy.contains("TABS"));
        assert!(hierarchy.contains("AGENTS"));

        let navigation = render_page(1);
        assert!(navigation.contains("C-M-p / C-M-n"));
        assert!(navigation.contains("C-x p / C-x n"));
        assert!(navigation.contains("M-p / M-n"));

        let mouse = render_page(2);
        assert!(mouse.contains("Click any workspace, tab, agent, or pane"));
        assert!(mouse.contains("M-x herdr-onboarding"));
        assert!(mouse.contains("C-x ?"));
    }
}
