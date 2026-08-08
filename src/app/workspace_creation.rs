use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    state::{
        WorkspaceCreateServer, WorkspaceCreateServerKind, WorkspaceCreateState, WorkspaceCreateStep,
    },
    text_input::{action_for_key, TextInputState},
    App, Mode,
};

impl App {
    pub(crate) fn open_workspace_create_dialog(&mut self) {
        let local_directory = self
            .state
            .active
            .and_then(|ws_idx| self.state.workspaces.get(ws_idx))
            .and_then(|workspace| {
                workspace.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
            })
            .map(|path| path.display().to_string());
        let local_label = self
            .state
            .federation_member_label
            .clone()
            .unwrap_or_else(|| self.state.federation_member_id.clone());
        let mut servers = vec![WorkspaceCreateServer {
            kind: WorkspaceCreateServerKind::Local,
            member_id: self.state.federation_member_id.clone(),
            label: local_label,
            status: crate::federation::EndpointConnectionStatus::Connected,
            suggested_directory: local_directory,
        }];
        servers.extend(self.state.federation_states().map(|endpoint| {
            let suggested_directory = endpoint.snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .panes
                    .iter()
                    .find(|pane| pane.focused)
                    .or_else(|| snapshot.panes.first())
                    .and_then(|pane| pane.foreground_cwd.clone().or_else(|| pane.cwd.clone()))
            });
            WorkspaceCreateServer {
                kind: WorkspaceCreateServerKind::Federation {
                    endpoint_id: endpoint.endpoint.id.clone(),
                },
                member_id: endpoint.endpoint.id.clone(),
                label: endpoint
                    .endpoint
                    .label
                    .clone()
                    .unwrap_or_else(|| endpoint.endpoint.id.clone()),
                status: endpoint.status,
                suggested_directory,
            }
        }));
        self.state.workspace_create = Some(WorkspaceCreateState {
            servers,
            selected_server: 0,
            step: WorkspaceCreateStep::Server,
            directory: String::new(),
            directory_input: TextInputState::default(),
            open_after_creation: true,
            error: None,
            creating: false,
        });
        self.state.mode = Mode::NewWorkspace;
    }

    pub(crate) fn handle_workspace_create_key(&mut self, key: KeyEvent) {
        if self
            .state
            .workspace_create
            .as_ref()
            .is_some_and(|create| create.creating)
        {
            return;
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('g') && key.modifiers == KeyModifiers::CONTROL)
        {
            self.close_workspace_create_dialog();
            return;
        }

        let Some(step) = self
            .state
            .workspace_create
            .as_ref()
            .map(|create| create.step)
        else {
            return;
        };
        match step {
            WorkspaceCreateStep::Server => self.handle_workspace_server_key(key),
            WorkspaceCreateStep::Directory => self.handle_workspace_directory_key(key),
            WorkspaceCreateStep::AfterCreation => self.handle_workspace_after_creation_key(key),
        }
    }

    fn handle_workspace_server_key(&mut self, key: KeyEvent) {
        let Some(create) = self.state.workspace_create.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('p')
                if key.code == KeyCode::Up || key.modifiers == KeyModifiers::CONTROL =>
            {
                create.selected_server = create.selected_server.saturating_sub(1);
                create.error = None;
            }
            KeyCode::Down | KeyCode::Char('n')
                if key.code == KeyCode::Down || key.modifiers == KeyModifiers::CONTROL =>
            {
                create.selected_server =
                    (create.selected_server + 1).min(create.servers.len().saturating_sub(1));
                create.error = None;
            }
            KeyCode::Enter => self.advance_workspace_create_to_directory(),
            _ => {}
        }
    }

    fn advance_workspace_create_to_directory(&mut self) {
        let Some(create) = self.state.workspace_create.as_mut() else {
            return;
        };
        let Some(server) = create.servers.get(create.selected_server) else {
            return;
        };
        if server.status != crate::federation::EndpointConnectionStatus::Connected {
            create.error = Some(format!(
                "{} is {}; reconnect it before creating a workspace",
                server.label,
                workspace_server_status(server.status)
            ));
            return;
        }
        create.directory = server.suggested_directory.clone().unwrap_or_default();
        create.directory_input = TextInputState::at_end(&create.directory);
        if !create.directory.is_empty() {
            create.directory_input.apply(
                &mut create.directory,
                crate::app::text_input::TextInputAction::SelectAll,
                "",
            );
        }
        create.step = WorkspaceCreateStep::Directory;
        create.error = None;
    }

    fn handle_workspace_directory_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('p')
                if key.code == KeyCode::Up || key.modifiers == KeyModifiers::CONTROL =>
            {
                if let Some(create) = self.state.workspace_create.as_mut() {
                    create.step = WorkspaceCreateStep::Server;
                    create.error = None;
                }
            }
            KeyCode::Down | KeyCode::Char('n') | KeyCode::Enter
                if key.code == KeyCode::Down
                    || key.code == KeyCode::Enter
                    || key.modifiers == KeyModifiers::CONTROL =>
            {
                self.advance_workspace_create_to_action();
            }
            _ if action_for_key(key).is_some() => {
                let yank = self.state.text_input_yank.clone();
                if let Some(create) = self.state.workspace_create.as_mut() {
                    if let Some(killed) = action_for_key(key).and_then(|action| {
                        create
                            .directory_input
                            .apply(&mut create.directory, action, &yank)
                    }) {
                        self.state.text_input_yank = killed;
                    }
                    create.error = None;
                }
            }
            KeyCode::Char(ch)
                if (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                    && !ch.is_control() =>
            {
                self.insert_workspace_create_text(&ch.to_string());
            }
            _ => {}
        }
    }

    pub(crate) fn insert_workspace_create_text(&mut self, text: &str) -> bool {
        let Some(create) = self.state.workspace_create.as_mut() else {
            return false;
        };
        if create.step != WorkspaceCreateStep::Directory || create.creating {
            return false;
        }
        create
            .directory_input
            .insert_str(&mut create.directory, text);
        create.error = None;
        true
    }

    fn advance_workspace_create_to_action(&mut self) {
        let Some(create) = self.state.workspace_create.as_mut() else {
            return;
        };
        if create.directory.trim().is_empty() {
            create.error = Some("Choose or enter a directory on this server.".into());
            return;
        }
        create.directory = create.directory.trim().to_string();
        create.directory_input.move_to_end(&create.directory);
        create.step = WorkspaceCreateStep::AfterCreation;
        create.error = None;
    }

    fn handle_workspace_after_creation_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('p')
                if key.code == KeyCode::Up || key.modifiers == KeyModifiers::CONTROL =>
            {
                if let Some(create) = self.state.workspace_create.as_mut() {
                    create.step = WorkspaceCreateStep::Directory;
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('b') | KeyCode::Char('f')
                if matches!(key.code, KeyCode::Left | KeyCode::Right)
                    || key.modifiers == KeyModifiers::CONTROL =>
            {
                if let Some(create) = self.state.workspace_create.as_mut() {
                    create.open_after_creation = !create.open_after_creation;
                }
            }
            KeyCode::Enter => {
                if let Some(create) = self.state.workspace_create.as_mut() {
                    create.creating = true;
                    create.error = None;
                    self.state.request_submit_workspace_create = true;
                }
            }
            _ => {}
        }
    }

    pub(crate) fn submit_workspace_create(&mut self) {
        let Some(create) = self.state.workspace_create.as_ref() else {
            return;
        };
        let Some(server) = create.servers.get(create.selected_server).cloned() else {
            return;
        };
        let directory = create.directory.clone();
        let open_after_creation = create.open_after_creation;
        match server.kind {
            WorkspaceCreateServerKind::Local => {
                self.runtime_workspace_create(
                    "tui.workspace.create_dialog",
                    crate::api::schema::WorkspaceCreateParams {
                        cwd: Some(directory),
                        focus: open_after_creation,
                        label: None,
                        env: Default::default(),
                    },
                );
                self.close_workspace_create_dialog();
            }
            WorkspaceCreateServerKind::Federation { endpoint_id } => {
                self.submit_federated_workspace_create(
                    &endpoint_id,
                    directory,
                    open_after_creation,
                );
            }
        }
    }

    fn submit_federated_workspace_create(
        &mut self,
        endpoint_id: &str,
        directory: String,
        open_after_creation: bool,
    ) {
        let Some(endpoint) = self.state.federation_state(endpoint_id).cloned() else {
            self.workspace_create_failed("The selected federation server is no longer available.");
            return;
        };
        if endpoint.status != crate::federation::EndpointConnectionStatus::Connected {
            self.workspace_create_failed("The selected federation server disconnected.");
            return;
        }
        let request = crate::api::schema::Request {
            id: format!("federation:{endpoint_id}:workspace:create"),
            method: crate::api::schema::Method::WorkspaceCreate(
                crate::api::schema::WorkspaceCreateParams {
                    cwd: Some(directory),
                    focus: false,
                    label: None,
                    env: Default::default(),
                },
            ),
        };
        match crate::federation::request(&endpoint.endpoint, &request) {
            Ok(response) => {
                let crate::api::schema::ResponseResult::WorkspaceCreated { workspace, .. } =
                    response.result
                else {
                    self.workspace_create_failed(
                        "The remote server returned an unexpected workspace response.",
                    );
                    return;
                };
                self.state.workspace_create = None;
                if open_after_creation {
                    if !self.state.request_federation_target(
                        endpoint_id,
                        crate::federation::FederatedResourceKind::Workspace,
                        Some(workspace.workspace_id),
                    ) {
                        self.state.emacs.echo = Some(format!(
                            "Created {} on {}, but could not open it yet.",
                            workspace.label, endpoint_id
                        ));
                    }
                } else {
                    self.state.mode = if self.state.active.is_some() {
                        Mode::Terminal
                    } else {
                        Mode::Navigate
                    };
                    self.state.emacs.echo = Some(format!(
                        "Created {} on {} in the background.",
                        workspace.label, endpoint_id
                    ));
                }
            }
            Err(error) => self.workspace_create_failed(&format!(
                "Could not create the workspace on {endpoint_id}: {error}"
            )),
        }
    }

    fn workspace_create_failed(&mut self, message: &str) {
        if let Some(create) = self.state.workspace_create.as_mut() {
            create.creating = false;
            create.error = Some(message.into());
            create.step = WorkspaceCreateStep::AfterCreation;
        }
    }

    fn close_workspace_create_dialog(&mut self) {
        self.state.workspace_create = None;
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }
}

