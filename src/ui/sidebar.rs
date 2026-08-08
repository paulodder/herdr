mod tokens;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use self::tokens::{ResolvedToken, SpaceTokenContext};
use super::scrollbar::{render_scrollbar, should_show_scrollbar};
use super::status::{agent_icon, state_dot, state_label, state_label_color};
use super::text::{display_width, display_width_u16, truncate_end};
use crate::app::state::{AgentPanelSort, Palette};
use crate::app::{AppState, Mode};
use crate::detect::AgentState;
use crate::terminal::TerminalRuntimeRegistry;

const WORKSPACE_SECTION_HEADER_ROWS: u16 = 2;
const AGENT_PANEL_HEADER_ROWS: u16 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentPanelTarget {
    Local {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: crate::layout::PaneId,
    },
    Remote {
        endpoint_id: String,
        pane_id: String,
    },
}

impl AgentPanelTarget {
    pub(crate) fn navigator_target(&self) -> crate::app::state::NavigatorTarget {
        match self {
            Self::Local {
                ws_idx,
                tab_idx,
                pane_id,
            } => crate::app::state::NavigatorTarget::Pane {
                ws_idx: *ws_idx,
                tab_idx: *tab_idx,
                pane_id: *pane_id,
            },
            Self::Remote {
                endpoint_id,
                pane_id,
            } => crate::app::state::NavigatorTarget::RemotePane {
                endpoint_id: endpoint_id.clone(),
                pane_id: pane_id.clone(),
            },
        }
    }

    pub(crate) fn is_active(&self, app: &AppState) -> bool {
        match self {
            Self::Local {
                ws_idx,
                tab_idx,
                pane_id,
            } => app.is_active_pane(*ws_idx, *tab_idx, *pane_id),
            Self::Remote { .. } => false,
        }
    }
}

pub(crate) struct AgentPanelEntry {
    pub target: AgentPanelTarget,
    pub primary_label: String,
    pub location: String,
    pub primary_tab_label: Option<String>,
    pub pane_label: Option<String>,
    pub terminal_title: Option<String>,
    pub terminal_title_stripped: Option<String>,
    pub agent_label: Option<String>,
    pub agent: Option<crate::detect::Agent>,
    pub state: AgentState,
    pub seen: bool,
    pub last_agent_state_change_seq: Option<u64>,
    pub state_labels: std::collections::HashMap<String, String>,
    pub tokens: std::collections::HashMap<String, String>,
}

fn sidebar_section_heights(total_h: u16, split_ratio: f32) -> (u16, u16) {
    if total_h == 0 {
        return (0, 0);
    }

    if total_h < 6 {
        let ws_h = total_h.div_ceil(2);
        return (ws_h, total_h.saturating_sub(ws_h));
    }

    let ratio = split_ratio.clamp(0.1, 0.9);
    let ws_h = ((total_h as f32) * ratio).round() as u16;
    let ws_h = ws_h.clamp(3, total_h.saturating_sub(3));
    let detail_h = total_h.saturating_sub(ws_h);
    (ws_h, detail_h)
}

pub(crate) fn expanded_sidebar_sections(area: Rect, split_ratio: f32) -> (Rect, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), Rect::default());
    }

    let (ws_h, detail_h) = sidebar_section_heights(content.height, split_ratio);
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h);
    let detail_area = Rect::new(content.x, content.y + ws_h, content.width, detail_h);
    (ws_area, detail_area)
}

pub(crate) fn sidebar_section_divider_rect(area: Rect, split_ratio: f32) -> Rect {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height < 6 {
        return Rect::default();
    }

    let (ws_h, _) = sidebar_section_heights(content.height, split_ratio);
    Rect::new(content.x, content.y + ws_h, content.width, 1)
}

fn agent_panel_sort_label(sort: AgentPanelSort) -> &'static str {
    match sort {
        AgentPanelSort::Spaces => "grouped",
        AgentPanelSort::Priority => "priority",
    }
}

pub(crate) fn agent_panel_toggle_rect(area: Rect, sort: AgentPanelSort) -> Rect {
    if area.width == 0 || area.height < 2 {
        return Rect::default();
    }

    let label = agent_panel_sort_label(sort);
    let width = display_width_u16(label);
    Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + 1,
        width,
        1,
    )
}

pub(crate) fn agent_panel_entries(app: &AppState) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, None)
}

pub(crate) fn agent_panel_entries_from(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<AgentPanelEntry> {
    agent_panel_entries_with_runtimes(app, Some(terminal_runtimes))
}

fn agent_panel_entries_with_runtimes(
    app: &AppState,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> Vec<AgentPanelEntry> {
    let empty_runtimes;
    let terminal_runtimes = match terminal_runtimes {
        Some(terminal_runtimes) => terminal_runtimes,
        None => {
            empty_runtimes = TerminalRuntimeRegistry::new();
            &empty_runtimes
        }
    };

    let local_entries: Vec<_> = app
        .workspaces
        .iter()
        .enumerate()
        .flat_map(|(ws_idx, ws)| {
            let multi_tab = ws.tabs.len() > 1;
            let workspace_label = ws.base_display_name_from(&app.terminals, terminal_runtimes);
            ws.pane_details(&app.terminals)
                .into_iter()
                .map(move |detail| {
                    let show_tab = multi_tab
                        || ws
                            .tabs
                            .get(detail.tab_idx)
                            .is_some_and(|tab| !tab.is_auto_named());
                    AgentPanelEntry {
                        target: AgentPanelTarget::Local {
                            ws_idx,
                            tab_idx: detail.tab_idx,
                            pane_id: detail.pane_id,
                        },
                        primary_label: workspace_label.clone(),
                        location: app.federation_member_id.clone(),
                        primary_tab_label: show_tab.then_some(detail.tab_label),
                        pane_label: detail.pane_label,
                        terminal_title: detail.terminal_title,
                        terminal_title_stripped: detail.terminal_title_stripped,
                        agent_label: Some(detail.agent_label),
                        agent: detail.agent,
                        state: detail.state,
                        seen: detail.seen,
                        last_agent_state_change_seq: detail.last_agent_state_change_seq,
                        state_labels: detail.state_labels,
                        tokens: detail.tokens,
                    }
                })
        })
        .collect();

    let mut entries_by_member = std::collections::BTreeMap::new();
    entries_by_member.insert(app.federation_member_id.clone(), local_entries);
    for endpoint in app.federation_states() {
        if endpoint.endpoint.id == app.federation_member_id {
            continue;
        }
        let Some(snapshot) = endpoint.snapshot.as_ref() else {
            continue;
        };
        let mut remote_entries = Vec::new();
        for agent in &snapshot.agents {
            let workspace = snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == agent.workspace_id);
            let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
            let Some(workspace) = workspace else {
                continue;
            };
            let multi_tab = workspace.tab_count > 1;
            let tab_label = tab.map(|tab| {
                crate::metadata_tokens::unqualified_name(&tab.label, &endpoint.endpoint.id)
                    .to_string()
            });
            let show_tab = multi_tab || tab_label.as_deref().is_some_and(|label| !label.is_empty());
            let agent_label = agent
                .display_agent
                .clone()
                .or_else(|| agent.name.clone())
                .or_else(|| agent.agent.clone());
            let parsed_agent = agent
                .agent
                .as_deref()
                .or(agent.display_agent.as_deref())
                .and_then(crate::detect::parse_agent_label);
            let (state, seen) = agent_status_state(agent.agent_status);
            remote_entries.push(AgentPanelEntry {
                target: AgentPanelTarget::Remote {
                    endpoint_id: endpoint.endpoint.id.clone(),
                    pane_id: agent.pane_id.clone(),
                },
                primary_label: crate::metadata_tokens::unqualified_name(
                    &workspace.label,
                    &endpoint.endpoint.id,
                )
                .to_string(),
                location: endpoint.endpoint.id.clone(),
                primary_tab_label: show_tab.then_some(tab_label).flatten(),
                pane_label: agent.title.clone(),
                terminal_title: agent.terminal_title.clone(),
                terminal_title_stripped: agent.terminal_title_stripped.clone(),
                agent_label,
                agent: parsed_agent,
                state,
                seen,
                last_agent_state_change_seq: Some(agent.revision),
                state_labels: agent.state_labels.clone(),
                tokens: agent.tokens.clone(),
            });
        }
        entries_by_member.insert(endpoint.endpoint.id.clone(), remote_entries);
    }

    let mut entries: Vec<_> = entries_by_member.into_values().flatten().collect();

    if matches!(app.agent_panel_sort, AgentPanelSort::Priority) {
        entries.sort_by_key(|entry| {
            (
                std::cmp::Reverse(workspace_attention_priority(entry.state, entry.seen)),
                entry.location.clone(),
                std::cmp::Reverse(entry.last_agent_state_change_seq),
            )
        });
    }

    entries
}

pub(super) fn agent_panel_status_key(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Working, _) => "working",
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Unknown, _) => "unknown",
    }
}

fn workspace_row_height(app: &AppState, ws: &crate::workspace::Workspace, indented: bool) -> u16 {
    let (state, seen) = ws.aggregate_state(&app.terminals);
    let label = if indented {
        grouped_child_display_label(
            &ws.base_display_name(),
            ws.branch().as_deref(),
            ws.custom_name.is_some(),
        )
    } else {
        ws.base_display_name()
    };
    let token_values = ws.metadata_tokens.values();
    tokens::space_rows(
        &app.sidebar_spaces,
        SpaceTokenContext {
            workspace: &label,
            branch: ws.branch().as_deref(),
            state_text: state_label(state, seen),
            ahead_behind: ws.git_ahead_behind(),
            tokens: &token_values,
            suppress_git_details: indented,
        },
    )
    .len()
    .max(1)
    .min(u16::MAX as usize) as u16
}

fn workspace_row_height_in_body(
    app: &AppState,
    workspace: &crate::workspace::Workspace,
    indented: bool,
    body_height: u16,
) -> u16 {
    workspace_row_height(app, workspace, indented).min(body_height)
}

fn workspace_entry_gap(entries: &[WorkspaceListEntry], entry_idx: usize, indented: bool) -> u16 {
    u16::from(
        entry_idx + 1 < entries.len()
            && !(indented && next_entry_is_indented_workspace(entries, entry_idx)),
    )
}

fn workspace_attention_priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Blocked, _) => 4,
        (AgentState::Idle, false) => 3,
        (AgentState::Working, _) => 2,
        (AgentState::Idle, true) => 1,
        (AgentState::Unknown, _) => 0,
    }
}

pub(crate) fn agent_status_state(status: crate::api::schema::AgentStatus) -> (AgentState, bool) {
    match status {
        crate::api::schema::AgentStatus::Blocked => (AgentState::Blocked, true),
        crate::api::schema::AgentStatus::Working => (AgentState::Working, true),
        crate::api::schema::AgentStatus::Done => (AgentState::Idle, false),
        crate::api::schema::AgentStatus::Idle => (AgentState::Idle, true),
        crate::api::schema::AgentStatus::Unknown => (AgentState::Unknown, true),
    }
}

