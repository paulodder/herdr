use std::borrow::Cow;
use std::ops::Range;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use super::release_notes::release_notes_close_button_rect;
use super::scrollbar::{release_notes_scrollbar_rect, render_scrollbar};
use super::widgets::{
    modal_stack_areas, panel_contrast_fg, render_action_button, render_modal_header,
    render_modal_shell,
};
use crate::app::AppState;

pub(super) type HelpEntry = (String, Cow<'static, str>);
pub(super) type HelpGroup = (&'static str, Vec<HelpEntry>);

fn help_entry(key: impl Into<String>, label: &'static str) -> HelpEntry {
    (key.into(), Cow::Borrowed(label))
}

fn keybind_label(bindings: &crate::config::ActionKeybinds) -> String {
    bindings.label().unwrap_or_else(|| "unset".to_string())
}

fn indexed_label(bindings: &[crate::config::IndexedKeybind]) -> String {
    if bindings.is_empty() {
        return "unset".to_string();
    }

    let mut parts = Vec::new();
    let mut index = 0;
    while index < bindings.len() {
        if let Some(prefix) = indexed_range_prefix(&bindings[index..]) {
            parts.push(format!("{prefix}1..9"));
            index += 9;
        } else {
            parts.push(bindings[index].label.clone());
            index += 1;
        }
    }

    parts.join(" / ")
}

fn indexed_range_prefix(bindings: &[crate::config::IndexedKeybind]) -> Option<&str> {
    let run = bindings.get(..9)?;
    let prefix = run[0].label.strip_suffix('1')?;
    for (offset, binding) in run.iter().enumerate() {
        let digit = char::from(b'1' + offset as u8);
        if binding.label.strip_suffix(digit) != Some(prefix) {
            return None;
        }
    }
    Some(prefix)
}

fn emacs_help_group(
    title: &'static str,
    map: &crate::emacs::keymap::Keymap<crate::emacs::commands::EmacsCommand>,
) -> HelpGroup {
    let mut entries: Vec<HelpEntry> = map
        .bindings()
        .iter()
        .map(|(sequence, command)| {
            (
                crate::emacs::keymap::format_seq(sequence),
                Cow::Borrowed(command.name()),
            )
        })
        .collect();
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    (title, entries)
}

pub(super) fn keybind_help_groups(app: &AppState) -> Vec<HelpGroup> {
    let kb = &app.keybinds;
    let mut groups = Vec::new();

    if app.emacs.enabled {
        groups.push(emacs_help_group(
            "emacs — global / live",
            &app.emacs.keymaps.global,
        ));
        groups.push(emacs_help_group(
            "emacs — text mode",
            &app.emacs.keymaps.text,
        ));
        groups.push(emacs_help_group(
            "emacs — minibuffer",
            &app.emacs.keymaps.minibuffer,
        ));
    }

    groups.push((
        "global",
        vec![
            help_entry(
                crate::config::format_key_combo((app.prefix_code, app.prefix_mods)),
                "prefix mode",
            ),
            help_entry(keybind_label(&kb.help), "keybinds"),
            help_entry(keybind_label(&kb.settings), "settings"),
            help_entry(keybind_label(&kb.detach), "detach"),
            help_entry(keybind_label(&kb.reload_config), "reload config"),
            help_entry("C-p / up · C-n / down", "move selection / scroll"),
            help_entry(
                keybind_label(&kb.open_notification_target),
                "open notification target",
            ),
        ],
    ));

    groups.push((
        "navigation",
        vec![
            help_entry("esc", "back"),
            help_entry(
                format!(
                    "{} / {}",
                    keybind_label(&kb.navigate.workspace_up),
                    keybind_label(&kb.navigate.workspace_down)
                ),
                "workspace list",
            ),
            help_entry(
                format!(
                    "{} / {} / {} / {} / left / right",
                    keybind_label(&kb.navigate.pane_left),
                    keybind_label(&kb.navigate.pane_down),
                    keybind_label(&kb.navigate.pane_up),
                    keybind_label(&kb.navigate.pane_right)
                ),
                "move focus",
            ),
            help_entry("tab / shift+tab", "cycle pane"),
            help_entry("enter", "open workspace"),
            help_entry("1..9", "switch workspace"),
        ],
    ));

    let workspace_tab = vec![
        help_entry(keybind_label(&kb.workspace_picker), "workspace navigation"),
        help_entry(keybind_label(&kb.goto), "session navigator"),
        help_entry(keybind_label(&kb.new_workspace), "new workspace"),
        help_entry(keybind_label(&kb.new_worktree), "new worktree"),
        help_entry(keybind_label(&kb.open_worktree), "open worktree"),
        help_entry(
            keybind_label(&kb.remove_worktree),
            "delete worktree checkout",
        ),
        help_entry(keybind_label(&kb.rename_workspace), "rename workspace"),
        help_entry(keybind_label(&kb.close_workspace), "close workspace"),
        help_entry(keybind_label(&kb.previous_workspace), "previous workspace"),
        help_entry(keybind_label(&kb.next_workspace), "next workspace"),
        help_entry(indexed_label(&kb.switch_workspace), "switch workspace 1-9"),
        help_entry(keybind_label(&kb.previous_agent), "previous agent"),
        help_entry(keybind_label(&kb.next_agent), "next agent"),
        help_entry(indexed_label(&kb.focus_agent), "focus agent 1-9"),
        help_entry(keybind_label(&kb.new_tab), "new tab"),
        help_entry(keybind_label(&kb.rename_tab), "rename tab"),
        help_entry(keybind_label(&kb.previous_tab), "previous tab"),
        help_entry(keybind_label(&kb.next_tab), "next tab"),
        help_entry(indexed_label(&kb.switch_tab), "switch tab 1-9"),
        help_entry(keybind_label(&kb.close_tab), "close tab"),
    ];
    groups.push(("workspaces / tabs", workspace_tab));

    let panes = vec![
        help_entry(keybind_label(&kb.split_vertical), "split vertical"),
        help_entry(keybind_label(&kb.split_horizontal), "split horizontal"),
        help_entry(keybind_label(&kb.close_pane), "close pane"),
        help_entry(keybind_label(&kb.rename_pane), "rename pane"),
        help_entry(keybind_label(&kb.edit_scrollback), "edit scrollback"),
        help_entry(keybind_label(&kb.copy_mode), "copy mode"),
        help_entry(keybind_label(&kb.zoom), "zoom pane"),
        help_entry(keybind_label(&kb.resize_mode), "resize mode"),
        help_entry(keybind_label(&kb.toggle_sidebar), "toggle sidebar"),
        help_entry(keybind_label(&kb.focus_pane_left), "focus pane left"),
        help_entry(keybind_label(&kb.focus_pane_down), "focus pane down"),
        help_entry(keybind_label(&kb.focus_pane_up), "focus pane up"),
        help_entry(keybind_label(&kb.focus_pane_right), "focus pane right"),
        help_entry(keybind_label(&kb.cycle_pane_next), "cycle pane next"),
        help_entry(
            keybind_label(&kb.cycle_pane_previous),
            "cycle pane previous",
        ),
        help_entry(keybind_label(&kb.last_pane), "last pane"),
    ];
    groups.push(("panes", panes));

    if !kb.custom_commands.is_empty() {
        groups.push((
            "custom",
            kb.custom_commands
                .iter()
                .map(|binding| {
                    (
                        binding.label.clone(),
                        binding
                            .description
                            .clone()
                            .map(Cow::Owned)
                            .unwrap_or(Cow::Borrowed("custom command")),
                    )
                })
                .collect(),
        ));
    }

    let query = app.keybind_help.query.as_str();
    if query.is_empty() {
        return groups;
    }

    groups
        .into_iter()
        .filter_map(|(title, entries)| {
            let matching_entries = entries
                .into_iter()
                .filter(|(key, label)| {
                    contains_case_insensitive(key, query)
                        || contains_case_insensitive(label.as_ref(), query)
                })
                .collect::<Vec<_>>();
            (!matching_entries.is_empty()).then_some((title, matching_entries))
        })
        .collect()
}

fn contains_case_insensitive(text: &str, query: &str) -> bool {
    text.to_ascii_lowercase()
        .contains(&query.to_ascii_lowercase())
}

fn matching_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }

    let lowercase_text = text.to_ascii_lowercase();
    let lowercase_query = query.to_ascii_lowercase();
    lowercase_text
        .match_indices(&lowercase_query)
        .map(|(start, matched)| start..start + matched.len())
        .collect()
}

