//! Personal Herdr federation over SSH-backed JSON API streams.
//!
//! Every endpoint remains authoritative for its own PTYs and session state.
//! This module only transports qualified references, snapshots, and events.

use std::io::{self, BufReader, Read as _, Write as _};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::api::schema::{
    EventData, Method, Request, ResponseResult, SequencedEventEnvelope, SessionSnapshot,
    SessionWatchParams, SuccessResponse,
};
use crate::config::{FederationConfig, FederationEndpointConfig};

const CONNECT_TIMEOUT_SECONDS: u64 = 10;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(15);
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FederatedResourceRef {
    pub endpoint_id: String,
    pub server_id: String,
    pub session_id: String,
    pub kind: FederatedResourceKind,
    pub resource_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FederatedResourceKind {
    Workspace,
    Tab,
    Pane,
    Terminal,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointConnectionStatus {
    Disabled,
    Connecting,
    Connected,
    Disconnected,
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointState {
    pub endpoint: FederationEndpointConfig,
    pub status: EndpointConnectionStatus,
    #[serde(default)]
    pub snapshot: Option<SessionSnapshot>,
    #[serde(default)]
    pub cursor: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

impl EndpointState {
    pub fn configured(endpoint: FederationEndpointConfig) -> Self {
        let status = if endpoint.enabled {
            EndpointConnectionStatus::Disconnected
        } else {
            EndpointConnectionStatus::Disabled
        };
        Self {
            endpoint,
            status,
            snapshot: None,
            cursor: None,
            error: None,
        }
    }

    pub fn resource_ref(
        &self,
        kind: FederatedResourceKind,
        resource_id: impl Into<String>,
    ) -> Option<FederatedResourceRef> {
        let identity = &self.snapshot.as_ref()?.identity;
        Some(FederatedResourceRef {
            endpoint_id: self.endpoint.id.clone(),
            server_id: identity.server_id.clone(),
            session_id: identity.session_id.clone(),
            kind,
            resource_id: resource_id.into(),
        })
    }
}

pub fn configured_states(config: &FederationConfig) -> Vec<EndpointState> {
    config
        .endpoints
        .iter()
        .cloned()
        .map(EndpointState::configured)
        .collect()
}

/// Reconnect an endpoint forever, resuming from the last cursor when possible.
/// Returning `false` from `publish` stops the worker.
pub fn run_endpoint_watch(
    endpoint: FederationEndpointConfig,
    mut publish: impl FnMut(EndpointState) -> bool,
) {
    if !endpoint.enabled {
        let _ = publish(EndpointState::configured(endpoint));
        return;
    }
    let mut cursor = None;
    let mut snapshot = None;
    let mut backoff = Duration::from_secs(1);
    loop {
        if !publish(EndpointState {
            endpoint: endpoint.clone(),
            status: EndpointConnectionStatus::Connecting,
            snapshot: snapshot.clone(),
            cursor,
            error: None,
        }) {
            return;
        }
        let resume_identity = snapshot.as_ref().map(|snapshot| &snapshot.identity);
        match EndpointWatch::connect(endpoint.clone(), cursor, resume_identity) {
            Ok(mut watch) => {
                if watch.state.snapshot.is_none() {
                    watch.state.snapshot = snapshot.take();
                }
                cursor = watch.state.cursor;
                snapshot = watch.state.snapshot.clone();
                if !publish(watch.state.clone()) {
                    return;
                }
                backoff = Duration::from_secs(1);
                loop {
                    match watch.next() {
                        Ok(Some(_)) => {
                            cursor = watch.state.cursor;
                            snapshot = watch.state.snapshot.clone();
                            if !publish(watch.state.clone()) {
                                return;
                            }
                        }
                        Ok(None) => {
                            if !publish(EndpointState {
                                endpoint: endpoint.clone(),
                                status: EndpointConnectionStatus::Disconnected,
                                snapshot: snapshot.clone(),
                                cursor,
                                error: Some("federation stream closed; reconnecting".into()),
                            }) {
                                return;
                            }
                            break;
                        }
                        Err(err) => {
                            if !publish(EndpointState {
                                endpoint: endpoint.clone(),
                                status: classify_error(&err),
                                snapshot: snapshot.clone(),
                                cursor,
                                error: Some(err.to_string()),
                            }) {
                                return;
                            }
                            break;
                        }
                    }
                }
            }
            Err(err) => {
                if !publish(EndpointState {
                    endpoint: endpoint.clone(),
                    status: classify_error(&err),
                    snapshot: snapshot.clone(),
                    cursor,
                    error: Some(err.to_string()),
                }) {
                    return;
                }
            }
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

pub fn classify_error(err: &io::Error) -> EndpointConnectionStatus {
    let message = err.to_string();
    if message.contains("session.watch") || message.contains("does not support federation") {
        EndpointConnectionStatus::Incompatible
    } else {
        EndpointConnectionStatus::Disconnected
    }
}

pub fn start_app_watchers(
    config: &FederationConfig,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    expected_generation: u64,
) {
    if !config.enabled {
        return;
    }
    for endpoint in config.endpoints.iter().filter(|endpoint| endpoint.enabled) {
        let endpoint = endpoint.clone();
        let event_tx = event_tx.clone();
        let generation = generation.clone();
        std::thread::spawn(move || {
            run_endpoint_watch(endpoint, |state| {
                if generation.load(std::sync::atomic::Ordering::Acquire) != expected_generation {
                    return false;
                }
                event_tx
                    .blocking_send(crate::events::AppEvent::FederationUpdated(Box::new(state)))
                    .is_ok()
            });
        });
    }
}

/// A live `session.watch` stream transported through one SSH process.
pub struct EndpointWatch {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    pub state: EndpointState,
}

struct ReapedChild(Option<Child>);

impl ReapedChild {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn take(&mut self) -> Child {
        self.0.take().expect("child already transferred")
    }
}

impl std::ops::Deref for ReapedChild {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("child already transferred")
    }
}

impl std::ops::DerefMut for ReapedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("child already transferred")
    }
}

impl Drop for ReapedChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl EndpointWatch {
    pub fn connect(
        endpoint: FederationEndpointConfig,
        after_cursor: Option<u64>,
        resume_identity: Option<&crate::api::schema::RuntimeIdentity>,
    ) -> io::Result<Self> {
        validate_endpoint(&endpoint)?;
        let mut child = ReapedChild::new(
            ssh_bridge_command(&endpoint)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh: {err}")))?,
        );

        let request = Request {
            id: format!("federation:{}:watch", endpoint.id),
            method: Method::SessionWatch(SessionWatchParams {
                after_cursor,
                member_id: resume_identity.map(|identity| identity.member_id.clone()),
                server_id: resume_identity.map(|identity| identity.server_id.clone()),
                session_id: resume_identity.map(|identity| identity.session_id.clone()),
            }),
        };
        if let Some(mut stdin) = child.stdin.take() {
            write_json_line(&mut stdin, &request)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh bridge stdin is unavailable",
            ));
        }
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh bridge stdout is unavailable",
            )
        })?;
        let reader = BufReader::new(stdout);
        let (reader, ack) =
            read_success_response_with_timeout(&mut child, reader).map_err(|err| {
                endpoint_stream_error(&mut child, &endpoint, "watch handshake failed", err)
            })?;
        let ResponseResult::SessionWatchStarted {
            cursor,
            resumed: _,
            snapshot,
        } = ack.result
        else {
            return Err(io::Error::other(format!(
                "endpoint {} returned an unexpected watch response",
                endpoint.id
            )));
        };

        let snapshot = snapshot.map(|snapshot| *snapshot);
        if let Some(snapshot) = &snapshot {
            validate_snapshot_member(&endpoint, snapshot)?;
        }
        let state = EndpointState {
            endpoint,
            status: EndpointConnectionStatus::Connected,
            snapshot,
            cursor: Some(cursor),
            error: None,
        };
        Ok(Self {
            child: child.take(),
            reader,
            state,
        })
    }

    pub fn next(&mut self) -> io::Result<Option<SequencedEventEnvelope>> {
        let event: Option<SequencedEventEnvelope> = read_optional_json_line(&mut self.reader)?;
        let Some(event) = event else {
            let message = child_stderr(&mut self.child);
            return if message.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::other(message))
            };
        };
        self.state.cursor = Some(event.cursor);
        if let Some(snapshot) = &mut self.state.snapshot {
            snapshot.event_cursor = event.cursor;
            apply_event(snapshot, &event);
        }
        Ok(Some(event))
    }
}