fn space_aggregate_state(app: &AppState, key: &str) -> (AgentState, bool) {
    let local = app
        .workspaces
        .iter()
        .filter(|ws| ws.worktree_space().is_some_and(|space| space.key == key))
        .map(|ws| ws.aggregate_state(&app.terminals));
    let remote = app.federation_states().flat_map(|endpoint| {
        endpoint
            .snapshot
            .iter()
            .flat_map(|snapshot| snapshot.workspaces.iter())
            .filter(|workspace| {
                workspace
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.repo_key == key)
            })
            .map(|workspace| agent_status_state(workspace.agent_status))
    });
    local
        .chain(remote)
        .max_by_key(|(state, seen)| workspace_attention_priority(*state, *seen))
        .unwrap_or((AgentState::Unknown, true))
}

pub(crate) fn workspace_parent_group_state(
    app: &AppState,
    ws_idx: usize,
) -> Option<(String, bool)> {
    let space = app.workspaces.get(ws_idx)?.worktree_space()?;
    if space.is_linked_worktree {
        return None;
    }
    let member_count = app
        .workspaces
        .iter()
        .filter(|ws| {
            ws.worktree_space()
                .is_some_and(|member| member.key == space.key)
        })
        .count();
    (member_count >= 2).then(|| {
        (
            space.key.clone(),
            app.collapsed_space_keys.contains(&space.key),
        )
    })
}

pub(crate) fn grouped_child_display_label(
    label: &str,
    branch: Option<&str>,
    has_custom_name: bool,
) -> String {
    if has_custom_name {
        return label.to_string();
    }
    let Some(branch) = branch else {
        return label.to_string();
    };
    branch
        .strip_prefix("worktree/")
        .unwrap_or(branch)
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspaceListEntry {
    Workspace {
        ws_idx: usize,
        indented: bool,
    },
    RemoteWorkspace {
        endpoint_id: String,
        workspace_id: String,
        indented: bool,
    },
}

pub(crate) fn next_entry_is_indented_workspace(entries: &[WorkspaceListEntry], idx: usize) -> bool {
    matches!(
        entries.get(idx.saturating_add(1)),
        Some(
            WorkspaceListEntry::Workspace { indented: true, .. }
                | WorkspaceListEntry::RemoteWorkspace { indented: true, .. }
        )
    )
}

pub(crate) fn normalized_workspace_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    let body = workspace_list_body_rect(ws_area, false);
    if body.height == 0 {
        return requested;
    }

    if workspace_list_entries(app).is_empty() {
        0
    } else {
        requested.min(workspace_list_bottom_start(app, ws_area))
    }
}

pub(crate) fn workspace_list_entries(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, false)
}

/// Like [`workspace_list_entries`] but always expands worktree groups, ignoring
/// `collapsed_space_keys`. The mobile switcher has no collapse affordance and
/// always shows the full worktree tree.
pub(crate) fn workspace_list_entries_expanded(app: &AppState) -> Vec<WorkspaceListEntry> {
    workspace_list_entries_inner(app, true)
}

fn workspace_list_entries_inner(app: &AppState, force_expanded: bool) -> Vec<WorkspaceListEntry> {
    #[derive(Clone)]
    struct Candidate {
        entry: WorkspaceListEntry,
        worktree_key: Option<String>,
        linked_worktree: bool,
    }

    // The active member changes when the client crosses a federation boundary,
    // but the sidebar must not. Build one canonical member-ordered directory,
    // then group worktrees across that whole directory by repository identity.
    let mut entries_by_member = std::collections::BTreeMap::<String, Vec<Candidate>>::new();
    entries_by_member.insert(
        app.federation_member_id.clone(),
        app.workspaces
            .iter()
            .enumerate()
            .map(|(ws_idx, workspace)| Candidate {
                entry: WorkspaceListEntry::Workspace {
                    ws_idx,
                    indented: false,
                },
                worktree_key: workspace.worktree_space().map(|space| space.key.clone()),
                linked_worktree: workspace
                    .worktree_space()
                    .is_some_and(|space| space.is_linked_worktree),
            })
            .collect(),
    );
    for endpoint in app.federation_states() {
        if endpoint.endpoint.id == app.federation_member_id {
            continue;
        }
        let Some(snapshot) = endpoint.snapshot.as_ref() else {
            continue;
        };
        entries_by_member.insert(
            endpoint.endpoint.id.clone(),
            snapshot
                .workspaces
                .iter()
                .map(|workspace| Candidate {
                    entry: WorkspaceListEntry::RemoteWorkspace {
                        endpoint_id: endpoint.endpoint.id.clone(),
                        workspace_id: workspace.workspace_id.clone(),
                        indented: false,
                    },
                    worktree_key: workspace
                        .worktree
                        .as_ref()
                        .map(|worktree| worktree.repo_key.clone()),
                    linked_worktree: workspace
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| worktree.is_linked_worktree),
                })
                .collect(),
        );
    }

    let candidates = entries_by_member
        .into_values()
        .flatten()
        .collect::<Vec<_>>();
    let mut members_by_key = std::collections::HashMap::<String, Vec<usize>>::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(key) = &candidate.worktree_key {
            members_by_key.entry(key.clone()).or_default().push(index);
        }
    }
    let grouped_keys = members_by_key
        .iter()
        .filter(|(_, members)| {
            members.len() >= 2
                && members
                    .iter()
                    .any(|index| !candidates[*index].linked_worktree)
        })
        .map(|(key, _)| key.clone())
        .collect::<std::collections::HashSet<_>>();

    let visible_workspace_idx = if matches!(app.mode, Mode::Navigate) {
        Some(app.selected)
    } else {
        app.active
    };
    let active_candidate = visible_workspace_idx.and_then(|ws_idx| {
        candidates.iter().position(|candidate| {
            matches!(
                candidate.entry,
                WorkspaceListEntry::Workspace {
                    ws_idx: candidate_ws_idx,
                    ..
                } if candidate_ws_idx == ws_idx
            )
        })
    });
    let active_group = active_candidate
        .and_then(|index| candidates[index].worktree_key.as_deref())
        .map(str::to_string);

    let with_indentation = |entry: &WorkspaceListEntry, indented| match entry {
        WorkspaceListEntry::Workspace { ws_idx, .. } => WorkspaceListEntry::Workspace {
            ws_idx: *ws_idx,
            indented,
        },
        WorkspaceListEntry::RemoteWorkspace {
            endpoint_id,
            workspace_id,
            ..
        } => WorkspaceListEntry::RemoteWorkspace {
            endpoint_id: endpoint_id.clone(),
            workspace_id: workspace_id.clone(),
            indented,
        },
    };

    let mut emitted_groups = std::collections::HashSet::<String>::new();
    let mut entries = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let Some(key) = candidate
            .worktree_key
            .as_ref()
            .filter(|key| grouped_keys.contains(*key))
        else {
            entries.push(with_indentation(&candidate.entry, false));
            continue;
        };
        if !emitted_groups.insert(key.clone()) {
            continue;
        }

        let Some(members) = members_by_key.get(key) else {
            continue;
        };
        let parent = members
            .iter()
            .copied()
            .find(|member| !candidates[*member].linked_worktree)
            .unwrap_or(index);
        entries.push(with_indentation(&candidates[parent].entry, false));

        let collapsed = !force_expanded && app.collapsed_space_keys.contains(key);
        if collapsed {
            if let Some(active) = active_candidate
                .filter(|active| *active != parent)
                .filter(|_| active_group.as_deref() == Some(key.as_str()))
            {
                entries.push(with_indentation(&candidates[active].entry, true));
            }
        } else {
            for member in members.iter().copied().filter(|member| *member != parent) {
                entries.push(with_indentation(&candidates[member].entry, true));
            }
        }
    }
    entries
}

pub(crate) fn remote_workspace<'a>(
    app: &'a AppState,
    endpoint_id: &str,
    workspace_id: &str,
) -> Option<(
    &'a crate::federation::EndpointState,
    &'a crate::api::schema::WorkspaceInfo,
)> {
    let endpoint = app.federation_state(endpoint_id)?;
    let workspace = endpoint
        .snapshot
        .as_ref()?
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == workspace_id)?;
    Some((endpoint, workspace))
}

fn workspace_entry_group_key(app: &AppState, entry: &WorkspaceListEntry) -> Option<String> {
    match entry {
        WorkspaceListEntry::Workspace { ws_idx, .. } => app
            .workspaces
            .get(*ws_idx)?
            .worktree_space()
            .map(|space| space.key.clone()),
        WorkspaceListEntry::RemoteWorkspace {
            endpoint_id,
            workspace_id,
            ..
        } => remote_workspace(app, endpoint_id, workspace_id)?
            .1
            .worktree
            .as_ref()
            .map(|worktree| worktree.repo_key.clone()),
    }
}

fn remote_workspace_row_height(
    app: &AppState,
    endpoint_id: &str,
    workspace_id: &str,
    indented: bool,
    body_height: u16,
) -> u16 {
    let Some((endpoint, workspace)) = remote_workspace(app, endpoint_id, workspace_id) else {
        return 0;
    };
    let (state, seen) = agent_status_state(workspace.agent_status);
    let label = crate::metadata_tokens::unqualified_name(&workspace.label, &endpoint.endpoint.id);
    (tokens::space_rows(
        &app.sidebar_spaces,
        SpaceTokenContext {
            workspace: label,
            branch: workspace.branch.as_deref(),
            state_text: state_label(state, seen),
            ahead_behind: workspace.git_ahead_behind,
            tokens: &workspace.tokens,
            suppress_git_details: indented,
        },
    )
    .len()
    .max(1)
    .min(u16::MAX as usize) as u16)
        .min(body_height)
}

pub(crate) fn workspace_list_rect(area: Rect, split_ratio: f32) -> Rect {
    let (ws_area, _) = expanded_sidebar_sections(area, split_ratio);
    ws_area
}

pub(crate) fn workspace_list_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= WORKSPACE_SECTION_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(WORKSPACE_SECTION_HEADER_ROWS);
    let footer_y = area.y + area.height.saturating_sub(1);
    let body_height = footer_y.saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn workspace_list_visible_count(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = workspace_list_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let (row_height, gap) = match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                (
                    workspace_row_height_in_body(app, ws, *indented, body.height),
                    workspace_entry_gap(&entries, entry_idx, *indented),
                )
            }
            WorkspaceListEntry::RemoteWorkspace {
                endpoint_id,
                workspace_id,
                indented,
            } => (
                remote_workspace_row_height(app, endpoint_id, workspace_id, *indented, body.height),
                workspace_entry_gap(&entries, entry_idx, *indented),
            ),
        };
        if used_rows.saturating_add(row_height) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(row_height);
        visible += 1;
        if gap > 0 && used_rows < body.height {
            used_rows = used_rows.saturating_add(1);
        }
    }
    visible
}

