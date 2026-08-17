use bytes::Bytes;

use super::App;

impl App {
    pub(crate) fn reopen_archived_agent(
        &mut self,
        archive_id: &str,
    ) -> Result<(usize, crate::layout::PaneId), String> {
        let archive = self
            .state
            .archived_agent_sessions
            .get(archive_id)
            .cloned()
            .ok_or_else(|| format!("archived agent {archive_id} not found"))?;

        if let Some((ws_idx, pane_id)) = self.live_pane_for_archive(&archive) {
            self.state.focus_pane_in_workspace(ws_idx, pane_id);
            self.state.mode = super::Mode::Terminal;
            return Ok((ws_idx, pane_id));
        }

        let plan = archive
            .resume_plan()
            .ok_or_else(|| format!("{} session is not resumable", archive.agent))?;
        let cwd = reopen_cwd(&archive);
        let existing_ws_idx = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == archive.workspace_id)
            .or_else(|| {
                let key = archive
                    .project_identity
                    .as_ref()
                    .map(|project| &project.key)?;
                self.state.workspaces.iter().position(|workspace| {
                    workspace
                        .project_identity()
                        .is_some_and(|project| &project.key == key)
                })
            });

        let (ws_idx, tab_idx, created_workspace) = if let Some(ws_idx) = existing_ws_idx {
            self.state.switch_workspace(ws_idx);
            let tab_idx = self
                .create_tab_with_options(cwd.clone(), true)
                .map_err(|err| format!("could not create a tab for {}: {err}", archive.agent))?;
            (ws_idx, tab_idx, false)
        } else {
            let ws_idx = self
                .create_workspace_with_options(cwd.clone(), true)
                .map_err(|err| {
                    format!(
                        "could not recreate a workspace for {}: {err}",
                        archive.agent
                    )
                })?;
            if let Some(workspace) = self.state.workspaces.get_mut(ws_idx) {
                workspace.custom_name.clone_from(&archive.workspace_name);
                workspace.project_identity = archive.project_identity.clone();
                workspace.worktree_space = archive.worktree_space.clone().filter(|space| {
                    space.checkout_path.exists()
                        && crate::workspace::git_space_metadata(&space.checkout_path)
                            .is_some_and(|current| current.project_key == space.key)
                });
            }
            (ws_idx, 0, true)
        };

        let pane_id = self.state.workspaces[ws_idx].tabs[tab_idx].root_pane;
        if let Some(name) = archive.tab_name.as_deref().or(archive.title.as_deref()) {
            self.state.workspaces[ws_idx].tabs[tab_idx].set_custom_name(name.to_string());
        }
        let terminal_id = self.state.workspaces[ws_idx].tabs[tab_idx]
            .panes
            .get(&pane_id)
            .map(|pane| pane.attached_terminal_id.clone())
            .ok_or_else(|| "new archive pane has no terminal".to_string())?;
        let mut command = super::agent_resume::shell_command_from_argv(&plan.argv)
            .ok_or_else(|| "agent resume command is empty".to_string())?;
        command.push('\r');
        let sent = self
            .terminal_runtimes
            .get(&terminal_id)
            .is_some_and(|runtime| runtime.try_send_bytes(Bytes::from(command)).is_ok());
        if !sent {
            self.rollback_archived_agent_placement(
                ws_idx,
                tab_idx,
                created_workspace,
                &terminal_id,
            );
            return Err(format!(
                "could not start the archived {} session",
                archive.agent
            ));
        }

        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            if let (Some(source), Some(session_ref)) =
                (archive.source.clone(), archive.session_ref.clone())
            {
                terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                    source,
                    agent: archive.agent.clone(),
                    session_ref: crate::agent_resume::AgentSessionRef {
                        kind: session_ref.kind,
                        value: session_ref.value,
                    },
                });
            }
            terminal.last_user_activity_at = archive.last_user_activity_at;
            terminal.last_agent_activity_at = archive.last_agent_activity_at;
            if let Some(label) = archive.label.clone() {
                terminal.set_manual_label(label);
            }
            if let Some(name) = archive.agent_name.clone() {
                terminal.set_agent_name(name);
            }
            if let Some(agent) = crate::detect::parse_agent_label(&archive.agent) {
                terminal.set_detected_state_with_screen_signals_at(
                    Some(agent),
                    crate::detect::AgentState::Idle,
                    false,
                    false,
                    false,
                    false,
                    std::time::Instant::now(),
                );
            }
        }

        let public_pane_id = self
            .public_pane_id(ws_idx, pane_id)
            .ok_or_else(|| "new archive pane has no public identity".to_string())?;
        if let Some(stored) = self.state.archived_agent_sessions.get_mut(archive_id) {
            stored.active_pane_id = Some(public_pane_id);
            stored.closed_at = None;
        }
        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        self.state.mode = super::Mode::Terminal;
        self.schedule_session_save();
        if created_workspace {
            self.emit_workspace_open_events(ws_idx);
        } else {
            self.emit_tab_created_events(ws_idx, tab_idx);
        }
        self.emit_archived_agent_events();
        Ok((ws_idx, pane_id))
    }

    fn live_pane_for_archive(
        &self,
        archive: &crate::agent_archive::ArchivedAgentSession,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let dedupe_key = archive.dedupe_key()?;
        self.state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, workspace)| {
                workspace.tabs.iter().find_map(|tab| {
                    tab.panes.iter().find_map(|(pane_id, pane)| {
                        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
                        let session = terminal.persisted_agent_session.as_ref()?;
                        (crate::agent_resume::dedupe_key(
                            &session.source,
                            &session.agent,
                            &session.session_ref,
                        ) == dedupe_key)
                            .then_some((ws_idx, *pane_id))
                    })
                })
            })
    }

    fn rollback_archived_agent_placement(
        &mut self,
        ws_idx: usize,
        tab_idx: usize,
        created_workspace: bool,
        terminal_id: &crate::terminal::TerminalId,
    ) {
        if created_workspace {
            if ws_idx < self.state.workspaces.len() {
                self.state.workspaces.remove(ws_idx);
            }
        } else if let Some(workspace) = self.state.workspaces.get_mut(ws_idx) {
            workspace.close_tab(tab_idx);
        }
        self.state.terminals.remove(terminal_id);
        if let Some(runtime) = self.terminal_runtimes.remove(terminal_id) {
            runtime.shutdown();
        }
    }
}

fn reopen_cwd(archive: &crate::agent_archive::ArchivedAgentSession) -> std::path::PathBuf {
    if archive.cwd.exists() {
        return archive.cwd.clone();
    }
    if let Some(root) = archive.project_root.as_ref().filter(|root| root.exists()) {
        return root.clone();
    }
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|home| home.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
}