fn validate_snapshot_member(
    endpoint: &FederationEndpointConfig,
    snapshot: &SessionSnapshot,
) -> io::Result<()> {
    if snapshot.identity.member_id == endpoint.id {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "endpoint {} reported federation member {}",
            endpoint.id, snapshot.identity.member_id
        ),
    ))
}

impl Drop for EndpointWatch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn request(
    endpoint: &FederationEndpointConfig,
    request: &Request,
) -> io::Result<SuccessResponse> {
    validate_endpoint(endpoint)?;
    let mut child = ReapedChild::new(
        ssh_bridge_command(endpoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh: {err}")))?,
    );
    if let Some(mut stdin) = child.stdin.take() {
        write_json_line(&mut stdin, request)?;
    }
    let stdout = child.stdout.take().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "ssh bridge stdout is unavailable",
        )
    })?;
    let reader = BufReader::new(stdout);
    read_success_response_with_timeout(&mut child, reader)
        .map(|(_, response)| response)
        .map_err(|err| endpoint_stream_error(&mut child, endpoint, "request failed", err))
}

pub fn run_stdio_bridge() -> io::Result<()> {
    ensure_local_server_ready()?;
    let status = crate::api::read_runtime_status_at(&crate::api::socket_path(), STATUS_TIMEOUT)?
        .ok_or_else(|| io::Error::other("local Herdr status API is unavailable"))?;
    if !status
        .capabilities
        .as_ref()
        .is_some_and(|capabilities| capabilities.session_watch)
    {
        return Err(io::Error::other(
            "local Herdr server does not support federation session.watch",
        ));
    }

    let stream = crate::ipc::connect_local_stream(&crate::api::socket_path())?;
    copy_stdio(stream)
}