fn workspace_list_bottom_start(app: &AppState, area: Rect) -> usize {
    let body = workspace_list_body_rect(area, false);
    let entries = workspace_list_entries(app);
    let mut used_rows = 0u16;
    let mut start = entries.len();
    for (entry_idx, entry) in entries.iter().enumerate().rev() {
        let (height, indented) = match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(workspace) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                (
                    workspace_row_height_in_body(app, workspace, *indented, body.height),
                    *indented,
                )
            }
            WorkspaceListEntry::RemoteWorkspace {
                endpoint_id,
                workspace_id,
                indented,
            } => (
                remote_workspace_row_height(app, endpoint_id, workspace_id, *indented, body.height),
                *indented,
            ),
        };
        let gap = workspace_entry_gap(&entries, entry_idx, indented);
        let needed = height.saturating_add(gap);
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        start = entry_idx;
    }
    start.min(entries.len().saturating_sub(1))
}

pub(crate) fn workspace_list_scroll_metrics(
    app: &AppState,
    area: Rect,
) -> crate::pane::ScrollMetrics {
    let max_scroll = workspace_list_bottom_start(app, area);
    let scroll = app.workspace_scroll.min(max_scroll);
    let viewport_rows = workspace_list_visible_count(app, area, scroll);

    crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows,
    }
}

pub(crate) fn workspace_list_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = workspace_list_scroll_metrics(app, area);
    let body = workspace_list_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn agent_panel_body_rect(area: Rect, has_scrollbar: bool) -> Rect {
    if area.width == 0 || area.height <= AGENT_PANEL_HEADER_ROWS {
        return Rect::default();
    }

    let body_y = area.y.saturating_add(AGENT_PANEL_HEADER_ROWS);
    let body_height = (area.y + area.height).saturating_sub(body_y);
    let body_width = area.width.saturating_sub(u16::from(has_scrollbar));
    Rect::new(area.x, body_y, body_width, body_height)
}

fn resolved_agent_rows(app: &AppState, entry: &AgentPanelEntry) -> Vec<Vec<ResolvedToken>> {
    let label = entry
        .state_labels
        .get(agent_panel_status_key(entry.state, entry.seen))
        .map(String::as_str)
        .unwrap_or_else(|| state_label(entry.state, entry.seen));
    tokens::agent_rows(&app.sidebar_agents, entry, label)
}

fn trailing_workspace(resolved: &[ResolvedToken]) -> Option<(&[ResolvedToken], &str)> {
    let (last, leading) = resolved.split_last()?;
    match last {
        ResolvedToken::Workspace(workspace) if !leading.is_empty() => {
            Some((leading, workspace.as_str()))
        }
        _ => None,
    }
}

pub(crate) fn agent_entry_height_in_body(
    app: &AppState,
    entry: &AgentPanelEntry,
    body_height: u16,
) -> u16 {
    (resolved_agent_rows(app, entry)
        .len()
        .max(1)
        .min(u16::MAX as usize) as u16)
        .min(body_height)
}

fn agent_panel_visible_count_from(app: &AppState, area: Rect, scroll: usize) -> usize {
    let body = agent_panel_body_rect(area, false);
    if body.width == 0 || body.height == 0 {
        return 0;
    }

    let mut used_rows = 0u16;
    let mut visible = 0usize;
    for entry in agent_panel_entries(app).iter().skip(scroll) {
        let height = agent_entry_height_in_body(app, entry, body.height);
        if used_rows.saturating_add(height) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(height);
        visible += 1;
        if used_rows < body.height {
            used_rows = used_rows.saturating_add(1);
        }
    }
    visible
}

fn agent_panel_bottom_start(app: &AppState, area: Rect) -> usize {
    let body = agent_panel_body_rect(area, false);
    let entries = agent_panel_entries(app);
    let mut used_rows = 0u16;
    let mut start = entries.len();
    for (index, entry) in entries.iter().enumerate().rev() {
        let gap = u16::from(index + 1 < entries.len());
        let needed = agent_entry_height_in_body(app, entry, body.height).saturating_add(gap);
        if used_rows.saturating_add(needed) > body.height {
            break;
        }
        used_rows = used_rows.saturating_add(needed);
        start = index;
    }
    start.min(entries.len().saturating_sub(1))
}

pub(crate) fn agent_panel_scroll_for_target(
    app: &AppState,
    area: Rect,
    current_scroll: usize,
    target: usize,
) -> usize {
    let max_scroll = agent_panel_bottom_start(app, area);
    if target < current_scroll {
        return target.min(max_scroll);
    }
    let mut scroll = current_scroll.min(max_scroll);
    while scroll < target {
        let visible = agent_panel_visible_count_from(app, area, scroll);
        if visible > 0 && target < scroll.saturating_add(visible) {
            break;
        }
        scroll += 1;
    }
    scroll.min(max_scroll)
}

pub(crate) fn agent_panel_scroll_metrics(app: &AppState, area: Rect) -> crate::pane::ScrollMetrics {
    let max_scroll = agent_panel_bottom_start(app, area);
    let scroll = app.agent_panel_scroll.min(max_scroll);
    let viewport_rows = agent_panel_visible_count_from(app, area, scroll);

    crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows,
    }
}

pub(crate) fn agent_panel_scrollbar_rect(app: &AppState, area: Rect) -> Option<Rect> {
    let metrics = agent_panel_scroll_metrics(app, area);
    let body = agent_panel_body_rect(area, true);
    (should_show_scrollbar(metrics) && body.width > 0 && body.height > 0).then_some(Rect::new(
        area.x + area.width.saturating_sub(1),
        body.y,
        1,
        body.height,
    ))
}

pub(crate) fn compute_workspace_list_areas(
    app: &AppState,
    area: Rect,
) -> (Vec<crate::app::state::WorkspaceCardArea>, Vec<()>) {
    let ws_area = workspace_list_rect(area, app.sidebar_section_split);
    if ws_area == Rect::default() {
        return (Vec::new(), Vec::new());
    }

    let metrics = workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(ws_area, should_show_scrollbar(metrics));
    if body.width == 0 || body.height == 0 {
        return (Vec::new(), Vec::new());
    }

    let scroll = app.workspace_scroll;
    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    let mut cards = Vec::new();
    let headers = Vec::new();

    let entries = workspace_list_entries(app);
    for (entry_idx, entry) in entries.iter().enumerate().skip(scroll) {
        let indented = match entry {
            WorkspaceListEntry::Workspace { indented, .. }
            | WorkspaceListEntry::RemoteWorkspace { indented, .. } => *indented,
        };
        let group_parent = !indented && next_entry_is_indented_workspace(&entries, entry_idx);
        let group_key = group_parent
            .then(|| workspace_entry_group_key(app, entry))
            .flatten();
        match entry {
            WorkspaceListEntry::Workspace { ws_idx, indented } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let row_height = workspace_row_height_in_body(app, ws, *indented, body.height);
                let gap = workspace_entry_gap(&entries, entry_idx, *indented);
                if row_y.saturating_add(row_height) > body_bottom {
                    break;
                }
                cards.push(crate::app::state::WorkspaceCardArea {
                    ws_idx: *ws_idx,
                    remote: None,
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    indented: *indented,
                    group_key,
                    group_parent,
                });
                row_y = row_y.saturating_add(row_height);
                if gap > 0 && row_y < body_bottom {
                    row_y = row_y.saturating_add(1);
                }
            }
            WorkspaceListEntry::RemoteWorkspace {
                endpoint_id,
                workspace_id,
                indented,
            } => {
                let row_height = remote_workspace_row_height(
                    app,
                    endpoint_id,
                    workspace_id,
                    *indented,
                    body.height,
                );
                let gap = workspace_entry_gap(&entries, entry_idx, *indented);
                if row_y.saturating_add(row_height) > body_bottom {
                    break;
                }
                cards.push(crate::app::state::WorkspaceCardArea {
                    ws_idx: usize::MAX,
                    remote: Some(crate::app::state::FederatedWorkspaceTarget {
                        endpoint_id: endpoint_id.clone(),
                        workspace_id: workspace_id.clone(),
                    }),
                    rect: Rect::new(body.x, row_y, body.width, row_height),
                    indented: *indented,
                    group_key,
                    group_parent,
                });
                row_y = row_y.saturating_add(row_height);
                if gap > 0 && row_y < body_bottom {
                    row_y = row_y.saturating_add(1);
                }
            }
        }
    }

    (cards, headers)
}

pub(crate) fn compute_workspace_card_areas(
    app: &AppState,
    area: Rect,
) -> Vec<crate::app::state::WorkspaceCardArea> {
    compute_workspace_list_areas(app, area).0
}

/// Auto-scale sidebar width based on workspace identity + agent summary.
pub(crate) fn collapsed_sidebar_sections(area: Rect) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.width == 0 || content.height == 0 {
        return (Rect::default(), None, Rect::default());
    }

    if content.height < 7 {
        return (content, None, Rect::default());
    }

    let total_h = content.height as usize;
    let ws_h = total_h.div_ceil(2);
    let detail_h = total_h.saturating_sub(ws_h + 1);
    if ws_h == 0 || detail_h == 0 {
        return (content, None, Rect::default());
    }

    let divider_y = content.y + ws_h as u16;
    let ws_area = Rect::new(content.x, content.y, content.width, ws_h as u16);
    let detail_area = Rect::new(content.x, divider_y + 1, content.width, detail_h as u16);
    (ws_area, Some(divider_y), detail_area)
}