fn push_highlighted_spans(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    query: &str,
    base_style: Style,
    highlight_style: Style,
) {
    let ranges = matching_ranges(text, query);
    if ranges.is_empty() {
        spans.push(Span::styled(text.to_string(), base_style));
        return;
    }

    let mut cursor = 0;
    for range in ranges {
        if cursor < range.start {
            spans.push(Span::styled(
                text[cursor..range.start].to_string(),
                base_style,
            ));
        }
        spans.push(Span::styled(
            text[range.clone()].to_string(),
            highlight_style,
        ));
        cursor = range.end;
    }
    if cursor < text.len() {
        spans.push(Span::styled(text[cursor..].to_string(), base_style));
    }
}

pub(crate) fn keybind_help_lines(app: &AppState) -> Vec<(usize, Line<'static>)> {
    let heading_style = Style::default()
        .fg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(app.palette.mauve)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(app.palette.text);
    let highlight_style = Style::default()
        .fg(panel_contrast_fg(&app.palette))
        .bg(app.palette.accent)
        .add_modifier(Modifier::BOLD);
    let query = app.keybind_help.query.as_str();

    let groups = keybind_help_groups(app);
    if groups.is_empty() {
        const EMPTY_MESSAGE: &str = " no matching keybindings";
        return vec![(
            EMPTY_MESSAGE.chars().count(),
            Line::from(Span::styled(
                EMPTY_MESSAGE,
                Style::default().fg(app.palette.overlay1),
            )),
        )];
    }
    let key_width = groups
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|(key, _)| key.chars().count()))
        .max()
        .unwrap_or(8);

    let mut lines = Vec::new();

    for (group, entries) in groups {
        lines.push((
            group.len() + 1,
            Line::from(vec![Span::styled(format!(" {group}"), heading_style)]),
        ));
        for (key, label) in entries {
            let width = key_width + 2 + label.chars().count();
            let mut spans = vec![Span::styled(" ", key_style)];
            push_highlighted_spans(&mut spans, &key, query, key_style, highlight_style);
            spans.push(Span::styled(
                " ".repeat(key_width.saturating_sub(key.chars().count()) + 1),
                key_style,
            ));
            push_highlighted_spans(
                &mut spans,
                label.as_ref(),
                query,
                label_style,
                highlight_style,
            );
            lines.push((width, Line::from(spans)));
        }
        lines.push((0, Line::raw("")));
    }

    lines
}