fn ensure_local_server_ready() -> io::Result<()> {
    if crate::api::read_runtime_status_at(&crate::api::socket_path(), STATUS_TIMEOUT)?.is_some() {
        return Ok(());
    }
    crate::server::autodetect::spawn_server_daemon()?;
    crate::server::autodetect::wait_for_server_socket(
        &crate::server::socket_paths::client_socket_path(),
        SERVER_READY_TIMEOUT,
    )
}

#[cfg(unix)]
fn copy_stdio(stream: crate::ipc::LocalStream) -> io::Result<()> {
    use interprocess::TryClone as _;

    let mut upload_stream = stream.try_clone()?;
    let mut download_stream = stream;
    let upload = std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        io::copy(&mut stdin, &mut upload_stream).map(|_| ())
    });
    let mut stdout = io::stdout().lock();
    io::copy(&mut download_stream, &mut stdout)?;
    stdout.flush()?;
    upload
        .join()
        .map_err(|_| io::Error::other("federation bridge input thread panicked"))?
}

#[cfg(windows)]
fn copy_stdio(mut stream: crate::ipc::LocalStream) -> io::Result<()> {
    use interprocess::TryClone as _;

    let mut upload_stream = stream.try_clone()?;
    let upload = std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        io::copy(&mut stdin, &mut upload_stream).map(|_| ())
    });
    let mut stdout = io::stdout().lock();
    io::copy(&mut stream, &mut stdout)?;
    stdout.flush()?;
    upload
        .join()
        .map_err(|_| io::Error::other("federation bridge input thread panicked"))?
}