/// Collapsed sidebar: workspace glance on top, compact agent list below.
pub(super) fn render_sidebar_collapsed(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let is_navigating = matches!(app.mode, Mode::Navigate);

    let p = &app.palette;
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };
    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    if ws_area == Rect::default() {
        render_sidebar_toggle(app, frame, area, true, p);
        return;
    }

    for (visible_idx, entry) in workspace_list_entries(app).iter().enumerate() {
        let y = ws_area.y + visible_idx as u16;
        if y >= ws_area.y + ws_area.height {
            break;
        }
        let (agg_state, agg_seen, local_ws_idx) = match entry {
            WorkspaceListEntry::Workspace { ws_idx, .. } => {
                let Some(ws) = app.workspaces.get(*ws_idx) else {
                    continue;
                };
                let (state, seen) = ws.aggregate_state(&app.terminals);
                (state, seen, Some(*ws_idx))
            }
            WorkspaceListEntry::RemoteWorkspace {
                endpoint_id,
                workspace_id,
                ..
            } => {
                let Some((_, workspace)) = remote_workspace(app, endpoint_id, workspace_id) else {
                    continue;
                };
                let (state, seen) = agent_status_state(workspace.agent_status);
                (state, seen, None)
            }
        };
        let (icon, icon_style) = state_dot(agg_state, agg_seen, p);
        let is_selected = local_ws_idx == Some(app.selected) && is_navigating;
        let is_active = local_ws_idx == app.active;
        let row_style = if is_selected {
            Style::default().bg(p.surface0)
        } else if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };
        let num_style = if is_selected {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else if is_active {
            Style::default().fg(p.text).bg(p.surface_dim)
        } else {
            Style::default().fg(p.overlay0)
        };

        if is_selected || is_active {
            let buf = frame.buffer_mut();
            for x in ws_area.x..ws_area.x + ws_area.width {
                buf[(x, y)].set_style(row_style);
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{}", visible_idx + 1), num_style),
                Span::styled(" ", row_style),
                Span::styled(icon, icon_style),
            ])),
            Rect::new(ws_area.x, y, ws_area.width, 1),
        );
    }

    if let Some(divider_y) = divider_y {
        let buf = frame.buffer_mut();
        for x in ws_area.x..ws_area.x + ws_area.width {
            buf[(x, divider_y)].set_symbol("─");
            buf[(x, divider_y)].set_style(Style::default().fg(p.surface_dim));
        }
    }

    let detail_content_area = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    if detail_content_area != Rect::default() {
        for (detail_idx, detail) in agent_panel_entries(app).iter().enumerate() {
            let y = detail_content_area.y + detail_idx as u16;
            if y >= detail_content_area.y + detail_content_area.height {
                break;
            }
            let position = detail_idx + 1;
            let position_style = Style::default().fg(p.overlay0);
            let (icon, icon_style) = agent_icon(detail.state, detail.seen, app.spinner_tick, p);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!("{position:<2}"), position_style),
                    Span::styled(icon, icon_style),
                ])),
                Rect::new(detail_content_area.x, y, detail_content_area.width, 1),
            );
        }
    }

    render_sidebar_toggle(app, frame, area, true, p);
}

pub(crate) fn workspace_drop_indicator_row(
    cards: &[crate::app::state::WorkspaceCardArea],
    area: Rect,
    insert_idx: usize,
) -> Option<u16> {
    if area.height == 0 {
        return None;
    }
    let list_bottom = area.y + area.height.saturating_sub(1);

    let local_cards = cards
        .iter()
        .filter(|card| card.remote.is_none())
        .collect::<Vec<_>>();
    let first = *local_cards.first()?;
    if insert_idx == first.ws_idx {
        return first.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    if let Some(row) = local_cards
        .last()
        .filter(|card| insert_idx == card.ws_idx.saturating_add(1))
        .map(|card| card.rect.y.saturating_add(card.rect.height))
        .filter(|y| *y < list_bottom)
    {
        return Some(row);
    }

    if let Some(card) = local_cards.iter().find(|card| card.ws_idx == insert_idx) {
        return card.rect.y.checked_sub(1).filter(|y| *y < list_bottom);
    }

    None
}

fn render_remote_workspace_card(
    app: &AppState,
    frame: &mut Frame,
    card: &crate::app::state::WorkspaceCardArea,
    list_bottom: u16,
) -> bool {
    let Some(target) = card.remote.as_ref() else {
        return false;
    };
    let Some((endpoint, workspace)) =
        remote_workspace(app, &target.endpoint_id, &target.workspace_id)
    else {
        return true;
    };
    let p = &app.palette;
    let parent_group = card
        .group_key
        .as_ref()
        .filter(|_| card.group_parent)
        .map(|key| (key, app.collapsed_space_keys.contains(key.as_str())));
    let (state, seen) = parent_group
        .as_ref()
        .filter(|(_, collapsed)| *collapsed)
        .map(|(key, _)| space_aggregate_state(app, key))
        .unwrap_or_else(|| agent_status_state(workspace.agent_status));
    let label = crate::metadata_tokens::unqualified_name(&workspace.label, &endpoint.endpoint.id);
    let state_icon = state_dot(state, seen, p);
    let state_text_style = Style::default()
        .fg(state_label_color(state, seen, p))
        .add_modifier(Modifier::DIM);
    let secondary_style = Style::default().fg(p.overlay0);
    let rows = tokens::space_rows(
        &app.sidebar_spaces,
        SpaceTokenContext {
            workspace: label,
            branch: workspace.branch.as_deref(),
            state_text: state_label(state, seen),
            ahead_behind: workspace.git_ahead_behind,
            tokens: &workspace.tokens,
            suppress_git_details: card.indented,
        },
    );
    for (row_index, resolved) in rows.iter().enumerate() {
        if row_index as u16 >= card.rect.height || card.rect.y + row_index as u16 >= list_bottom {
            break;
        }
        let mut spans = Vec::new();
        if row_index == 0 {
            if card.indented {
                spans.push(Span::raw("   "));
            } else if let Some((_, collapsed)) = parent_group {
                spans.push(Span::styled(
                    if collapsed { "▸" } else { "▾" },
                    Style::default().fg(p.accent),
                ));
                spans.push(Span::raw(" "));
            } else {
                spans.push(Span::raw(" "));
            }
        } else if card.indented {
            spans.push(Span::raw("     "));
        } else {
            spans.push(Span::raw("   "));
        }
        let prefix_width = if row_index == 0 {
            if card.indented {
                3
            } else if parent_group.is_some() {
                2
            } else {
                1
            }
        } else if card.indented {
            5
        } else {
            3
        };
        let location_width = if row_index == 0 {
            workspace_location_width(&endpoint.endpoint.id, card.rect.width, prefix_width)
        } else {
            0
        };
        let content_width = card
            .rect
            .width
            .saturating_sub(prefix_width)
            .saturating_sub(location_width)
            .saturating_sub(u16::from(location_width > 0));
        spans.extend(resolved_token_spans(
            resolved,
            state_icon,
            state_text_style,
            Style::default().fg(p.subtext0),
            secondary_style,
            secondary_style,
            secondary_style,
            p,
            content_width as usize,
        ));
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(
                card.rect.x,
                card.rect.y + row_index as u16,
                card.rect.width,
                1,
            ),
        );
        if row_index == 0 {
            render_workspace_location(frame, card.rect, &endpoint.endpoint.id, location_width, p);
        }
    }
    true
}

fn workspace_location_width(location: &str, row_width: u16, prefix_width: u16) -> u16 {
    let available = row_width.saturating_sub(prefix_width);
    let max_location_width = available / 2;
    display_width_u16(location).min(max_location_width)
}

fn render_workspace_location(
    frame: &mut Frame,
    row: Rect,
    location: &str,
    width: u16,
    palette: &Palette,
) {
    if width == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            truncate_end(location, width as usize),
            Style::default()
                .fg(palette.overlay0)
                .add_modifier(Modifier::DIM),
        ))
        .alignment(Alignment::Right),
        Rect::new(row.x + row.width.saturating_sub(width), row.y, width, 1),
    );
}

pub(super) fn render_sidebar(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;
    let is_navigating = matches!(app.mode, Mode::Navigate);
    let sep_style = if is_navigating {
        Style::default().fg(p.accent)
    } else {
        Style::default().fg(p.surface_dim)
    };

    let sep_x = area.x + area.width.saturating_sub(1);
    let buf = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        buf[(sep_x, y)].set_symbol("│");
        buf[(sep_x, y)].set_style(sep_style);
    }

    let (ws_area, detail_area) = expanded_sidebar_sections(area, app.sidebar_section_split);

    render_workspace_list(app, terminal_runtimes, frame, ws_area, is_navigating);
    render_agent_detail(app, terminal_runtimes, frame, detail_area);
    render_sidebar_toggle(app, frame, area, false, p);
}

fn resolved_token_spans(
    resolved: &[ResolvedToken],
    state_icon: (&str, Style),
    state_text_style: Style,
    workspace_style: Style,
    secondary_style: Style,
    agent_style: Style,
    custom_style: Style,
    p: &Palette,
    max_width: usize,
) -> Vec<Span<'static>> {
    let fixed_widths = resolved
        .iter()
        .map(|token| match token {
            ResolvedToken::StateIcon => display_width(state_icon.0),
            ResolvedToken::GitStatus { ahead, behind } => {
                usize::from(*ahead > 0) * display_width(&format!("↑{ahead}"))
                    + usize::from(*behind > 0) * display_width(&format!("↓{behind}"))
                    + usize::from(*ahead > 0 && *behind > 0)
            }
            _ => 0,
        })
        .collect::<Vec<_>>();
    let flexible_widths = resolved
        .iter()
        .map(|token| match token {
            ResolvedToken::StateText(text)
            | ResolvedToken::Workspace(text)
            | ResolvedToken::Tab(text)
            | ResolvedToken::Pane(text)
            | ResolvedToken::Agent(text)
            | ResolvedToken::TerminalTitle(text)
            | ResolvedToken::Branch(text)
            | ResolvedToken::Custom(text) => display_width(text),
            _ => 0,
        })
        .collect::<Vec<_>>();
    let minimum_width = |active: &[bool]| {
        let indices = active
            .iter()
            .enumerate()
            .filter_map(|(index, active)| active.then_some(index))
            .collect::<Vec<_>>();
        let content = indices
            .iter()
            .map(|index| fixed_widths[*index] + usize::from(flexible_widths[*index] > 0))
            .sum::<usize>();
        let separators = indices
            .windows(2)
            .map(|pair| display_width(tokens::separator(&resolved[pair[0]], &resolved[pair[1]])))
            .sum::<usize>();
        content + separators
    };
    let mut active = resolved.iter().map(|_| true).collect::<Vec<_>>();
    if minimum_width(&active) > max_width {
        for (index, width) in flexible_widths.iter().enumerate() {
            if *width > 0 {
                active[index] = false;
            }
        }
        for index in (0..resolved.len()).rev() {
            if flexible_widths[index] == 0 {
                continue;
            }
            active[index] = true;
            if minimum_width(&active) > max_width {
                active[index] = false;
            }
        }
    }
    let visible_indices = active
        .iter()
        .enumerate()
        .filter_map(|(index, active)| active.then_some(index))
        .collect::<Vec<_>>();
    let separator_width = visible_indices
        .windows(2)
        .map(|pair| display_width(tokens::separator(&resolved[pair[0]], &resolved[pair[1]])))
        .sum::<usize>();
    let fixed_width = visible_indices
        .iter()
        .map(|index| fixed_widths[*index])
        .sum::<usize>();
    let mut budgets = flexible_widths
        .iter()
        .enumerate()
        .map(|(index, width)| usize::from(active[index] && *width > 0))
        .collect::<Vec<_>>();
    let minimum = budgets.iter().sum::<usize>();
    let mut remaining = max_width
        .saturating_sub(separator_width + fixed_width)
        .saturating_sub(minimum);
    while remaining > 0 {
        let mut grew = false;
        for (budget, width) in budgets.iter_mut().zip(&flexible_widths) {
            if *budget > 0 && *budget < *width {
                *budget += 1;
                remaining -= 1;
                grew = true;
                if remaining == 0 {
                    break;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut spans = Vec::new();
    for (position, index) in visible_indices.iter().copied().enumerate() {
        let token = &resolved[index];
        if position > 0 {
            let previous = &resolved[visible_indices[position - 1]];
            spans.push(Span::styled(
                tokens::separator(previous, token),
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ));
        }
        match token {
            ResolvedToken::StateIcon => {
                spans.push(Span::styled(state_icon.0.to_string(), state_icon.1));
            }
            ResolvedToken::StateText(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    state_text_style,
                ));
            }
            ResolvedToken::Workspace(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    workspace_style,
                ));
            }
            ResolvedToken::Tab(text) | ResolvedToken::Pane(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    secondary_style,
                ));
            }
            ResolvedToken::Agent(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    agent_style,
                ));
            }
            ResolvedToken::Branch(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    secondary_style,
                ));
            }
            ResolvedToken::GitStatus { ahead, behind } => {
                if *ahead > 0 {
                    spans.push(Span::styled(
                        format!("↑{ahead}"),
                        Style::default().fg(p.green),
                    ));
                }
                if *ahead > 0 && *behind > 0 {
                    spans.push(Span::raw(" "));
                }
                if *behind > 0 {
                    spans.push(Span::styled(
                        format!("↓{behind}"),
                        Style::default().fg(p.red),
                    ));
                }
            }
            ResolvedToken::TerminalTitle(text) | ResolvedToken::Custom(text) => {
                spans.push(Span::styled(
                    truncate_end(text, budgets[index]),
                    custom_style,
                ));
            }
        }
    }
    spans
}

