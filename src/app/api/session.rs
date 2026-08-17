use crate::api::schema::{ResponseResult, SessionSnapshot};
use crate::app::App;

use super::responses::encode_success;

impl App {
    pub(super) fn handle_session_snapshot(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::SessionSnapshot {
                snapshot: Box::new(self.session_snapshot()),
            },
        )
    }

    pub(crate) fn session_snapshot(&self) -> SessionSnapshot {
        let focused_workspace_id = self
            .state
            .active
            .map(|ws_idx| self.public_workspace_id(ws_idx));
        let focused_tab_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_tab_id(ws_idx, ws.active_tab)
        });
        let focused_pane_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_pane_id(ws_idx, ws.focused_pane_id()?)
        });

        let mut workspaces = Vec::new();
        let mut tabs = Vec::new();
        let mut layouts = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            workspaces.push(self.workspace_info(ws_idx));
            for tab_idx in 0..ws.tabs.len() {
                if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                    tabs.push(tab);
                }
                if let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) {
                    layouts.push(layout);
                }
            }
        }

        SessionSnapshot {
            identity: crate::runtime_identity::current().unwrap_or_else(|err| {
                tracing::error!(%err, "failed to read runtime identity for session snapshot");
                crate::api::schema::RuntimeIdentity {
                    server_id: "unavailable".into(),
                    session_id: "unavailable".into(),
                    session_name: crate::session::active_name()
                        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string()),
                    member_id: self.state.federation_member_id.clone(),
                    member_target: self.state.federation_member_target.clone(),
                    member_label: self.state.federation_member_label.clone(),
                }
            }),
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            event_cursor: self.event_hub.current_sequence(),
            focused_workspace_id,
            focused_tab_id,
            focused_pane_id,
            workspaces,
            tabs,
            panes: self.collect_panes_for_workspace(None).unwrap_or_default(),
            layouts,
            agents: self.collect_agent_infos(),
            archived_agents: self
                .state
                .archived_agent_sessions
                .values()
                .map(|archive| crate::api::schema::ArchivedAgentInfo {
                    archive_id: archive.archive_id.clone(),
                    member_id: self.state.federation_member_id.clone(),
                    agent: archive.agent.clone(),
                    title: archive.title.clone(),
                    label: archive.label.clone(),
                    cwd: archive.cwd.display().to_string(),
                    workspace_id: archive.workspace_id.clone(),
                    workspace_name: archive.workspace_name.clone(),
                    project_key: archive
                        .project_identity
                        .as_ref()
                        .map(|project| project.key.clone()),
                    project_name: archive
                        .project_identity
                        .as_ref()
                        .map(|project| project.label.clone()),
                    tab_name: archive.tab_name.clone(),
                    resumable: archive.resume_plan().is_some(),
                    active_pane_id: archive.active_pane_id.clone(),
                    last_user_activity_at: archive.last_user_activity_at,
                    last_agent_activity_at: archive.last_agent_activity_at,
                    closed_at: archive.closed_at,
                })
                .collect(),
        }
    }

    pub(crate) fn apply_federation_directory(
        &mut self,
        directory: Vec<crate::federation::EndpointState>,
    ) {
        let mut overlay = std::collections::BTreeMap::new();
        for mut state in directory {
            if state.endpoint.id == self.state.federation_member_id {
                continue;
            }
            if state.snapshot.is_none() {
                if let Some(previous) = self.state.federation_client_overlay.get(&state.endpoint.id)
                {
                    state.snapshot = previous.snapshot.clone();
                    state.cursor = state.cursor.or(previous.cursor);
                }
            }
            let authoritative_connected = self
                .state
                .federation
                .get(&state.endpoint.id)
                .is_some_and(|current| {
                    current.status == crate::federation::EndpointConnectionStatus::Connected
                });
            if !authoritative_connected {
                overlay.insert(state.endpoint.id.clone(), state);
            }
        }
        self.state.federation_client_overlay = overlay;
    }

    pub(crate) fn focus_federated_resource(
        &mut self,
        resource: &crate::federation::FederatedResourceRef,
    ) -> Result<(), String> {
        if resource.endpoint_id != self.state.federation_member_id {
            return Err(format!(
                "resource belongs to {}, but this member is {}",
                resource.endpoint_id, self.state.federation_member_id
            ));
        }
        let identity = crate::runtime_identity::current()
            .map_err(|err| format!("could not verify runtime identity: {err}"))?;
        if resource.server_id != identity.server_id || resource.session_id != identity.session_id {
            return Err("resource refers to a replaced federation runtime".to_string());
        }

        let target = match resource.kind {
            crate::federation::FederatedResourceKind::Workspace => self
                .state
                .workspaces
                .iter()
                .position(|workspace| workspace.id == resource.resource_id)
                .map(|ws_idx| crate::app::state::NavigatorTarget::Workspace { ws_idx }),
            crate::federation::FederatedResourceKind::Tab => self
                .state
                .workspaces
                .iter()
                .enumerate()
                .find_map(|(ws_idx, workspace)| {
                    (0..workspace.tabs.len()).find_map(|tab_idx| {
                        let number = workspace.public_tab_number(tab_idx)?;
                        (crate::workspace::public_tab_id_for_number(&workspace.id, number)
                            == resource.resource_id)
                            .then_some(crate::app::state::NavigatorTarget::Tab { ws_idx, tab_idx })
                    })
                }),
            crate::federation::FederatedResourceKind::Pane
            | crate::federation::FederatedResourceKind::Terminal
            | crate::federation::FederatedResourceKind::Agent => self
                .state
                .workspaces
                .iter()
                .enumerate()
                .find_map(|(ws_idx, workspace)| {
                    workspace
                        .tabs
                        .iter()
                        .enumerate()
                        .find_map(|(tab_idx, tab)| {
                            tab.panes.keys().find_map(|pane_id| {
                                let number = workspace.public_pane_number(*pane_id)?;
                                (crate::workspace::public_pane_id_for_number(&workspace.id, number)
                                    == resource.resource_id)
                                    .then_some(crate::app::state::NavigatorTarget::Pane {
                                        ws_idx,
                                        tab_idx,
                                        pane_id: *pane_id,
                                    })
                            })
                        })
                }),
            crate::federation::FederatedResourceKind::ArchivedAgent => {
                let archive_id = resource.resource_id.clone();
                return self.reopen_archived_agent(&archive_id).map(|_| ());
            }
        }
        .ok_or_else(|| {
            format!(
                "federated resource {} no longer exists",
                resource.resource_id
            )
        })?;

        self.state
            .focus_navigator_target(target)
            .then_some(())
            .ok_or_else(|| {
                format!(
                    "could not focus federated resource {}",
                    resource.resource_id
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{EmptyParams, Method, ResponseResult, SuccessResponse};
    use crate::{config::Config, workspace::Workspace};

    fn app_with_two_tabs() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("snapshot");
        workspace.test_add_tab(None);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app
    }

    #[test]
    fn session_snapshot_bootstraps_runtime_resources() {
        let mut app = app_with_two_tabs();
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_snapshot".into(),
            method: Method::SessionSnapshot(EmptyParams::default()),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::SessionSnapshot { snapshot } = success.result else {
            panic!("expected session snapshot response");
        };
        assert_eq!(success.id, "req_snapshot");
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.panes.len(), 2);
        assert_eq!(snapshot.layouts.len(), 2);
        assert_eq!(
            snapshot.focused_workspace_id.as_deref(),
            Some(snapshot.workspaces[0].workspace_id.as_str())
        );
        assert_eq!(
            snapshot.focused_tab_id.as_deref(),
            Some(snapshot.tabs[0].tab_id.as_str())
        );
        assert_eq!(
            snapshot.focused_pane_id.as_deref(),
            Some(snapshot.panes[0].pane_id.as_str())
        );
    }
}