fn validate_endpoint(endpoint: &FederationEndpointConfig) -> io::Result<()> {
    let config = FederationConfig {
        enabled: true,
        endpoints: vec![endpoint.clone()],
        ..FederationConfig::default()
    };
    let diagnostics = config.diagnostics();
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            diagnostics.join("; "),
        ))
    }
}

fn ssh_bridge_command(endpoint: &FederationEndpointConfig) -> Command {
    let mut command = Command::new("ssh");
    command
        .arg("-T")
        .arg("-o")
        .arg("ControlMaster=no")
        .arg("-o")
        .arg("ControlPath=none")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={CONNECT_TIMEOUT_SECONDS}"))
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=3")
        .arg(&endpoint.target)
        .arg(remote_bridge_command(&endpoint.session));
    command
}

fn remote_bridge_command(session: &str) -> String {
    format!(
        "if command -v herdr >/dev/null 2>&1; then exec herdr --session {session} federation bridge; elif [ -x \"$HOME/.local/bin/herdr\" ]; then exec \"$HOME/.local/bin/herdr\" --session {session} federation bridge; else echo 'herdr executable not found' >&2; exit 127; fi"
    )
}

fn write_json_line(mut writer: impl io::Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut writer, value).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn read_required_json_line<T: serde::de::DeserializeOwned>(
    reader: &mut impl io::BufRead,
) -> io::Result<T> {
    read_optional_json_line(reader)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "bridge closed without a response",
        )
    })
}

fn read_success_response(reader: &mut impl io::BufRead) -> io::Result<SuccessResponse> {
    let value: serde_json::Value = read_required_json_line(reader)?;
    crate::api::client::parse_response_value(value).map_err(io::Error::other)
}

fn read_success_response_with_timeout(
    child: &mut Child,
    mut reader: BufReader<std::process::ChildStdout>,
) -> io::Result<(BufReader<std::process::ChildStdout>, SuccessResponse)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let response = read_success_response(&mut reader);
        let _ = tx.send((reader, response));
    });
    match rx.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok((reader, response)) => {
            let _ = worker.join();
            response.map(|response| (reader, response))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            drop(worker);
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "federation bridge produced no response within {} seconds",
                    HANDSHAKE_TIMEOUT.as_secs()
                ),
            ))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let _ = worker.join();
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "federation bridge response worker stopped",
            ))
        }
    }
}

fn read_optional_json_line<T: serde::de::DeserializeOwned>(
    reader: &mut impl io::BufRead,
) -> io::Result<Option<T>> {
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(io::Error::other)
}

fn endpoint_stream_error(
    child: &mut Child,
    endpoint: &FederationEndpointConfig,
    context: &str,
    source: io::Error,
) -> io::Error {
    let _ = child.kill();
    let _ = child.wait();
    let stderr = child_stderr(child);
    let detail = if stderr.is_empty() {
        source.to_string()
    } else {
        format!("{source}: {stderr}")
    };
    io::Error::new(
        source.kind(),
        format!("endpoint {} {context}: {detail}", endpoint.id),
    )
}

fn child_stderr(child: &mut Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return String::new();
    };
    let mut output = String::new();
    let _ = stderr.read_to_string(&mut output);
    output.trim().to_string()
}

fn upsert_by<T>(items: &mut Vec<T>, item: T, same: impl Fn(&T, &T) -> bool) {
    if let Some(existing) = items.iter_mut().find(|existing| same(existing, &item)) {
        *existing = item;
    } else {
        items.push(item);
    }
}