fn render_workspace_list(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
    is_navigating: bool,
) {
    let p = &app.palette;
    let dragged_ws_idx = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder { source_ws_idx, .. }) => {
            Some(*source_ws_idx)
        }
        _ => None,
    };
    let insertion_row = match app.drag.as_ref().map(|drag| &drag.target) {
        Some(crate::app::state::DragTarget::WorkspaceReorder {
            insert_idx: Some(insert_idx),
            ..
        }) => workspace_drop_indicator_row(&app.view.workspace_card_areas, area, *insert_idx),
        _ => None,
    };

    let list_bottom = area.y + area.height.saturating_sub(1);
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " spaces",
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            )])),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    let metrics = workspace_list_scroll_metrics(app, area);
    let scrollbar_rect = workspace_list_scrollbar_rect(app, area);
    let cards = &app.view.workspace_card_areas;

    for card in cards {
        if render_remote_workspace_card(app, frame, card, list_bottom) {
            continue;
        }
        let i = card.ws_idx;
        let ws = &app.workspaces[i];
        let row_y = card.rect.y;
        let row_height = card.rect.height;
        let selected = i == app.selected && is_navigating;
        let is_active = Some(i) == app.active;
        let is_dragged = dragged_ws_idx == Some(i);
        let highlighted = selected || is_active || is_dragged;
        let (agg_state, agg_seen) = ws.aggregate_state(&app.terminals);

        if highlighted {
            let bg = if selected {
                p.surface0
            } else if is_dragged {
                p.surface1
            } else {
                p.surface_dim
            };
            let buf = frame.buffer_mut();
            for y in row_y..row_y + row_height {
                if y >= list_bottom {
                    break;
                }
                for x in card.rect.x..card.rect.x + card.rect.width {
                    buf[(x, y)].set_style(Style::default().bg(bg));
                }
            }
        }

        let name_style = if selected || is_active || is_dragged {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };

        let label = ws.base_display_name_from(&app.terminals, terminal_runtimes);
        let display_label = if card.indented {
            grouped_child_display_label(&label, ws.branch().as_deref(), ws.custom_name.is_some())
        } else {
            label
        };
        let parent_group = card
            .group_key
            .as_ref()
            .filter(|_| card.group_parent)
            .map(|key| (key.clone(), app.collapsed_space_keys.contains(key)));
        let (display_state, display_seen) = parent_group
            .as_ref()
            .filter(|(_, collapsed)| *collapsed)
            .map(|(key, _)| space_aggregate_state(app, key))
            .unwrap_or((agg_state, agg_seen));
        let state_icon = state_dot(display_state, display_seen, p);
        let state_text_style = Style::default()
            .fg(state_label_color(display_state, display_seen, p))
            .add_modifier(Modifier::DIM);
        let branch_style = Style::default().fg(if selected || is_active {
            p.mauve
        } else {
            p.overlay0
        });
        let token_values = ws.metadata_tokens.values();
        let rows = tokens::space_rows(
            &app.sidebar_spaces,
            SpaceTokenContext {
                workspace: &display_label,
                branch: ws.branch().as_deref(),
                state_text: state_label(display_state, display_seen),
                ahead_behind: ws.git_ahead_behind(),
                tokens: &token_values,
                suppress_git_details: card.indented,
            },
        );

        for (row_index, resolved) in rows.iter().enumerate() {
            if row_index as u16 >= row_height || row_y + row_index as u16 >= list_bottom {
                break;
            }
            let mut spans = Vec::new();
            if row_index == 0 {
                if card.indented {
                    spans.push(Span::raw("   "));
                } else if let Some((_, collapsed)) = parent_group.as_ref() {
                    spans.push(Span::styled(
                        if *collapsed { "▸" } else { "▾" },
                        Style::default().fg(p.accent),
                    ));
                    spans.push(Span::raw(" "));
                } else {
                    spans.push(Span::raw(" "));
                }
            } else {
                spans.push(Span::raw(if card.indented { "     " } else { "   " }));
            }
            let prefix_width = if row_index == 0 {
                if card.indented {
                    3
                } else if parent_group.is_some() {
                    2
                } else {
                    1
                }
            } else if card.indented {
                5
            } else {
                3
            };
            let location_width = if row_index == 0 && app.federation_states().next().is_some() {
                workspace_location_width(&app.federation_member_id, card.rect.width, prefix_width)
            } else {
                0
            };
            let content_width = card
                .rect
                .width
                .saturating_sub(prefix_width)
                .saturating_sub(location_width)
                .saturating_sub(u16::from(location_width > 0));
            spans.extend(resolved_token_spans(
                resolved,
                state_icon,
                state_text_style,
                name_style,
                branch_style,
                branch_style,
                branch_style,
                p,
                content_width as usize,
            ));
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect::new(card.rect.x, row_y + row_index as u16, card.rect.width, 1),
            );
            if row_index == 0 {
                render_workspace_location(
                    frame,
                    card.rect,
                    &app.federation_member_id,
                    location_width,
                    p,
                );
            }
        }
    }

    if let Some(y) = insertion_row.filter(|y| *y < list_bottom) {
        let indicator_right = scrollbar_rect
            .map(|rect| rect.x)
            .unwrap_or(area.x + area.width);
        let buf = frame.buffer_mut();
        for x in area.x..indicator_right {
            buf[(x, y)].set_symbol("─");
            buf[(x, y)].set_style(Style::default().fg(p.accent));
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }

    if app.mouse_capture && list_bottom > area.y {
        let new_rect = app.sidebar_new_button_rect();
        frame.render_widget(
            Paragraph::new(Span::styled(" new", Style::default().fg(p.overlay0))),
            new_rect,
        );

        let menu_rect = app.global_launcher_rect();
        let menu_line = if app.global_menu_attention_badge_visible() {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled("menu", Style::default().fg(p.overlay0)),
            ])
        } else {
            Line::from(vec![Span::styled("menu", Style::default().fg(p.overlay0))])
        };
        frame.render_widget(
            Paragraph::new(menu_line).alignment(Alignment::Right),
            menu_rect,
        );
    }
}

fn render_agent_detail(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let p = &app.palette;

    if area.height < 3 {
        return;
    }

    let sep_line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep_line, Style::default().fg(p.surface_dim))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " agents",
            Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
        )])),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    let toggle_rect = agent_panel_toggle_rect(area, app.agent_panel_sort);
    if toggle_rect != Rect::default() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                agent_panel_sort_label(app.agent_panel_sort),
                Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Right),
            toggle_rect,
        );
    }

    let details = agent_panel_entries_from(app, terminal_runtimes);
    let metrics = agent_panel_scroll_metrics(app, area);
    let scrollbar_rect = agent_panel_scrollbar_rect(app, area);
    let body = agent_panel_body_rect(area, should_show_scrollbar(metrics));
    if body == Rect::default() {
        return;
    }

    let mut row_y = body.y;
    let body_bottom = body.y + body.height;
    for detail in details.iter().skip(app.agent_panel_scroll) {
        let label_color = state_label_color(detail.state, detail.seen, p);
        let rows = resolved_agent_rows(app, detail);
        let height = (rows.len().max(1) as u16).min(body.height);
        if row_y.saturating_add(height) > body_bottom {
            break;
        }

        let is_active = detail.target.is_active(app);
        let row_style = if is_active {
            Style::default().bg(p.surface_dim)
        } else {
            Style::default()
        };
        let title_style = if is_active {
            Style::default().fg(p.text).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0).add_modifier(Modifier::BOLD)
        };
        let status_style = if is_active {
            Style::default().fg(label_color)
        } else {
            Style::default().fg(label_color).add_modifier(Modifier::DIM)
        };
        let workspace_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
        let secondary_style = Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
        let agent_style = Style::default().fg(p.mauve).add_modifier(Modifier::BOLD);
        let state_icon = agent_icon(detail.state, detail.seen, app.spinner_tick, p);

        for (row_index, resolved) in rows.iter().take(height as usize).enumerate() {
            let y = row_y + row_index as u16;
            let row_rect = Rect::new(body.x, y, body.width, 1);
            frame.render_widget(Paragraph::new("").style(row_style), row_rect);

            let prefix = if row_index == 0 { " " } else { "   " };
            let prefix_width = display_width(prefix).min(body.width as usize);
            if let Some((leading, workspace)) = trailing_workspace(resolved) {
                let content_width = (body.width as usize).saturating_sub(prefix_width);
                let leading_has_title = leading.iter().any(|token| {
                    matches!(
                        token,
                        ResolvedToken::Tab(_)
                            | ResolvedToken::Pane(_)
                            | ResolvedToken::Agent(_)
                            | ResolvedToken::TerminalTitle(_)
                            | ResolvedToken::Custom(_)
                    )
                });
                let workspace_limit = if leading_has_title && content_width > 1 {
                    (content_width / 2).max(1)
                } else {
                    content_width
                };
                let workspace_width = display_width(workspace).min(workspace_limit);
                let gap = usize::from(workspace_width < content_width);
                let leading_width = content_width.saturating_sub(workspace_width + gap);
                let mut spans = vec![Span::raw(prefix)];
                spans.extend(resolved_token_spans(
                    leading,
                    state_icon,
                    status_style,
                    workspace_style,
                    title_style,
                    agent_style,
                    secondary_style,
                    p,
                    leading_width,
                ));
                frame.render_widget(
                    Paragraph::new(Line::from(spans)).style(row_style),
                    Rect::new(
                        body.x,
                        y,
                        u16::try_from(prefix_width + leading_width).unwrap_or(body.width),
                        1,
                    ),
                );
                if workspace_width > 0 {
                    let workspace_width = u16::try_from(workspace_width).unwrap_or(body.width);
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            truncate_end(workspace, workspace_width as usize),
                            workspace_style,
                        ))
                        .style(row_style)
                        .alignment(Alignment::Right),
                        Rect::new(
                            body.x + body.width.saturating_sub(workspace_width),
                            y,
                            workspace_width,
                            1,
                        ),
                    );
                }
            } else {
                let mut spans = vec![Span::raw(prefix)];
                spans.extend(resolved_token_spans(
                    resolved,
                    state_icon,
                    status_style,
                    workspace_style,
                    title_style,
                    agent_style,
                    secondary_style,
                    p,
                    body.width.saturating_sub(prefix_width as u16) as usize,
                ));
                frame.render_widget(Paragraph::new(Line::from(spans)).style(row_style), row_rect);
            }
        }
        row_y = row_y.saturating_add(height);
        if row_y < body_bottom {
            row_y += 1;
        }
    }

    if let Some(track) = scrollbar_rect {
        render_scrollbar(frame, metrics, track, p.surface_dim, p.overlay0, "▕");
    }
}

