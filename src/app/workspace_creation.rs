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
            directory_parent: None,
            directory_entries: Vec::new(),
            selected_directory_entry: 0,
            directory_dirty: false,
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
        if key.code == KeyCode::Esc {
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
        if key.code == KeyCode::Char('g') && key.modifiers == KeyModifiers::CONTROL {
            match step {
                WorkspaceCreateStep::Server => self.close_workspace_create_dialog(),
                WorkspaceCreateStep::Directory => {
                    if let Some(create) = self.state.workspace_create.as_mut() {
                        create.step = WorkspaceCreateStep::Server;
                        create.error = None;
                    }
                }
                WorkspaceCreateStep::AfterCreation => {
                    if let Some(create) = self.state.workspace_create.as_mut() {
                        create.step = WorkspaceCreateStep::Directory;
                        create.error = None;
                    }
                }
            }
            return;
        }
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
        let Some((server, suggested_directory)) =
            self.state.workspace_create.as_ref().and_then(|create| {
                create
                    .servers
                    .get(create.selected_server)
                    .cloned()
                    .map(|server| {
                        let suggested = server
                            .suggested_directory
                            .clone()
                            .unwrap_or_else(|| "~".into());
                        (server, suggested)
                    })
            })
        else {
            return;
        };
        if server.status != crate::federation::EndpointConnectionStatus::Connected {
            let Some(create) = self.state.workspace_create.as_mut() else {
                return;
            };
            create.error = Some(format!(
                "{} is {}; reconnect it before creating a workspace",
                server.label,
                workspace_server_status(server.status)
            ));
            return;
        }
        if let Err(error) = self.load_workspace_directory(&server, &suggested_directory) {
            if let Some(create) = self.state.workspace_create.as_mut() {
                create.error = Some(format!(
                    "Could not browse directories on {}: {error}",
                    server.label
                ));
            }
        }
    }

    fn handle_workspace_directory_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('p')
                if key.code == KeyCode::Up || key.modifiers == KeyModifiers::CONTROL =>
            {
                if let Some(create) = self.state.workspace_create.as_mut() {
                    create.selected_directory_entry =
                        create.selected_directory_entry.saturating_sub(1);
                    create.error = None;
                }
            }
            KeyCode::Down | KeyCode::Char('n')
                if key.code == KeyCode::Down || key.modifiers == KeyModifiers::CONTROL =>
            {
                if let Some(create) = self.state.workspace_create.as_mut() {
                    let row_count = 1
                        + usize::from(create.directory_parent.is_some())
                        + create.directory_entries.len();
                    create.selected_directory_entry =
                        (create.selected_directory_entry + 1).min(row_count.saturating_sub(1));
                    create.error = None;
                }
            }
            KeyCode::Enter => self.activate_workspace_directory_entry(),
            KeyCode::Tab => self.activate_workspace_directory_entry(),
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
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
                    create.directory_dirty = true;
                    create.selected_directory_entry = 0;
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
        create.directory_dirty = true;
        create.selected_directory_entry = 0;
        create.error = None;
        true
    }

    fn activate_workspace_directory_entry(&mut self) {
        let Some(create) = self.state.workspace_create.as_ref() else {
            return;
        };
        if create.directory_dirty {
            let path = create.directory.trim().to_string();
            let Some(server) = create.servers.get(create.selected_server).cloned() else {
                return;
            };
            if let Err(error) = self.load_workspace_directory(&server, &path) {
                if let Some(create) = self.state.workspace_create.as_mut() {
                    create.error = Some(error.to_string());
                }
            }
            return;
        }
        let selected = create.selected_directory_entry;
        if selected == 0 {
            self.advance_workspace_create_to_action();
            return;
        }
        let parent_offset = usize::from(create.directory_parent.is_some());
        let path = if selected == 1 && parent_offset == 1 {
            create.directory_parent.clone()
        } else {
            create
                .directory_entries
                .get(selected.saturating_sub(1 + parent_offset))
                .map(|entry| entry.path.clone())
        };
        let Some(path) = path else {
            return;
        };
        let Some(server) = create.servers.get(create.selected_server).cloned() else {
            return;
        };
        if let Err(error) = self.load_workspace_directory(&server, &path) {
            if let Some(create) = self.state.workspace_create.as_mut() {
                create.error = Some(error.to_string());
            }
        }
    }

    fn load_workspace_directory(
        &mut self,
        server: &WorkspaceCreateServer,
        path: &str,
    ) -> std::io::Result<()> {
        let (path, parent, entries) = match &server.kind {
            WorkspaceCreateServerKind::Local => list_workspace_directories(path)?,
            WorkspaceCreateServerKind::Federation { endpoint_id } => {
                let endpoint = self.state.federation_state(endpoint_id).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        "federation endpoint is no longer available",
                    )
                })?;
                let request = crate::api::schema::Request {
                    id: format!("federation:{endpoint_id}:directory:list"),
                    method: crate::api::schema::Method::DirectoryList(
                        crate::api::schema::DirectoryListParams { path: path.into() },
                    ),
                };
                let response = crate::federation::request(&endpoint.endpoint, &request)?;
                let crate::api::schema::ResponseResult::DirectoryList {
                    path,
                    parent,
                    entries,
                } = response.result
                else {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "remote server returned an unexpected directory response",
                    ));
                };
                (path, parent, entries)
            }
        };
        let Some(create) = self.state.workspace_create.as_mut() else {
            return Ok(());
        };
        create.directory = path;
        create.directory_input = TextInputState::at_end(&create.directory);
        create.directory_parent = parent;
        create.directory_entries = entries;
        create.selected_directory_entry = 0;
        create.directory_dirty = false;
        create.step = WorkspaceCreateStep::Directory;
        create.error = None;
        Ok(())
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
        let label = inferred_workspace_name(&directory);
        match server.kind {
            WorkspaceCreateServerKind::Local => {
                if let Some(existing) =
                    self.state
                        .workspaces
                        .iter()
                        .enumerate()
                        .find_map(|(index, workspace)| {
                            workspace
                                .resolved_identity_cwd_from(
                                    &self.state.terminals,
                                    &self.terminal_runtimes,
                                )
                                .filter(|cwd| local_paths_match(cwd, &directory))
                                .map(|_| index)
                        })
                {
                    let existing_label = self.state.workspaces[existing]
                        .display_name_from(&self.state.terminals, &self.terminal_runtimes);
                    if open_after_creation {
                        self.state.switch_workspace(existing);
                    }
                    self.close_workspace_create_dialog();
                    self.state.emacs.echo = Some(format!(
                        "{existing_label} is already open on {}{}.",
                        server.label,
                        if open_after_creation {
                            ""
                        } else {
                            "; left in the background"
                        }
                    ));
                    return;
                }
                self.runtime_workspace_create(
                    "tui.workspace.create_dialog",
                    crate::api::schema::WorkspaceCreateParams {
                        cwd: Some(directory),
                        focus: open_after_creation,
                        label: Some(label),
                        env: Default::default(),
                    },
                );
                self.close_workspace_create_dialog();
            }
            WorkspaceCreateServerKind::Federation { endpoint_id } => {
                self.submit_federated_workspace_create(
                    &endpoint_id,
                    directory,
                    label,
                    open_after_creation,
                );
            }
        }
    }

    fn submit_federated_workspace_create(
        &mut self,
        endpoint_id: &str,
        directory: String,
        label: String,
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
        if let Some((workspace_id, workspace_label)) =
            endpoint.snapshot.as_ref().and_then(|snapshot| {
                let workspace_id = snapshot
                    .panes
                    .iter()
                    .find(|pane| {
                        pane.foreground_cwd
                            .as_deref()
                            .or(pane.cwd.as_deref())
                            .is_some_and(|cwd| remote_paths_match(cwd, &directory))
                    })?
                    .workspace_id
                    .clone();
                let label = snapshot
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.workspace_id == workspace_id)
                    .map(|workspace| workspace.label.clone())
                    .unwrap_or_else(|| workspace_id.clone());
                Some((workspace_id, label))
            })
        {
            self.state.workspace_create = None;
            if open_after_creation {
                if !self.state.request_federation_target(
                    endpoint_id,
                    crate::federation::FederatedResourceKind::Workspace,
                    Some(workspace_id),
                ) {
                    self.state.emacs.echo = Some(format!(
                        "{workspace_label} is already open on {endpoint_id}, but could not be opened yet."
                    ));
                }
            } else {
                self.state.mode = if self.state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                };
                self.state.emacs.echo = Some(format!(
                    "{workspace_label} is already open on {endpoint_id}; left in the background."
                ));
            }
            return;
        }
        let request = crate::api::schema::Request {
            id: format!("federation:{endpoint_id}:workspace:create"),
            method: crate::api::schema::Method::WorkspaceCreate(
                crate::api::schema::WorkspaceCreateParams {
                    cwd: Some(directory),
                    focus: false,
                    label: Some(label),
                    env: Default::default(),
                },
            ),
        };
        match crate::federation::request(&endpoint.endpoint, &request) {
            Ok(response) => {
                let crate::api::schema::ResponseResult::WorkspaceCreated {
                    workspace,
                    tab,
                    root_pane,
                } = response.result
                else {
                    self.workspace_create_failed(
                        "The remote server returned an unexpected workspace response.",
                    );
                    return;
                };
                if let Some(snapshot) = self
                    .state
                    .federation
                    .get_mut(endpoint_id)
                    .and_then(|endpoint| endpoint.snapshot.as_mut())
                {
                    record_federated_workspace_created(snapshot, &workspace, &tab, &root_pane);
                }
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

fn record_federated_workspace_created(
    snapshot: &mut crate::api::schema::SessionSnapshot,
    workspace: &crate::api::schema::WorkspaceInfo,
    tab: &crate::api::schema::TabInfo,
    pane: &crate::api::schema::PaneInfo,
) {
    snapshot
        .workspaces
        .retain(|existing| existing.workspace_id != workspace.workspace_id);
    snapshot
        .tabs
        .retain(|existing| existing.tab_id != tab.tab_id);
    snapshot
        .panes
        .retain(|existing| existing.pane_id != pane.pane_id);
    snapshot.workspaces.push(workspace.clone());
    snapshot.tabs.push(tab.clone());
    snapshot.panes.push(pane.clone());
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

pub(crate) fn inferred_workspace_name(directory: &str) -> String {
    let path = std::path::Path::new(directory.trim());
    let components = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let leaf = components
        .last()
        .cloned()
        .unwrap_or_else(|| "workspace".into());
    let worktree_project = components
        .iter()
        .position(|component| component == "worktrees")
        .and_then(|index| components.get(index + 1..))
        .filter(|tail| tail.len() >= 2)
        .and_then(|tail| tail.get(tail.len() - 2))
        .cloned();
    if let Some(project) = worktree_project {
        return project;
    }
    let checkout_id =
        (8..=40).contains(&leaf.len()) && leaf.bytes().all(|byte| byte.is_ascii_hexdigit());
    if checkout_id {
        return components.iter().rev().nth(1).cloned().unwrap_or(leaf);
    }
    leaf
}

fn local_paths_match(existing: &std::path::Path, requested: &str) -> bool {
    let requested = crate::worktree::expand_tilde_absolute_path(requested);
    crate::worktree::canonical_or_original(existing)
        == crate::worktree::canonical_or_original(&requested)
}

fn remote_paths_match(existing: &str, requested: &str) -> bool {
    existing.trim_end_matches(std::path::MAIN_SEPARATOR)
        == requested.trim_end_matches(std::path::MAIN_SEPARATOR)
}

pub(crate) fn list_workspace_directories(
    path: &str,
) -> std::io::Result<(
    String,
    Option<String>,
    Vec<crate::api::schema::DirectoryEntry>,
)> {
    let expanded = crate::worktree::expand_tilde_absolute_path(path);
    let resolved = std::fs::canonicalize(&expanded).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("cannot open {}: {error}", expanded.display()),
        )
    })?;
    if !resolved.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("{} is not a directory", resolved.display()),
        ));
    }

    let mut entries = std::fs::read_dir(&resolved)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir() || kind.is_symlink())?;
            let path = entry.path();
            path.is_dir().then(|| crate::api::schema::DirectoryEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path: path.display().to_string(),
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    let parent = resolved.parent().map(|path| path.display().to_string());
    Ok((resolved.display().to_string(), parent, entries))
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
        app.open_workspace_create_dialog();

        app.handle_workspace_create_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            app.state.workspace_create.as_ref().unwrap().step,
            WorkspaceCreateStep::Directory
        );
        app.handle_workspace_create_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
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
        app.handle_workspace_create_key(key(KeyCode::Char('g'), KeyModifiers::CONTROL));
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
        app.handle_workspace_create_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        app.handle_workspace_create_key(key(KeyCode::Enter, KeyModifiers::NONE));

        let create = app.state.workspace_create.as_ref().unwrap();
        assert!(create.creating);
        assert!(app.state.request_submit_workspace_create);
    }

    #[test]
    fn inferred_name_uses_project_instead_of_handoff_checkout_id() {
        assert_eq!(
            inferred_workspace_name(
                "/home/paul/worktrees/github.com/socialtechnologylab/zojb/10643eba0e39"
            ),
            "zojb"
        );
        assert_eq!(inferred_workspace_name("/srv/projects/herdr"), "herdr");
    }

    #[test]
    fn directory_browser_lists_only_directories_and_supports_parent_navigation() {
        let root =
            std::env::temp_dir().join(format!("herdr-workspace-browser-{}", std::process::id()));
        let child = root.join("project");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(root.join("not-a-directory"), "fixture").unwrap();

        let (path, parent, entries) =
            list_workspace_directories(&root.display().to_string()).unwrap();
        assert_eq!(
            path,
            std::fs::canonicalize(&root).unwrap().display().to_string()
        );
        assert!(parent.is_some());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "project");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_response_immediately_populates_the_remote_navigation_snapshot() {
        let workspace: crate::api::schema::WorkspaceInfo =
            serde_json::from_value(serde_json::json!({
                "workspace_id": "wB", "number": 3, "label": "project",
                "focused": false, "pane_count": 1, "tab_count": 1,
                "active_tab_id": "wB:t1", "agent_status": "unknown"
            }))
            .unwrap();
        let tab: crate::api::schema::TabInfo = serde_json::from_value(serde_json::json!({
            "tab_id": "wB:t1", "workspace_id": "wB", "number": 1,
            "label": "1", "focused": false, "pane_count": 1,
            "agent_status": "unknown"
        }))
        .unwrap();
        let pane: crate::api::schema::PaneInfo = serde_json::from_value(serde_json::json!({
            "pane_id": "wB:p1", "terminal_id": "terminal-B",
            "workspace_id": "wB", "tab_id": "wB:t1", "focused": false,
            "cwd": "/tmp/project", "agent_status": "unknown", "revision": 0
        }))
        .unwrap();
        let mut snapshot = crate::api::schema::SessionSnapshot {
            identity: crate::api::schema::RuntimeIdentity::default(),
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            event_cursor: 0,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            layouts: Vec::new(),
            agents: Vec::new(),
        };

        record_federated_workspace_created(&mut snapshot, &workspace, &tab, &pane);

        assert!(snapshot
            .workspaces
            .iter()
            .any(|candidate| candidate.workspace_id == "wB"));
        assert!(snapshot
            .panes
            .iter()
            .any(|candidate| candidate.cwd.as_deref() == Some("/tmp/project")));
    }
}