/// Reduce a sequenced event into a cached snapshot used by the combined navigator.
pub fn apply_event(snapshot: &mut SessionSnapshot, envelope: &SequencedEventEnvelope) {
    match &envelope.event.data {
        EventData::WorkspaceCreated { workspace }
        | EventData::WorkspaceUpdated { workspace }
        | EventData::WorkspaceMetadataUpdated { workspace }
        | EventData::WorktreeCreated { workspace, .. }
        | EventData::WorktreeOpened { workspace, .. } => upsert_by(
            &mut snapshot.workspaces,
            workspace.clone(),
            |left, right| left.workspace_id == right.workspace_id,
        ),
        EventData::WorkspaceClosed { workspace_id, .. }
        | EventData::WorktreeRemoved { workspace_id, .. } => {
            snapshot
                .workspaces
                .retain(|workspace| workspace.workspace_id != *workspace_id);
            snapshot
                .tabs
                .retain(|tab| tab.workspace_id != *workspace_id);
            snapshot
                .panes
                .retain(|pane| pane.workspace_id != *workspace_id);
            snapshot
                .layouts
                .retain(|layout| layout.workspace_id != *workspace_id);
        }
        EventData::WorkspaceRenamed {
            workspace_id,
            label,
        } => {
            if let Some(workspace) = snapshot
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.workspace_id == *workspace_id)
            {
                workspace.label.clone_from(label);
            }
        }
        EventData::WorkspaceMoved { workspaces, .. } => snapshot.workspaces.clone_from(workspaces),
        EventData::WorkspaceFocused { workspace_id } => {
            snapshot.focused_workspace_id = Some(workspace_id.clone());
            for workspace in &mut snapshot.workspaces {
                workspace.focused = workspace.workspace_id == *workspace_id;
            }
        }
        EventData::TabCreated { tab } => {
            upsert_by(&mut snapshot.tabs, tab.clone(), |left, right| {
                left.tab_id == right.tab_id
            })
        }
        EventData::TabClosed { tab_id, .. } => {
            snapshot.tabs.retain(|tab| tab.tab_id != *tab_id);
            snapshot.panes.retain(|pane| pane.tab_id != *tab_id);
            snapshot.layouts.retain(|layout| layout.tab_id != *tab_id);
        }
        EventData::TabRenamed { tab_id, label, .. } => {
            if let Some(tab) = snapshot.tabs.iter_mut().find(|tab| tab.tab_id == *tab_id) {
                tab.label.clone_from(label);
            }
        }
        EventData::TabMoved {
            tabs, workspace_id, ..
        } => {
            snapshot
                .tabs
                .retain(|tab| tab.workspace_id != *workspace_id);
            snapshot.tabs.extend(tabs.iter().cloned());
        }
        EventData::TabFocused {
            tab_id,
            workspace_id,
        } => {
            snapshot.focused_workspace_id = Some(workspace_id.clone());
            snapshot.focused_tab_id = Some(tab_id.clone());
            for tab in &mut snapshot.tabs {
                tab.focused = tab.tab_id == *tab_id;
            }
        }
        EventData::PaneCreated { pane } | EventData::PaneUpdated { pane } => {
            upsert_by(&mut snapshot.panes, pane.clone(), |left, right| {
                left.pane_id == right.pane_id
            })
        }
        EventData::PaneClosed { pane_id, .. } => {
            snapshot.panes.retain(|pane| pane.pane_id != *pane_id);
        }
        EventData::PaneFocused {
            pane_id,
            workspace_id,
        } => {
            snapshot.focused_workspace_id = Some(workspace_id.clone());
            snapshot.focused_pane_id = Some(pane_id.clone());
            if let Some(tab_id) = snapshot
                .panes
                .iter()
                .find(|pane| pane.pane_id == *pane_id)
                .map(|pane| pane.tab_id.clone())
            {
                snapshot.focused_tab_id = Some(tab_id);
            }
            for pane in &mut snapshot.panes {
                pane.focused = pane.pane_id == *pane_id;
            }
        }
        EventData::PaneMoved {
            previous_pane_id,
            pane,
            created_workspace,
            created_tab,
            closed_workspace_id,
            closed_tab_id,
            ..
        } => {
            snapshot
                .panes
                .retain(|existing| existing.pane_id != *previous_pane_id);
            upsert_by(&mut snapshot.panes, (**pane).clone(), |left, right| {
                left.pane_id == right.pane_id
            });
            if let Some(workspace) = created_workspace {
                upsert_by(
                    &mut snapshot.workspaces,
                    workspace.clone(),
                    |left, right| left.workspace_id == right.workspace_id,
                );
            }
            if let Some(tab) = created_tab {
                upsert_by(&mut snapshot.tabs, tab.clone(), |left, right| {
                    left.tab_id == right.tab_id
                });
            }
            if let Some(workspace_id) = closed_workspace_id {
                snapshot
                    .workspaces
                    .retain(|workspace| workspace.workspace_id != *workspace_id);
            }
            if let Some(tab_id) = closed_tab_id {
                snapshot.tabs.retain(|tab| tab.tab_id != *tab_id);
            }
        }
        EventData::PaneOutputChanged {
            pane_id, revision, ..
        } => {
            if let Some(pane) = snapshot
                .panes
                .iter_mut()
                .find(|pane| pane.pane_id == *pane_id)
            {
                pane.revision = *revision;
            }
        }
        EventData::PaneExited { pane_id, .. } => {
            if let Some(pane) = snapshot
                .panes
                .iter_mut()
                .find(|pane| pane.pane_id == *pane_id)
            {
                pane.agent_status = crate::api::schema::AgentStatus::Unknown;
            }
        }
        EventData::PaneAgentDetected { pane_id, agent, .. } => {
            if let Some(pane) = snapshot
                .panes
                .iter_mut()
                .find(|pane| pane.pane_id == *pane_id)
            {
                pane.agent.clone_from(agent);
            }
        }
        EventData::PaneAgentStatusChanged {
            pane_id,
            agent_status,
            agent,
            title,
            display_agent,
            state_labels,
            ..
        } => {
            if let Some(pane) = snapshot
                .panes
                .iter_mut()
                .find(|pane| pane.pane_id == *pane_id)
            {
                pane.agent_status = *agent_status;
                pane.agent.clone_from(agent);
                pane.title.clone_from(title);
                pane.display_agent.clone_from(display_agent);
                pane.state_labels.clone_from(state_labels);
            }
        }
        EventData::LayoutUpdated { layout } => {
            upsert_by(&mut snapshot.layouts, layout.clone(), |left, right| {
                left.tab_id == right.tab_id
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_bridge_command_contains_only_validated_session_data() {
        let command = remote_bridge_command("work-1");
        assert!(command.contains("herdr --session work-1 federation bridge"));
        assert!(command.contains("$HOME/.local/bin/herdr"));
    }

    #[test]
    fn ssh_bridge_disables_connection_multiplexing() {
        let endpoint = FederationEndpointConfig {
            id: "tana".into(),
            target: "tana.tail.example".into(),
            ..FederationEndpointConfig::default()
        };
        let args = ssh_bridge_command(&endpoint)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-o", "ControlMaster=no"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-o", "ControlPath=none"]));
    }

    #[test]
    fn endpoint_validation_rejects_option_like_ssh_targets() {
        let endpoint = FederationEndpointConfig {
            id: "bad".into(),
            target: "-ProxyCommand=oops".into(),
            ..FederationEndpointConfig::default()
        };
        assert_eq!(
            validate_endpoint(&endpoint).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn endpoint_snapshot_must_report_the_configured_member() {
        let endpoint = FederationEndpointConfig {
            id: "stl-agents-1".into(),
            ..FederationEndpointConfig::default()
        };
        let snapshot = SessionSnapshot {
            identity: crate::api::schema::RuntimeIdentity {
                member_id: "local".into(),
                ..crate::api::schema::RuntimeIdentity::default()
            },
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
        let error = validate_snapshot_member(&endpoint, &snapshot).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error
            .to_string()
            .contains("reported federation member local"));
    }
}