pub(crate) fn collapsed_sidebar_toggle_rect(area: Rect) -> Rect {
    let bottom_y = area.y + area.height.saturating_sub(1);
    let content_w = area.width.saturating_sub(1);
    if content_w == 0 || area.height == 0 {
        return Rect::default();
    }
    let x = area.x + content_w / 2;
    Rect::new(x, bottom_y, 1, 1)
}

pub(crate) fn expanded_sidebar_toggle_rect(area: Rect) -> Rect {
    if area.width <= 1 || area.height == 0 {
        return Rect::default();
    }
    Rect::new(
        area.x + area.width.saturating_sub(2),
        area.y + area.height.saturating_sub(1),
        1,
        1,
    )
}

fn render_sidebar_toggle(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    collapsed: bool,
    p: &Palette,
) {
    let toggle_area = if collapsed {
        collapsed_sidebar_toggle_rect(area)
    } else {
        expanded_sidebar_toggle_rect(area)
    };
    if toggle_area == Rect::default() {
        return;
    }
    let icon = if collapsed { "»" } else { "«" };
    let icon_style = if collapsed && app.global_menu_attention_badge_visible() {
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.overlay0)
    };
    frame.render_widget(Paragraph::new(Span::styled(icon, icon_style)), toggle_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{detect::Agent, workspace::Workspace};
    use ratatui::{backend::TestBackend, Terminal};

    fn remote_endpoint(
        member_id: &str,
        workspace_id: &str,
        label: &str,
    ) -> crate::federation::EndpointState {
        crate::federation::EndpointState {
            endpoint: crate::config::FederationEndpointConfig {
                id: member_id.into(),
                target: member_id.into(),
                label: None,
                session: "default".into(),
                enabled: true,
            },
            status: crate::federation::EndpointConnectionStatus::Connected,
            snapshot: Some(crate::api::schema::SessionSnapshot {
                identity: crate::api::schema::RuntimeIdentity {
                    server_id: format!("server-{member_id}"),
                    session_id: format!("session-{member_id}"),
                    session_name: "default".into(),
                    member_id: member_id.into(),
                    member_target: member_id.into(),
                    member_label: None,
                },
                version: "test".into(),
                protocol: crate::protocol::PROTOCOL_VERSION,
                event_cursor: 1,
                focused_workspace_id: Some(workspace_id.into()),
                focused_tab_id: None,
                focused_pane_id: None,
                workspaces: vec![crate::api::schema::WorkspaceInfo {
                    workspace_id: workspace_id.into(),
                    number: 1,
                    label: label.into(),
                    focused: true,
                    pane_count: 1,
                    tab_count: 1,
                    active_tab_id: format!("{workspace_id}:t1"),
                    agent_status: crate::api::schema::AgentStatus::Idle,
                    terminal_launcher_argv: None,
                    tokens: Default::default(),
                    branch: None,
                    git_ahead_behind: None,
                    worktree: None,
                }],
                tabs: Vec::new(),
                panes: Vec::new(),
                layouts: Vec::new(),
                agents: Vec::new(),
            }),
            cursor: Some(1),
            error: None,
        }
    }

    fn remote_endpoint_with_agent(
        member_id: &str,
        workspace_id: &str,
        label: &str,
        agent_name: &str,
    ) -> crate::federation::EndpointState {
        let mut endpoint = remote_endpoint(member_id, workspace_id, label);
        let snapshot = endpoint.snapshot.as_mut().unwrap();
        let tab_id = format!("{workspace_id}:t1");
        let pane_id = format!("{workspace_id}:p1");
        snapshot.tabs.push(crate::api::schema::TabInfo {
            tab_id: tab_id.clone(),
            workspace_id: workspace_id.into(),
            number: 1,
            label: "main".into(),
            focused: true,
            pane_count: 1,
            agent_status: crate::api::schema::AgentStatus::Working,
        });
        snapshot.focused_tab_id = Some(tab_id.clone());
        snapshot.focused_pane_id = Some(pane_id.clone());
        snapshot.agents.push(crate::api::schema::AgentInfo {
            terminal_id: format!("terminal-{member_id}"),
            name: Some(agent_name.into()),
            agent: Some("codex".into()),
            title: Some(agent_name.into()),
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: Some("codex".into()),
            agent_status: crate::api::schema::AgentStatus::Working,
            screen_detection_skipped: false,
            state_labels: Default::default(),
            tokens: Default::default(),
            agent_session: None,
            workspace_id: workspace_id.into(),
            tab_id,
            pane_id,
            focused: true,
            cwd: None,
            foreground_cwd: None,
            revision: 1,
        });
        endpoint
    }

    fn row_text(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
        (0..width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn agent_panel_is_a_location_aware_federation_wide_index() {
        let mut app = crate::app::state::AppState::test_new();
        app.federation_member_id = "b-stl".into();
        app.federation.insert(
            "a-x1".into(),
            remote_endpoint_with_agent("a-x1", "w1", "herdr@a-x1", "home-agent"),
        );
        app.federation.insert(
            "c-tana".into(),
            remote_endpoint_with_agent("c-tana", "w2", "geodeck@c-tana", "tana-agent"),
        );

        let entries = agent_panel_entries(&app);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].primary_label, "herdr");
        assert_eq!(entries[0].location, "a-x1");
        assert_eq!(entries[1].primary_label, "geodeck");
        assert_eq!(entries[1].location, "c-tana");
        assert!(matches!(
            &entries[1].target,
            AgentPanelTarget::Remote {
                endpoint_id,
                pane_id,
            } if endpoint_id == "c-tana" && pane_id == "w2:p1"
        ));
    }

    fn member_workspace_order(app: &AppState) -> Vec<(String, String)> {
        workspace_list_entries(app)
            .into_iter()
            .map(|entry| match entry {
                WorkspaceListEntry::Workspace { ws_idx, .. } => (
                    app.federation_member_id.clone(),
                    app.workspaces[ws_idx].base_display_name(),
                ),
                WorkspaceListEntry::RemoteWorkspace {
                    endpoint_id,
                    workspace_id,
                    ..
                } => (endpoint_id, workspace_id),
            })
            .collect()
    }

    #[test]
    fn federated_workspace_order_does_not_move_the_active_member_to_the_front() {
        let mut on_a = AppState::test_new();
        on_a.federation_member_id = "a".into();
        on_a.workspaces = vec![Workspace::test_new("a-workspace")];
        on_a.federation_client_overlay.insert(
            "b".into(),
            remote_endpoint("b", "b-workspace", "b workspace"),
        );
        on_a.federation_client_overlay.insert(
            "c".into(),
            remote_endpoint("c", "c-workspace", "c workspace"),
        );

        let mut on_b = AppState::test_new();
        on_b.federation_member_id = "b".into();
        on_b.workspaces = vec![Workspace::test_new("b-workspace")];
        on_b.federation_client_overlay.insert(
            "a".into(),
            remote_endpoint("a", "a-workspace", "a workspace"),
        );
        on_b.federation_client_overlay.insert(
            "c".into(),
            remote_endpoint("c", "c-workspace", "c workspace"),
        );

        assert_eq!(
            member_workspace_order(&on_a),
            vec![
                ("a".into(), "a-workspace".into()),
                ("b".into(), "b-workspace".into()),
                ("c".into(), "c-workspace".into()),
            ]
        );
        assert_eq!(member_workspace_order(&on_b), member_workspace_order(&on_a));
    }

    #[test]
    fn workspace_rows_render_semantic_name_with_muted_location_on_the_right() {
        let mut app = AppState::test_new();
        app.federation_member_id = "x1".into();
        app.workspaces = vec![Workspace::test_new("home")];
        app.active = Some(0);
        app.selected = 0;
        app.federation_client_overlay.insert(
            "stl-agents-1".into(),
            remote_endpoint("stl-agents-1", "w9", "checking@stl-agents-1"),
        );
        let remote_workspace = app
            .federation_client_overlay
            .get_mut("stl-agents-1")
            .and_then(|endpoint| endpoint.snapshot.as_mut())
            .and_then(|snapshot| snapshot.workspaces.first_mut())
            .expect("remote workspace");
        remote_workspace.branch = Some("handoff/abc123".into());
        remote_workspace.git_ahead_behind = Some((2, 1));

        let area = Rect::new(0, 0, 32, 18);
        app.view.workspace_card_areas = compute_workspace_card_areas(&app, area);
        let mut terminal = Terminal::new(TestBackend::new(32, 18)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let cards = compute_workspace_card_areas(&app, area);
        let remote = cards
            .iter()
            .find(|card| card.remote.is_some())
            .expect("remote workspace card");
        let rendered = row_text(
            terminal.backend().buffer(),
            remote.rect.y,
            remote.rect.width,
        );

        assert!(rendered.contains("checking"), "rendered row: {rendered:?}");
        assert!(
            !rendered.contains("checking@"),
            "rendered row: {rendered:?}"
        );
        assert!(
            rendered.ends_with("stl-agents-1"),
            "rendered row: {rendered:?}"
        );
        assert_eq!(remote.rect.height, 2);
        let subtitle = row_text(
            terminal.backend().buffer(),
            remote.rect.y + 1,
            remote.rect.width,
        );
        assert!(
            subtitle.contains("handoff/abc123"),
            "rendered subtitle: {subtitle:?}"
        );
        let location_x = remote.rect.x + remote.rect.width - "stl-agents-1".len() as u16;
        assert_eq!(
            terminal.backend().buffer()[(location_x, remote.rect.y)].fg,
            app.palette.overlay0
        );
    }

    #[test]
    fn remote_linked_worktree_is_nested_under_the_project_main_workspace() {
        let mut app = AppState::test_new();
        app.federation_member_id = "x1".into();
        app.workspaces = vec![workspace_with_worktree_space(
            "stl-agents",
            Some("repo-key"),
            "/repo/stl-agents",
        )];
        app.workspaces[0]
            .worktree_space
            .as_mut()
            .unwrap()
            .is_linked_worktree = false;
        let mut remote = remote_endpoint("stl-agents-1", "rw1", "handoff-09f05a1afd99");
        remote.snapshot.as_mut().unwrap().workspaces[0].worktree =
            Some(crate::api::schema::WorkspaceWorktreeInfo {
                repo_key: "repo-key".into(),
                repo_name: "stl-agents".into(),
                repo_root: "/repo/stl-agents".into(),
                checkout_path: "/worktrees/09f05a1afd99".into(),
                is_linked_worktree: true,
            });
        app.federation_client_overlay
            .insert("stl-agents-1".into(), remote);

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::RemoteWorkspace {
                    endpoint_id: "stl-agents-1".into(),
                    workspace_id: "rw1".into(),
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn agent_tokens_use_their_dedicated_strong_style() {
        let app = crate::app::state::AppState::test_new();
        let agent_style = Style::default()
            .fg(ratatui::style::Color::Magenta)
            .add_modifier(Modifier::BOLD);
        let spans = resolved_token_spans(
            &[ResolvedToken::Agent("planner".into())],
            ("", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            agent_style,
            Style::default(),
            &app.palette,
            20,
        );

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style, agent_style);
    }

    #[test]
    fn default_agent_rows_remove_redundant_state_text() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal_state = app.terminals.get_mut(&terminal_id).unwrap();
        terminal_state.detected_agent = Some(Agent::Pi);
        terminal_state.state = AgentState::Working;

        let area = Rect::new(0, 0, 26, 20);
        let mut terminal = Terminal::new(TestBackend::new(26, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (_, agent_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
        let body = agent_panel_body_rect(agent_area, false);

        let first = row_text(buffer, body.y, 25);
        let second = row_text(buffer, body.y + 1, 25);
        assert!(first.contains("one"));
        assert_eq!(second, "   pi");
        assert!(!first.contains("working"));
        assert!(!second.contains("working"));
    }

    #[test]
    fn narrow_agent_rows_preserve_title_and_right_aligned_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("very-long-workspace-name");
        let tab_idx = workspace.test_add_tab(Some("logs"));
        let pane_id = workspace.tabs[tab_idx].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[tab_idx].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);

        let area = Rect::new(0, 0, 18, 20);
        let mut terminal = Terminal::new(TestBackend::new(18, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (_, agent_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
        let body = agent_panel_body_rect(agent_area, false);
        let first = row_text(buffer, body.y, 17);

        assert!(first.contains("logs"), "rendered row: {first:?}");
        assert!(first.ends_with('…'), "rendered row: {first:?}");
    }

    #[test]
    fn agent_title_is_strong_and_clipped_before_right_aligned_workspace() {
        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("repo");
        workspace.tabs[0].set_custom_name("very-long-agent-title".into());
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);

        let area = Rect::new(0, 0, 18, 20);
        let mut terminal = Terminal::new(TestBackend::new(18, 20)).unwrap();
        terminal
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (_, agent_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
        let body = agent_panel_body_rect(agent_area, false);
        let first = row_text(buffer, body.y, body.width);

        assert!(first.ends_with("repo"), "rendered row: {first:?}");
        assert!(first.contains("very"), "rendered row: {first:?}");
        assert!(!first.contains("very-long-agent-title"));

        let workspace_x = body.x + body.width - 4;
        assert_eq!(buffer[(workspace_x, body.y)].symbol(), "r");
        assert_eq!(buffer[(workspace_x, body.y)].fg, app.palette.overlay0);
        let title_x = (body.x..workspace_x)
            .find(|x| buffer[(*x, body.y)].symbol() == "v")
            .expect("title should remain visible before workspace");
        assert_eq!(buffer[(title_x, body.y)].fg, app.palette.text);
    }

    #[test]
    fn stripped_terminal_title_renders_with_unicode_width_truncation() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Claude);
        terminal.set_terminal_title(Some("⠋ 修复🙂标题很长".into()));
        app.sidebar_agents.rows = vec![vec![
            crate::config::AgentSidebarToken::TerminalTitleStripped,
        ]];

        let area = Rect::new(0, 0, 10, 12);
        let mut renderer = Terminal::new(TestBackend::new(10, 12)).unwrap();
        renderer
            .draw(|frame| render_sidebar(&app, &TerminalRuntimeRegistry::new(), frame, area))
            .unwrap();
        let (_, agent_area) = expanded_sidebar_sections(area, app.sidebar_section_split);
        let body = agent_panel_body_rect(agent_area, false);
        let rendered = row_text(renderer.backend().buffer(), body.y, 9);

        assert!(!rendered.contains('⠋'));
        assert!(rendered.contains('修') && rendered.contains('复'));

        let spans = resolved_token_spans(
            &[ResolvedToken::TerminalTitle("修复🙂标题很长".into())],
            ("", Style::default()),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default(),
            Style::default(),
            &app.palette,
            8,
        );
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(display_width(&text) <= 8, "resolved title: {text:?}");
    }

    #[test]
    fn variable_agent_heights_pack_the_bottom_and_reveal_targets() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.ensure_test_terminals();
        for workspace in &app.workspaces {
            let pane_id = workspace.tabs[0].root_pane;
            let terminal_id = workspace.tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Pi);
        }
        let first_pane = app.workspaces[0].tabs[0].root_pane;
        let first_terminal = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal)
            .unwrap()
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([
                    ("a".into(), Some("a".into())),
                    ("b".into(), Some("b".into())),
                ]),
                None,
                std::time::Instant::now(),
            );
        app.sidebar_agents.rows = vec![
            vec![crate::config::AgentSidebarToken::Agent],
            vec![crate::config::AgentSidebarToken::Custom("a".into())],
            vec![crate::config::AgentSidebarToken::Custom("b".into())],
        ];
        let area = Rect::new(0, 0, 20, 6);

        let metrics = agent_panel_scroll_metrics(&app, area);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(agent_panel_scroll_for_target(&app, area, 0, 2), 1);
    }

    #[test]
    fn oversized_space_layout_is_clipped_to_the_section_body() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]; 6];
        let area = Rect::new(0, 0, 20, 10);
        let workspace_area = workspace_list_rect(area, app.sidebar_section_split);
        let body = workspace_list_body_rect(workspace_area, false);

        let metrics = workspace_list_scroll_metrics(&app, workspace_area);
        let (cards, _) = compute_workspace_list_areas(&app, area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 0);
        assert_eq!(cards[0].rect.height, body.height);
    }

    #[test]
    fn oversized_agent_override_is_clipped_to_the_panel_body() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("one");
        let pane_id = workspace.tabs[0].root_pane;
        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        app.sidebar_agents.rows_by_agent.insert(
            "claude".into(),
            vec![vec![crate::config::AgentSidebarToken::Agent]; 6],
        );
        let panel = Rect::new(0, 0, 20, 5);

        let metrics = agent_panel_scroll_metrics(&app, panel);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 0);
        let entry = agent_panel_entries(&app).pop().unwrap();
        assert_eq!(
            agent_entry_height_in_body(&app, &entry, agent_panel_body_rect(panel, false).height),
            agent_panel_body_rect(panel, false).height
        );
    }

    #[test]
    fn render_sidebar_toggle_draws_expanded_collapse_icon() {
        let app = crate::app::state::AppState::test_new();
        let area = Rect::new(0, 0, 26, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(26, 20)).expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_toggle(&app, frame, area, false, &app.palette))
            .expect("sidebar toggle should render");

        let toggle = expanded_sidebar_toggle_rect(area);
        assert_eq!(
            terminal.backend().buffer()[(toggle.x, toggle.y)].symbol(),
            "«"
        );
    }

    #[test]
    fn expanded_sidebar_toggle_sits_inside_sidebar_content() {
        let area = Rect::new(0, 0, 26, 20);
        let toggle = expanded_sidebar_toggle_rect(area);

        assert_eq!(toggle.x, area.x + area.width - 2);
        assert_eq!(toggle.y, area.y + area.height - 1);
    }

    #[test]
    fn agent_panel_tab_label_visibility_tracks_tab_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let single_auto = Workspace::test_new("auto");
        let mut single_custom = Workspace::test_new("custom");
        single_custom.tabs[0].set_custom_name("focus".into());
        let mut multi = Workspace::test_new("multi");
        multi.test_add_tab(Some("logs"));

        app.workspaces = vec![single_auto, single_custom, multi];
        app.ensure_test_terminals();
        for (ws_idx, tab_idx, agent) in [
            (0, 0, Agent::Pi),
            (1, 0, Agent::Claude),
            (2, 0, Agent::Codex),
            (2, 1, Agent::Pi),
        ] {
            let pane_id = app.workspaces[ws_idx].tabs[tab_idx].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(agent);
        }

        let entries = agent_panel_entries(&app);
        let labels: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.primary_label.as_str(),
                    entry.primary_tab_label.as_deref(),
                )
            })
            .collect();

        assert_eq!(
            labels,
            [
                ("auto", None),
                ("custom", Some("focus")),
                ("multi", Some("1")),
                ("multi", Some("logs")),
            ]
        );
    }

    #[test]
    fn priority_agent_panel_sort_uses_attention_then_space_order() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
            Workspace::test_new("four"),
        ];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.selected = 0;
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, state| {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, AgentState::Working);
        set_state(&mut app, 1, AgentState::Idle);
        set_state(&mut app, 2, AgentState::Working);
        set_state(&mut app, 3, AgentState::Blocked);

        let done_pane = app.workspaces[1].tabs[0].root_pane;
        app.workspaces[1].tabs[0]
            .panes
            .get_mut(&done_pane)
            .unwrap()
            .seen = false;

        let labels: Vec<String> = agent_panel_entries(&app)
            .into_iter()
            .map(|entry| entry.primary_label)
            .collect();

        assert_eq!(labels, ["four", "two", "one", "three"]);
    }

    #[test]
    fn collapsed_sidebar_numbers_grouped_agents_by_list_position() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.ensure_test_terminals();

        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        let area = Rect::new(0, 0, 4, 12);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(detail_area.x, detail_area.y)].symbol(), "1");
        assert_eq!(buffer[(detail_area.x, detail_area.y + 1)].symbol(), "2");
    }

    #[test]
    fn collapsed_sidebar_keeps_status_visible_for_two_digit_positions() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = (1..=10)
            .map(|idx| Workspace::test_new(&format!("workspace-{idx}")))
            .collect();
        app.ensure_test_terminals();

        for ws_idx in 0..app.workspaces.len() {
            let pane = app.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            app.terminals.get_mut(&terminal_id).unwrap().detected_agent = Some(Agent::Claude);
        }

        let area = Rect::new(0, 0, 4, 25);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let tenth_row = detail_area.y + 9;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(detail_area.x, tenth_row)].symbol(), "1");
        assert_eq!(buffer[(detail_area.x + 1, tenth_row)].symbol(), "0");
        assert_eq!(buffer[(detail_area.x + 2, tenth_row)].symbol(), "○");
    }

    #[test]
    fn collapsed_sidebar_numbers_priority_agents_by_list_position() {
        let first = Workspace::test_new("one");
        let first_pane = first.tabs[0].root_pane;
        let mut second = Workspace::test_new("two");
        let second_pane = second.tabs[0].root_pane;
        let urgent_pane = second.test_split(ratatui::layout::Direction::Horizontal);

        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![first, second];
        app.ensure_test_terminals();
        app.agent_panel_sort = crate::app::state::AgentPanelSort::Priority;

        let set_state = |app: &mut crate::app::state::AppState, ws_idx: usize, pane_id, state| {
            let terminal_id = app.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = app.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = state;
        };
        set_state(&mut app, 0, first_pane, AgentState::Working);
        set_state(&mut app, 1, second_pane, AgentState::Working);
        set_state(&mut app, 1, urgent_pane, AgentState::Blocked);

        assert_eq!(app.workspaces[1].public_pane_number(urgent_pane), Some(2));
        assert_eq!(
            agent_panel_entries(&app)[0].target,
            AgentPanelTarget::Local {
                ws_idx: 1,
                tab_idx: 0,
                pane_id: urgent_pane,
            }
        );

        let area = Rect::new(0, 0, 4, 16);
        let (_, _, detail_area) = collapsed_sidebar_sections(area);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("test terminal should initialize");

        terminal
            .draw(|frame| render_sidebar_collapsed(&app, frame, area))
            .expect("collapsed sidebar should render");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(detail_area.x, detail_area.y)].symbol(), "1");
        assert_eq!(buffer[(detail_area.x, detail_area.y + 1)].symbol(), "2");
        assert_eq!(buffer[(detail_area.x, detail_area.y + 2)].symbol(), "3");
        assert_eq!(buffer[(detail_area.x + 2, detail_area.y)].symbol(), "◉");
        assert_eq!(
            buffer[(detail_area.x + 2, detail_area.y)].style().fg,
            Some(app.palette.red)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn all_workspaces_agent_panel_entries_use_live_root_runtime_cwd_for_workspace_label() {
        let unique = format!(
            "herdr-agent-panel-runtime-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let stale_cwd = root.join("issue-264-nix-support");
        let live_cwd = root.join("herdr");
        std::fs::create_dir_all(stale_cwd.join(".git")).unwrap();
        std::fs::create_dir_all(live_cwd.join(".git")).unwrap();

        let mut app = crate::app::state::AppState::test_new();
        let mut workspace = Workspace::test_new("stale-name");
        workspace.custom_name = None;
        workspace.identity_cwd = stale_cwd.clone();
        let pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).unwrap();
        terminal.cwd = stale_cwd;
        terminal.detected_agent = Some(Agent::Pi);
        app.active = Some(0);
        app.selected = 0;

        let (events, _) = tokio::sync::mpsc::channel(4);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane,
            24,
            80,
            live_cwd.clone(),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd() != Some(live_cwd.clone()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mut runtime_registry = TerminalRuntimeRegistry::new();
        runtime_registry.insert(terminal_id, runtime);
        let entries = agent_panel_entries_from(&app, &runtime_registry);
        let primary_label = entries[0].primary_label.clone();

        for (_, runtime) in runtime_registry.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(root);

        assert_eq!(primary_label, "herdr");
    }

    #[test]
    fn all_workspaces_agent_panel_entries_prefer_agent_names_for_agent_identity() {
        let mut app = crate::app::state::AppState::test_new();
        let workspace = Workspace::test_new("bridge");
        let first_pane = workspace.tabs[0].root_pane;

        app.workspaces = vec![workspace];
        app.ensure_test_terminals();
        let first_terminal_id = app.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Pi);
        app.terminals
            .get_mut(&first_terminal_id)
            .unwrap()
            .set_agent_name("planner".into());
        app.active = Some(0);
        app.selected = 0;

        let entries = agent_panel_entries(&app);
        assert_eq!(entries[0].primary_label, "bridge");
        assert_eq!(entries[0].agent_label.as_deref(), Some("planner"));
    }

    #[test]
    fn expanded_sidebar_sections_handle_tiny_heights() {
        let (ws_area, detail_area) = expanded_sidebar_sections(Rect::new(0, 0, 20, 5), 0.9);

        assert_eq!(ws_area, Rect::new(0, 0, 19, 3));
        assert_eq!(detail_area, Rect::new(0, 3, 19, 2));
    }

    #[test]
    fn sidebar_section_divider_is_hidden_for_tiny_heights() {
        let divider = sidebar_section_divider_rect(Rect::new(0, 0, 20, 5), 0.5);

        assert_eq!(divider, Rect::default());
    }

    #[test]
    fn grouped_child_label_keeps_custom_workspace_name() {
        assert_eq!(
            grouped_child_display_label("renamed issue", Some("worktree/issue-137"), true),
            "renamed issue"
        );
    }

    #[test]
    fn grouped_child_label_uses_short_branch_for_auto_named_workspace() {
        assert_eq!(
            grouped_child_display_label("herdr-issue", Some("worktree/issue-137"), false),
            "issue-137"
        );
    }

    #[test]
    fn workspace_list_truncates_cjk_branch_without_panic() {
        let mut app = crate::app::state::AppState::test_new();
        let mut ws = Workspace::test_new("repo");
        ws.cached_git_branch = Some("feature/中文-分支-644".into());
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app.view.workspace_card_areas = vec![crate::app::state::WorkspaceCardArea {
            ws_idx: 0,
            remote: None,
            rect: Rect::new(0, 1, 15, 2),
            indented: false,
            group_key: None,
            group_parent: false,
        }];

        let mut terminal = Terminal::new(TestBackend::new(15, 6)).expect("test terminal");
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        terminal
            .draw(|frame| {
                render_workspace_list(&app, &runtimes, frame, Rect::new(0, 0, 15, 6), false)
            })
            .expect("workspace list should render");
    }

    fn workspace_with_worktree_space(
        name: &str,
        key: Option<&str>,
        checkout_key: &str,
    ) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        if let Some(key) = key {
            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: key.into(),
                label: "herdr".into(),
                repo_root: std::path::PathBuf::from("/repo/herdr"),
                checkout_path: std::path::PathBuf::from(checkout_key),
                is_linked_worktree: name != "main",
            });
        }
        ws
    }

    fn workspace_with_git_space(name: &str, key: &str) -> crate::workspace::Workspace {
        let mut ws = crate::workspace::Workspace::test_new(name);
        ws.cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: key.into(),
            checkout_key: format!("/repo/{name}"),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from(format!("/repo/{name}")),
            is_linked_worktree: false,
        });
        ws
    }

    #[test]
    fn parent_workspace_row_stays_clickable_when_grouped() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 20));

        assert!(headers.is_empty());
        assert_eq!(cards[0].ws_idx, 0);
        assert!(!cards[0].indented);
        assert_eq!(cards[1].ws_idx, 1);
        assert!(cards[1].indented);
        assert_eq!(cards[1].rect.y, cards[0].rect.y + cards[0].rect.height + 1);
    }

    #[test]
    fn linked_only_worktree_members_do_not_form_parentless_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            workspace_with_worktree_space("review", Some("repo-key"), "/repo/herdr-review"),
        ];

        let entries = workspace_list_entries(&app);

        assert_eq!(
            entries,
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false
                },
            ]
        );
    }

    #[test]
    fn compact_space_group_scroll_clamps_when_all_entries_fit() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("one", Some("repo-key"), "/repo/herdr-one"),
            workspace_with_worktree_space("two", Some("repo-key"), "/repo/herdr-two"),
        ];
        let area = Rect::new(0, 0, 30, 20);
        app.workspace_scroll = normalized_workspace_scroll(&app, area, 2);

        let (cards, headers) = compute_workspace_list_areas(&app, area);

        assert!(headers.is_empty());
        assert_eq!(app.workspace_scroll, 0);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[2].ws_idx, 2);
    }

    #[test]
    fn workspace_scroll_metrics_count_display_entries_not_raw_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        for workspace in &mut app.workspaces {
            workspace.cached_git_branch = Some("main".into());
        }
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;

        let ws_area = Rect::new(0, 0, 30, 6);
        let metrics = workspace_list_scroll_metrics(&app, ws_area);

        assert_eq!(metrics.viewport_rows, 1);
        assert_eq!(metrics.max_offset_from_bottom, 1);
        assert_eq!(metrics.offset_from_bottom, 1);
    }

    #[test]
    fn workspace_scroll_offset_applies_to_group_children() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
            Workspace::test_new("notes"),
        ];
        app.collapsed_space_keys.insert("repo-key".into());
        app.active = None;
        app.mode = Mode::Terminal;
        app.workspace_scroll = 1;

        let (cards, headers) = compute_workspace_list_areas(&app, Rect::new(0, 0, 30, 12));

        assert!(headers.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].ws_idx, 2);
    }

    #[test]
    fn workspace_list_entries_group_multiple_workspaces_in_same_git_space() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_group_non_contiguous_explicit_members() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("normal", "other-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_group_normal_git_workspaces() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_git_space("two", "repo-key"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_do_not_auto_attach_normal_git_workspace_to_group() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_git_space("scratch", "repo-key"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 2,
                    indented: true,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn workspace_list_entries_leave_single_git_and_non_git_workspaces_flat() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_git_space("one", "repo-key"),
            workspace_with_worktree_space("notes", None, "/notes"),
        ];

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: false,
                },
            ]
        );
    }

    #[test]
    fn collapsed_group_hides_inactive_children_but_keeps_active_visible() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.active = Some(1);
        app.mode = Mode::Terminal;
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );

        app.active = None;
        app.mode = Mode::Terminal;
        assert_eq!(
            workspace_list_entries(&app),
            vec![WorkspaceListEntry::Workspace {
                ws_idx: 0,
                indented: false,
            }]
        );
    }

    #[test]
    fn collapsed_group_keeps_selected_child_visible_in_navigate_mode() {
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace_with_worktree_space("main", Some("repo-key"), "/repo/herdr"),
            workspace_with_worktree_space("issue", Some("repo-key"), "/repo/herdr-issue"),
        ];
        app.mode = Mode::Navigate;
        app.selected = 1;
        app.active = Some(1);
        app.collapsed_space_keys.insert("repo-key".into());

        assert_eq!(
            workspace_list_entries(&app),
            vec![
                WorkspaceListEntry::Workspace {
                    ws_idx: 0,
                    indented: false,
                },
                WorkspaceListEntry::Workspace {
                    ws_idx: 1,
                    indented: true,
                },
            ]
        );
    }
}