pub(super) fn render_keybind_help_overlay(app: &AppState, frame: &mut Frame) {
    super::dim_background(frame, frame.area());

    let Some(inner) = render_modal_shell(frame, frame.area(), 76, 22, &app.palette) else {
        return;
    };
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 1, 0, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);

    render_modal_header(frame, header_rows[0], "keybinds", &app.palette);
    render_action_button(
        frame,
        release_notes_close_button_rect(header_rows[0]),
        Some("esc"),
        "close",
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    let query = app.keybind_help.query.as_str();
    let search_text = if query.is_empty() {
        Span::styled(
            "type to search commands",
            Style::default().fg(app.palette.overlay0),
        )
    } else {
        Span::styled(
            app.keybind_help.query_input.with_cursor(query),
            Style::default().fg(app.palette.text),
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " / ",
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            search_text,
        ])),
        header_rows[1],
    );

    let body_area = stack.content;
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: app
            .keybind_help_max_scroll()
            .saturating_sub(app.keybind_help.scroll) as usize,
        max_offset_from_bottom: app.keybind_help_max_scroll() as usize,
        viewport_rows: body_area.height.max(1) as usize,
    };
    let track = release_notes_scrollbar_rect(body_area, metrics);
    let text_area = track
        .map(|_| {
            Rect::new(
                body_area.x,
                body_area.y,
                body_area.width.saturating_sub(1),
                body_area.height,
            )
        })
        .unwrap_or(body_area);

    let body = Paragraph::new(
        keybind_help_lines(app)
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .scroll((app.keybind_help.scroll, 0));
    frame.render_widget(body, text_area);
    if let Some(track) = track {
        render_scrollbar(
            frame,
            metrics,
            track,
            app.palette.overlay0,
            app.palette.overlay1,
            "▐",
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" search ", Style::default().fg(app.palette.overlay0)),
            Span::styled("type", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("erase ", Style::default().fg(app.palette.overlay0)),
            Span::styled("backspace", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("scroll ", Style::default().fg(app.palette.overlay0)),
            Span::styled("↑↓ / pgup / pgdn", Style::default().fg(app.palette.text)),
            Span::styled("  ·  ", Style::default().fg(app.palette.overlay0)),
            Span::styled("close ", Style::default().fg(app.palette.overlay0)),
            Span::styled("esc / enter", Style::default().fg(app.palette.text)),
        ])),
        stack.footer.unwrap_or_default(),
    );
}
