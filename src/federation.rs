//! Personal Herdr federation over SSH-backed JSON API streams.
//!
//! Every endpoint remains authoritative for its own PTYs and session state.
//! This module only transports qualified references, snapshots, and events.

#[cfg(windows)]
use std::io::Write as _;
use std::io::{self, BufReader, Read as _};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
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
    run_endpoint_watch_controlled(endpoint, &mut publish, None);
}

fn run_endpoint_watch_controlled(
    endpoint: FederationEndpointConfig,
    publish: &mut impl FnMut(EndpointState) -> bool,
    control: Option<&Arc<EndpointWatchControl>>,
) {
    if !endpoint.enabled {
        let _ = publish(EndpointState::configured(endpoint));
        return;
    }
    let mut cursor = None;
    let mut snapshot = None;
    let mut backoff = Duration::from_secs(1);
    loop {
        if control.is_some_and(|control| control.is_stopped()) {
            return;
        }
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
        match EndpointWatch::connect_controlled(endpoint.clone(), cursor, resume_identity, control)
        {
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
                            if control.is_some_and(|control| control.is_stopped()) {
                                return;
                            }
                            cursor = watch.state.cursor;
                            snapshot = watch.state.snapshot.clone();
                            if !publish(watch.state.clone()) {
                                return;
                            }
                        }
                        Ok(None) => {
                            if control.is_some_and(|control| control.is_stopped()) {
                                return;
                            }
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
                            if control.is_some_and(|control| control.is_stopped()) {
                                return;
                            }
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
                if control.is_some_and(|control| control.is_stopped()) {
                    return;
                }
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
        if let Some(control) = control {
            if !control.wait_while_running(backoff) {
                return;
            }
        } else {
            std::thread::sleep(backoff);
        }
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
) -> EndpointWatchController {
    let control = Arc::new(EndpointWatchControl::default());
    let mut controller = EndpointWatchController {
        control: control.clone(),
        workers: Vec::new(),
    };
    if !config.enabled {
        return controller;
    }
    for endpoint in config.endpoints.iter().filter(|endpoint| endpoint.enabled) {
        let endpoint = endpoint.clone();
        let event_tx = event_tx.clone();
        let generation = generation.clone();
        let worker_control = control.clone();
        controller.workers.push(std::thread::spawn(move || {
            run_endpoint_watch_controlled(
                endpoint,
                &mut |state| {
                    if generation.load(std::sync::atomic::Ordering::Acquire) != expected_generation
                    {
                        return false;
                    }
                    let mut event = crate::events::AppEvent::FederationUpdated(Box::new(state));
                    loop {
                        match event_tx.try_send(event) {
                            Ok(()) => return true,
                            Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                                event = returned;
                                if !worker_control.wait_while_running(Duration::from_millis(10)) {
                                    return false;
                                }
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return false,
                        }
                    }
                },
                Some(&worker_control),
            );
        }));
    }
    controller
}

/// Owns the endpoint-watch workers for one application configuration.
///
/// Shutdown interrupts each SSH transport before joining its worker so a
/// headless-server handoff cannot leave the old transport reparented.
pub struct EndpointWatchController {
    control: Arc<EndpointWatchControl>,
    workers: Vec<JoinHandle<()>>,
}

impl EndpointWatchController {
    pub fn shutdown(&mut self) {
        self.control.stop();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for EndpointWatchController {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Default)]
struct EndpointWatchControl {
    stopped: AtomicBool,
    children: Mutex<Vec<Weak<WatchChild>>>,
    backoff: Mutex<()>,
    wake: Condvar,
}

impl EndpointWatchControl {
    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    fn register(&self, child: &Arc<WatchChild>) -> io::Result<()> {
        let mut children = self.children.lock().unwrap_or_else(|err| err.into_inner());
        children.retain(|child| child.strong_count() > 0);
        if self.is_stopped() {
            drop(children);
            child.terminate_and_reap();
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "federation endpoint watch is shutting down",
            ));
        }
        children.push(Arc::downgrade(child));
        Ok(())
    }

    fn wait_while_running(&self, duration: Duration) -> bool {
        if self.is_stopped() {
            return false;
        }
        let backoff = self.backoff.lock().unwrap_or_else(|err| err.into_inner());
        let (_backoff, _) = self
            .wake
            .wait_timeout_while(backoff, duration, |_| !self.is_stopped())
            .unwrap_or_else(|err| err.into_inner());
        !self.is_stopped()
    }

    fn stop(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        self.wake.notify_all();
        let children = {
            let mut registered = self.children.lock().unwrap_or_else(|err| err.into_inner());
            registered
                .drain(..)
                .filter_map(|child| child.upgrade())
                .collect::<Vec<_>>()
        };
        for child in children {
            child.terminate_and_reap();
        }
    }
}

/// A live `session.watch` stream transported through one SSH process.
pub struct EndpointWatch {
    child: Arc<WatchChild>,
    // Keep the request side of the SSH transport open for the watch lifetime.
    // Dropping it after the one-shot session.watch request makes the remote
    // bridge observe EOF even though the watch is still healthy.
    _stdin: std::process::ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
    pub state: EndpointState,
}

struct WatchChild {
    child: Mutex<Child>,
}

impl WatchChild {
    fn new(child: Child) -> Self {
        Self {
            child: Mutex::new(child),
        }
    }

    fn terminate_and_reap(&self) {
        let mut child = self.child.lock().unwrap_or_else(|err| err.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for WatchChild {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|err| err.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

struct ReapedChild(Option<Child>);

impl ReapedChild {
    fn new(child: Child) -> Self {
        Self(Some(child))
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
        Self::connect_controlled(endpoint, after_cursor, resume_identity, None)
    }

    fn connect_controlled(
        endpoint: FederationEndpointConfig,
        after_cursor: Option<u64>,
        resume_identity: Option<&crate::api::schema::RuntimeIdentity>,
        control: Option<&Arc<EndpointWatchControl>>,
    ) -> io::Result<Self> {
        validate_endpoint(&endpoint)?;
        let child = Arc::new(WatchChild::new(
            ssh_bridge_command(&endpoint)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh: {err}")))?,
        ));
        if let Some(control) = control {
            control.register(&child)?;
        }

        let request = Request {
            id: format!("federation:{}:watch", endpoint.id),
            method: Method::SessionWatch(SessionWatchParams {
                after_cursor,
                member_id: resume_identity.map(|identity| identity.member_id.clone()),
                server_id: resume_identity.map(|identity| identity.server_id.clone()),
                session_id: resume_identity.map(|identity| identity.session_id.clone()),
            }),
        };
        let (mut stdin, stdout) = {
            let mut process = child.child.lock().unwrap_or_else(|err| err.into_inner());
            let stdin = process.stdin.take().ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdin is unavailable")
            })?;
            let stdout = process.stdout.take().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "ssh bridge stdout is unavailable",
                )
            })?;
            (stdin, stdout)
        };
        write_json_line(&mut stdin, &request)?;
        let reader = BufReader::new(stdout);
        let (reader, ack) =
            read_watch_success_response_with_timeout(&child, reader).map_err(|err| {
                endpoint_watch_stream_error(&child, &endpoint, "watch handshake failed", err)
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
            child,
            _stdin: stdin,
            reader,
            state,
        })
    }

    pub fn next(&mut self) -> io::Result<Option<SequencedEventEnvelope>> {
        let event: Option<SequencedEventEnvelope> = read_optional_json_line(&mut self.reader)?;
        let Some(event) = event else {
            let message = watch_child_stderr(&self.child);
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
        self.child.terminate_and_reap();
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
    let stdin = child.stdin.take().ok_or_else(|| {
        io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdin is unavailable")
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "ssh bridge stdout is unavailable",
        )
    })?;
    let reader = BufReader::new(stdout);
    hold_request_input_until_response(stdin, request, || {
        read_success_response_with_timeout(&mut child, reader).map(|(_, response)| response)
    })
    .map_err(|err| endpoint_stream_error(&mut child, endpoint, "request failed", err))
}

fn hold_request_input_until_response<T>(
    mut input: impl io::Write,
    request: &Request,
    read_response: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    write_json_line(&mut input, request)?;
    // A federation bridge treats input EOF as transport loss and shuts down
    // both halves of its local socket. Keep this handle alive until the reply
    // arrives so a successful mutating request cannot look like a failure.
    let response = read_response();
    drop(input);
    response
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
    let mut stdout = io::stdout().lock();
    copy_bridge_stream(stream, || io::stdin().lock(), &mut stdout)
}

#[cfg(unix)]
fn copy_bridge_stream<R: io::Read>(
    stream: crate::ipc::LocalStream,
    input: impl FnOnce() -> R + Send + 'static,
    output: &mut impl io::Write,
) -> io::Result<()> {
    use interprocess::TryClone as _;

    let mut upload_stream = stream.try_clone()?;
    let shutdown_stream = stream.try_clone()?;
    let mut download_stream = stream;
    let upload = std::thread::spawn(move || {
        let mut input = input();
        copy_bridge_input(&mut input, &mut upload_stream, &shutdown_stream)
    });
    let download_result = io::copy(&mut download_stream, output);
    // Server EOF is the normal live-handoff boundary. The upload thread can
    // still be blocked reading SSH stdin, so close the socket to wake any
    // pending write and only join when the uploader has already completed.
    let _ = shutdown_local_stream(&download_stream);
    download_result?;
    output.flush()?;
    if upload.is_finished() {
        upload
            .join()
            .map_err(|_| io::Error::other("federation bridge input thread panicked"))?
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn copy_bridge_input(
    input: &mut impl io::Read,
    upload_stream: &mut crate::ipc::LocalStream,
    shutdown_stream: &crate::ipc::LocalStream,
) -> io::Result<()> {
    let result = io::copy(input, upload_stream).map(|_| ());
    // EOF means the SSH transport disappeared. Closing only the upload clone
    // leaves the download clone blocked on a quiet session.watch stream and
    // turns the remote bridge into a PPID-1 orphan. Shut down the underlying
    // Unix socket in both directions so the download copy wakes immediately.
    let _ = shutdown_local_stream(shutdown_stream);
    result
}

#[cfg(unix)]
fn shutdown_local_stream(stream: &crate::ipc::LocalStream) -> io::Result<()> {
    match stream {
        crate::ipc::LocalStream::UdSocket(stream) => {
            stream.inner().shutdown(std::net::Shutdown::Both)
        }
    }
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

fn read_watch_success_response_with_timeout(
    child: &WatchChild,
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
            child.terminate_and_reap();
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

fn endpoint_watch_stream_error(
    child: &WatchChild,
    endpoint: &FederationEndpointConfig,
    context: &str,
    source: io::Error,
) -> io::Error {
    child.terminate_and_reap();
    let stderr = watch_child_stderr(child);
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

fn watch_child_stderr(child: &WatchChild) -> String {
    let stderr = {
        let mut child = child.child.lock().unwrap_or_else(|err| err.into_inner());
        child.stderr.take()
    };
    let Some(mut stderr) = stderr else {
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

fn snapshot_agent_status_priority(status: crate::api::schema::AgentStatus) -> u8 {
    use crate::api::schema::AgentStatus;

    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

fn agent_info_from_pane(pane: &crate::api::schema::PaneInfo) -> crate::api::schema::AgentInfo {
    crate::api::schema::AgentInfo {
        terminal_id: pane.terminal_id.clone(),
        name: None,
        agent: pane.agent.clone(),
        title: pane.title.clone(),
        terminal_title: pane.terminal_title.clone(),
        terminal_title_stripped: pane.terminal_title_stripped.clone(),
        display_agent: pane.display_agent.clone(),
        agent_status: pane.agent_status,
        screen_detection_skipped: false,
        state_labels: pane.state_labels.clone(),
        tokens: pane.tokens.clone(),
        agent_session: pane.agent_session.clone(),
        workspace_id: pane.workspace_id.clone(),
        tab_id: pane.tab_id.clone(),
        pane_id: pane.pane_id.clone(),
        focused: pane.focused,
        cwd: pane.cwd.clone(),
        foreground_cwd: pane.foreground_cwd.clone(),
        revision: pane.revision,
    }
}

fn sync_agent_info_from_pane(
    agent: &mut crate::api::schema::AgentInfo,
    pane: &crate::api::schema::PaneInfo,
) {
    // Preserve agent-only metadata (`name` and detection authority), but source
    // every live presentation/status field from the pane event stream.
    let name = agent.name.clone();
    let screen_detection_skipped = agent.screen_detection_skipped;
    *agent = agent_info_from_pane(pane);
    agent.name = name;
    agent.screen_detection_skipped = screen_detection_skipped;
}

fn reconcile_snapshot_agents(snapshot: &mut SessionSnapshot) {
    snapshot.agents.retain(|agent| {
        snapshot
            .panes
            .iter()
            .any(|pane| pane.pane_id == agent.pane_id)
    });

    for pane in &snapshot.panes {
        if let Some(agent) = snapshot
            .agents
            .iter_mut()
            .find(|agent| agent.pane_id == pane.pane_id)
        {
            sync_agent_info_from_pane(agent, pane);
        } else if pane.agent.is_some()
            || pane.display_agent.is_some()
            || pane.agent_session.is_some()
        {
            snapshot.agents.push(agent_info_from_pane(pane));
        }
    }

    for tab in &mut snapshot.tabs {
        tab.agent_status = snapshot
            .panes
            .iter()
            .filter(|pane| pane.tab_id == tab.tab_id)
            .map(|pane| pane.agent_status)
            .max_by_key(|status| snapshot_agent_status_priority(*status))
            .unwrap_or(crate::api::schema::AgentStatus::Unknown);
    }
    for workspace in &mut snapshot.workspaces {
        workspace.agent_status = snapshot
            .panes
            .iter()
            .filter(|pane| pane.workspace_id == workspace.workspace_id)
            .map(|pane| pane.agent_status)
            .max_by_key(|status| snapshot_agent_status_priority(*status))
            .unwrap_or(crate::api::schema::AgentStatus::Unknown);
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
    // Session-watch deltas carry pane-level lifecycle/status changes. Keep the
    // cached agent list and workspace/tab aggregates derived from those panes
    // so an inactive federation member remains as live as the active one.
    reconcile_snapshot_agents(snapshot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_shot_request_keeps_bridge_input_open_until_response() {
        struct TrackedWriter {
            bytes: Vec<u8>,
            dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }

        impl io::Write for TrackedWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl Drop for TrackedWriter {
            fn drop(&mut self) {
                self.dropped
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let request = Request {
            id: "request-lifetime".into(),
            method: crate::api::schema::Method::Ping(crate::api::schema::PingParams {}),
        };
        let result = hold_request_input_until_response(
            TrackedWriter {
                bytes: Vec::new(),
                dropped: dropped.clone(),
            },
            &request,
            || {
                assert!(!dropped.load(std::sync::atomic::Ordering::SeqCst));
                Ok("response")
            },
        )
        .unwrap();

        assert_eq!(result, "response");
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_watch_controller_drop_terminates_and_reaps_registered_child() {
        let control = Arc::new(EndpointWatchControl::default());
        let controller = EndpointWatchController {
            control: control.clone(),
            workers: Vec::new(),
        };
        let child = Arc::new(WatchChild::new(
            Command::new("sh")
                .args(["-c", "exec sleep 30"])
                .spawn()
                .expect("spawn sleeping endpoint transport"),
        ));
        control.register(&child).unwrap();
        assert!(child
            .child
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .try_wait()
            .unwrap()
            .is_none());

        drop(controller);

        assert!(child
            .child
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .try_wait()
            .unwrap()
            .is_some());
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_watch_shutdown_is_not_blocked_by_open_stderr_after_stdout_eof() {
        let control = Arc::new(EndpointWatchControl::default());
        let controller = EndpointWatchController {
            control: control.clone(),
            workers: Vec::new(),
        };
        let child = Arc::new(WatchChild::new(
            Command::new("sh")
                .args(["-c", "exec 1>&-; read _"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn endpoint transport with stdout EOF"),
        ));
        control.register(&child).unwrap();
        let (stdin, stdout) = {
            let mut process = child.child.lock().unwrap_or_else(|err| err.into_inner());
            (
                process.stdin.take().expect("child stdin"),
                process.stdout.take().expect("child stdout"),
            )
        };
        let mut watch = EndpointWatch {
            child: child.clone(),
            _stdin: stdin,
            reader: BufReader::new(stdout),
            state: EndpointState::configured(FederationEndpointConfig::default()),
        };
        let (watch_done_tx, watch_done_rx) = std::sync::mpsc::channel();
        let watch_thread = std::thread::spawn(move || {
            let result = watch.next();
            let _ = watch_done_tx.send(result);
        });

        // Wait until next() has entered its stderr diagnostic path. The fixed
        // path has already detached stderr and released the child mutex; the
        // old path is identifiable because it still holds that mutex.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            match child.child.try_lock() {
                Ok(process) if process.stderr.is_none() => break,
                Ok(_) => {}
                Err(std::sync::TryLockError::WouldBlock) => break,
                Err(std::sync::TryLockError::Poisoned(err)) => {
                    panic!("endpoint child mutex poisoned: {err}")
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "watch did not reach stderr diagnostic path"
            );
            std::thread::yield_now();
        }

        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();
        let shutdown_thread = std::thread::spawn(move || {
            drop(controller);
            let _ = shutdown_tx.send(());
        });
        shutdown_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("controller shutdown deadlocked behind stderr read");
        watch_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("watch did not wake after child termination")
            .unwrap();
        shutdown_thread.join().unwrap();
        watch_thread.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bridge_relay_exits_promptly_when_input_closes_and_server_stays_open() {
        use std::os::unix::net::UnixStream;
        use std::sync::mpsc;
        use std::time::Duration;

        let (bridge_socket, server_socket) = UnixStream::pair().unwrap();
        let bridge_socket = crate::ipc::LocalStream::UdSocket(
            interprocess::os::unix::uds_local_socket::Stream::from(bridge_socket),
        );
        let (finished_tx, finished_rx) = mpsc::channel();
        let relay = std::thread::spawn(move || {
            let mut output = io::sink();
            let result = copy_bridge_stream(bridge_socket, io::empty, &mut output);
            let _ = finished_tx.send(result);
        });

        // Keep the server peer alive and silent. Before the EOF cancellation,
        // the relay's download half waited forever for output from this peer.
        let result = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("bridge relay did not exit promptly after stdin EOF");
        result.unwrap();
        relay.join().unwrap();
        drop(server_socket);
    }

    #[cfg(unix)]
    #[test]
    fn bridge_relay_exits_promptly_when_server_closes_and_input_stays_open() {
        use std::os::unix::net::UnixStream;
        use std::sync::mpsc;
        use std::time::Duration;

        let (bridge_socket, server_socket) = UnixStream::pair().unwrap();
        let bridge_socket = crate::ipc::LocalStream::UdSocket(
            interprocess::os::unix::uds_local_socket::Stream::from(bridge_socket),
        );
        let (input_reader, input_writer) = UnixStream::pair().unwrap();
        let (finished_tx, finished_rx) = mpsc::channel();
        let relay = std::thread::spawn(move || {
            let mut output = io::sink();
            let result = copy_bridge_stream(bridge_socket, move || input_reader, &mut output);
            let _ = finished_tx.send(result);
        });

        // Keep input alive and silent while the server side reaches EOF. A
        // blocking upload join would keep the bridge process alive forever.
        drop(server_socket);
        let result = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("bridge relay did not exit promptly after server EOF");
        result.unwrap();
        drop(input_writer);
        relay.join().unwrap();
    }

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

    #[test]
    fn watched_pane_events_keep_inactive_agent_projection_live() {
        use crate::api::schema::{
            AgentStatus, EventEnvelope, EventKind, PaneInfo, TabInfo, WorkspaceInfo,
        };

        let pane = PaneInfo {
            pane_id: "w3:p1".into(),
            terminal_id: "terminal-1".into(),
            workspace_id: "w3".into(),
            tab_id: "w3:t1".into(),
            focused: false,
            cwd: Some("/srv/project".into()),
            foreground_cwd: None,
            label: None,
            agent: Some("codex".into()),
            title: Some("implementation".into()),
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: Some("Codex".into()),
            agent_status: AgentStatus::Idle,
            state_labels: Default::default(),
            tokens: Default::default(),
            agent_session: None,
            scroll: None,
            revision: 4,
        };
        let mut snapshot = SessionSnapshot {
            identity: crate::api::schema::RuntimeIdentity::default(),
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            event_cursor: 0,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            workspaces: vec![WorkspaceInfo {
                workspace_id: "w3".into(),
                number: 3,
                label: "project".into(),
                focused: false,
                pane_count: 1,
                tab_count: 1,
                active_tab_id: "w3:t1".into(),
                agent_status: AgentStatus::Idle,
                terminal_launcher_argv: None,
                tokens: Default::default(),
                worktree: None,
                project: None,
                branch: None,
                git_ahead_behind: None,
            }],
            tabs: vec![TabInfo {
                tab_id: "w3:t1".into(),
                workspace_id: "w3".into(),
                number: 1,
                label: "main".into(),
                focused: false,
                pane_count: 1,
                agent_status: AgentStatus::Idle,
            }],
            panes: vec![pane.clone()],
            layouts: Vec::new(),
            agents: vec![agent_info_from_pane(&pane)],
        };

        apply_event(
            &mut snapshot,
            &SequencedEventEnvelope {
                cursor: 1,
                event: EventEnvelope {
                    event: EventKind::PaneAgentStatusChanged,
                    data: EventData::PaneAgentStatusChanged {
                        pane_id: "w3:p1".into(),
                        workspace_id: "w3".into(),
                        agent_status: AgentStatus::Blocked,
                        agent: Some("codex".into()),
                        title: Some("needs input".into()),
                        display_agent: Some("Codex".into()),
                        state_labels: std::collections::HashMap::from([(
                            "summary".into(),
                            "Waiting for review".into(),
                        )]),
                    },
                },
            },
        );

        assert_eq!(snapshot.agents[0].agent_status, AgentStatus::Blocked);
        assert_eq!(snapshot.agents[0].title.as_deref(), Some("needs input"));
        assert_eq!(
            snapshot.agents[0]
                .state_labels
                .get("summary")
                .map(String::as_str),
            Some("Waiting for review")
        );
        assert_eq!(snapshot.tabs[0].agent_status, AgentStatus::Blocked);
        assert_eq!(snapshot.workspaces[0].agent_status, AgentStatus::Blocked);

        apply_event(
            &mut snapshot,
            &SequencedEventEnvelope {
                cursor: 2,
                event: EventEnvelope {
                    event: EventKind::PaneClosed,
                    data: EventData::PaneClosed {
                        pane_id: "w3:p1".into(),
                        workspace_id: "w3".into(),
                    },
                },
            },
        );
        assert!(snapshot.agents.is_empty());
        assert_eq!(snapshot.tabs[0].agent_status, AgentStatus::Unknown);
        assert_eq!(snapshot.workspaces[0].agent_status, AgentStatus::Unknown);
    }
}