pub(crate) fn workspace_server_status(
    status: crate::federation::EndpointConnectionStatus,
) -> &'static str {
    match status {
        crate::federation::EndpointConnectionStatus::Disabled => "disabled",
        crate::federation::EndpointConnectionStatus::Connecting => "connecting",
        crate::federation::EndpointConnectionStatus::Connected => "connected",
        crate::federation::EndpointConnectionStatus::Disconnected => "disconnected",
        crate::federation::EndpointConnectionStatus::Incompatible => "incompatible",
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    fn add_endpoint(
        app: &mut App,
        id: &str,
        label: &str,
        status: crate::federation::EndpointConnectionStatus,
    ) {
        let endpoint = crate::config::FederationEndpointConfig {
            id: id.into(),
            target: format!("{id}.example"),
            label: Some(label.into()),
            session: "default".into(),
            enabled: true,
        };
        let mut state = crate::federation::EndpointState::configured(endpoint);
        state.status = status;
        app.state.federation.insert(id.into(), state);
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn dialog_starts_with_server_and_lists_federation_members() {
        let mut app = test_app();
        app.state.federation_member_id = "x1".into();
        app.state.federation_member_label = Some("Local".into());
        add_endpoint(
            &mut app,
            "stl-agents-1",
            "STL Agents",
            crate::federation::EndpointConnectionStatus::Connected,
        );
        add_endpoint(
            &mut app,
            "tana",
            "Tana",
            crate::federation::EndpointConnectionStatus::Disconnected,
        );

        app.open_workspace_create_dialog();

        let create = app.state.workspace_create.as_ref().unwrap();
        assert_eq!(app.state.mode, Mode::NewWorkspace);
        assert_eq!(create.step, WorkspaceCreateStep::Server);
        assert_eq!(create.servers.len(), 3);
        assert_eq!(create.servers[0].member_id, "x1");
        assert_eq!(create.servers[1].label, "STL Agents");
        assert_eq!(create.servers[2].label, "Tana");
    }

    #[test]
    fn emacs_keys_drive_server_directory_and_after_creation_steps() {
        let mut app = test_app();
        add_endpoint(
            &mut app,
            "stl-agents-1",
            "STL Agents",
            crate::federation::EndpointConnectionStatus::Connected,
        );
        app.open_workspace_create_dialog();

        app.handle_workspace_create_key(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(
            app.state.workspace_create.as_ref().unwrap().selected_server,
            1
        );
        app.handle_workspace_create_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.state.workspace_create.as_ref().unwrap().step,
            WorkspaceCreateStep::Directory
        );
        assert!(app.insert_workspace_create_text("/srv/projects/herdr"));
        app.handle_workspace_create_key(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        let create = app.state.workspace_create.as_ref().unwrap();
        assert_eq!(create.step, WorkspaceCreateStep::AfterCreation);
        assert!(create.open_after_creation);

        app.handle_workspace_create_key(key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(
            !app.state
                .workspace_create
                .as_ref()
                .unwrap()
                .open_after_creation
        );
        app.handle_workspace_create_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(
            app.state.workspace_create.as_ref().unwrap().step,
            WorkspaceCreateStep::Directory
        );
        app.handle_workspace_create_key(key(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(
            app.state.workspace_create.as_ref().unwrap().step,
            WorkspaceCreateStep::Server
        );
        app.handle_workspace_create_key(key(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(app.state.workspace_create.is_none());
    }

    #[test]
    fn disconnected_server_cannot_advance_to_directory() {
        let mut app = test_app();
        add_endpoint(
            &mut app,
            "tana",
            "Tana",
            crate::federation::EndpointConnectionStatus::Disconnected,
        );
        app.open_workspace_create_dialog();
        app.handle_workspace_create_key(key(KeyCode::Char('n'), KeyModifiers::CONTROL));
        app.handle_workspace_create_key(key(KeyCode::Enter, KeyModifiers::NONE));

        let create = app.state.workspace_create.as_ref().unwrap();
        assert_eq!(create.step, WorkspaceCreateStep::Server);
        assert!(create.error.as_deref().unwrap().contains("disconnected"));
    }

    #[test]
    fn enter_on_final_step_queues_creation_without_bypassing_modal_state() {
        let mut app = test_app();
        app.open_workspace_create_dialog();
        app.handle_workspace_create_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.insert_workspace_create_text("/tmp/new-project"));
        app.handle_workspace_create_key(key(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_workspace_create_key(key(KeyCode::Enter, KeyModifiers::NONE));

        let create = app.state.workspace_create.as_ref().unwrap();
        assert!(create.creating);
        assert!(app.state.request_submit_workspace_create);
    }
}
