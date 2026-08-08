//! Process-level characterization of the single-client federation runtime.

#![cfg(unix)]

mod support;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid,
};

const REMOTE_SESSION: &str = "federated-e2e";
const REMOTE_QUALIFIED: &str = "federation-test@stl-agents-1";
const REMOTE_VISIBLE_PREFIX: &str = "federation-test@stl-a";
const REMOTE_MARKER: &str = "HERDR_FEDERATED_REMOTE_MARKER";
const HOME_MARKER: &str = "HERDR_FEDERATED_HOME_MARKER";
const HOME_AFTER_FAILED_SWITCH: &str = "HERDR_HOME_AFTER_FAILED_FEDERATION_SWITCH";
const HOME_AFTER_REMOTE_DISCONNECT: &str = "HERDR_HOME_AFTER_REMOTE_DISCONNECT";
const REMOTE_AFTER_LIVE_HANDOFF: &str = "HERDR_REMOTE_AFTER_LIVE_HANDOFF";
const MEMBER_B_MARKER: &str = "HERDR_FEDERATED_MEMBER_B_MARKER";
const MEMBER_B_AFTER_REVOCATION: &str = "HERDR_FEDERATED_MEMBER_B_AFTER_REVOCATION";
const MEMBER_C_MARKER: &str = "HERDR_FEDERATED_MEMBER_C_MARKER";

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    // Named-session sockets include the config root and session name and must
    // remain below Unix sockaddr_un's small path limit.
    PathBuf::from(format!("/tmp/hfc-{}-{nanos:x}", std::process::id()))
}

fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

struct SpawnedHerdr {
    master: Option<Box<dyn MasterPty + Send>>,
    output_rx: mpsc::Receiver<String>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        drop(self.master.take());

        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

struct SpawnedClient {
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedClient {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        drop(self.master.take());

        if let Some(pid) = pid {
            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

fn write_config(config_home: &Path, contents: &str) {
    let config_dir = config_home.join(app_dir_name());
    fs::create_dir_all(&config_dir).expect("create config directory");
    fs::write(config_dir.join("config.toml"), contents).expect("write test config");
}

fn spawn_server(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket: Option<&Path>,
    session: Option<&str>,
    path: &str,
) -> SpawnedHerdr {
    fs::create_dir_all(runtime_dir).expect("create runtime directory");
    register_runtime_dir(runtime_dir);

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open server PTY");
    let reader = pair
        .master
        .try_clone_reader()
        .expect("clone server PTY reader");
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    if let Some(session) = session {
        command.arg("--session");
        command.arg(session);
    }
    command.arg("server");
    command.env("XDG_CONFIG_HOME", config_home);
    command.env("XDG_RUNTIME_DIR", runtime_dir);
    command.env("PATH", path);
    command.env("SHELL", "/bin/sh");
    command.env_remove("HERDR_CONFIG_PATH");
    command.env_remove("HERDR_ENV");
    command.env_remove("HERDR_CLIENT_SOCKET_PATH");
    if let Some(api_socket) = api_socket {
        command.env("HERDR_SOCKET_PATH", api_socket);
    } else {
        command.env_remove("HERDR_SOCKET_PATH");
    }

    let child = pair.slave.spawn_command(command).expect("spawn server");
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);
    let (output_tx, output_rx) = mpsc::channel();
    thread::spawn(move || read_output(reader, output_tx));
    SpawnedHerdr {
        master: Some(pair.master),
        output_rx,
        child,
    }
}

fn wait_for_server_socket(server: &mut SpawnedHerdr, path: &Path, timeout: Duration) {
    use std::os::unix::net::UnixStream;

    let deadline = Instant::now() + timeout;
    let mut output = String::new();
    while Instant::now() < deadline {
        while let Ok(chunk) = server.output_rx.try_recv() {
            output.push_str(&chunk);
        }
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        if let Some(status) = server.child.try_wait().ok().flatten() {
            panic!(
                "server exited with {status} before socket appeared at {}; output: {output:?}",
                path.display()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "socket did not appear at {}; server output: {output:?}",
        path.display()
    );
}

fn spawn_client(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket: &Path,
    path: &str,
) -> (SpawnedClient, mpsc::Receiver<String>) {
    register_runtime_dir(runtime_dir);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open client PTY");
    let master = pair.master;
    let reader = master.try_clone_reader().expect("clone client PTY reader");
    let writer = master.take_writer().expect("take client PTY writer");

    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    command.arg("client");
    command.env("HERDR_DISABLE_SOUND", "1");
    command.env("XDG_CONFIG_HOME", config_home);
    command.env("XDG_RUNTIME_DIR", runtime_dir);
    command.env("HERDR_SOCKET_PATH", api_socket);
    command.env("PATH", path);
    command.env("SHELL", "/bin/sh");
    command.env_remove("HERDR_CONFIG_PATH");
    command.env_remove("HERDR_CLIENT_SOCKET_PATH");
    command.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(command).expect("spawn client");
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    let (output_tx, output_rx) = mpsc::channel();
    thread::spawn(move || read_output(reader, output_tx));

    (
        SpawnedClient {
            master: Some(master),
            writer,
            child,
        },
        output_rx,
    )
}

fn read_output(mut reader: Box<dyn Read + Send>, output_tx: mpsc::Sender<String>) {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if output_tx
                    .send(String::from_utf8_lossy(&buffer[..count]).into_owned())
                    .is_err()
                {
                    break;
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn strip_terminal_control_sequences(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut plain = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            plain.push(bytes[index]);
            index += 1;
            continue;
        }

        index += 1;
        match bytes.get(index).copied() {
            Some(b'[') => {
                index += 1;
                while index < bytes.len() {
                    let byte = bytes[index];
                    index += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            Some(b']') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            Some(_) => index += 1,
            None => {}
        }
    }
    String::from_utf8_lossy(&plain).into_owned()
}

fn wait_for_output(
    client: &mut SpawnedClient,
    output_rx: &mpsc::Receiver<String>,
    output: &mut String,
    needle: &str,
    timeout: Duration,
    diagnostic_logs: &[(&str, &Path)],
) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = client.child.try_wait().ok().flatten() {
            thread::sleep(Duration::from_millis(100));
            while let Ok(chunk) = output_rx.try_recv() {
                output.push_str(&chunk);
            }
            let plain = strip_terminal_control_sequences(output);
            let tail = plain
                .char_indices()
                .rev()
                .nth(1_500)
                .map(|(index, _)| &plain[index..])
                .unwrap_or(&plain);
            panic!(
                "client exited with {status} while waiting for {needle:?}; plain output tail: \
                 {tail:?}; process logs: {}",
                diagnostic_log_tails(diagnostic_logs)
            );
        }
        if strip_terminal_control_sequences(output).contains(needle) {
            return;
        }
        if let Ok(chunk) = output_rx.recv_timeout(Duration::from_millis(100)) {
            output.push_str(&chunk);
        }
    }
    panic!(
        "timed out waiting for {needle:?}; plain output: {:?}; process logs: {}",
        strip_terminal_control_sequences(output),
        diagnostic_log_tails(diagnostic_logs)
    );
}

fn diagnostic_log_tails(logs: &[(&str, &Path)]) -> String {
    logs.iter()
        .map(|(label, path)| {
            let contents = fs::read_to_string(path)
                .unwrap_or_else(|err| format!("<could not read {}: {err}>", path.display()));
            let tail = contents
                .char_indices()
                .rev()
                .nth(4_000)
                .map(|(index, _)| &contents[index..])
                .unwrap_or(&contents);
            format!("\n--- {label} ({}) ---\n{tail}", path.display())
        })
        .collect()
}

fn clear_output(output_rx: &mpsc::Receiver<String>, output: &mut String) {
    output.clear();
    while output_rx.try_recv().is_ok() {}
}

fn navigate_to(client: &mut SpawnedClient, query: &str) {
    client.writer.write_all(&[0x18]).expect("send C-x");
    client.writer.flush().expect("flush C-x");
    thread::sleep(Duration::from_millis(40));
    client
        .writer
        .write_all(b"b/")
        .expect("open navigator search");
    client
        .writer
        .write_all(query.as_bytes())
        .expect("type navigator query");
    client.writer.write_all(b"\r").expect("accept selection");
    client.writer.flush().expect("flush navigator selection");
}

fn send_json_request(socket_path: &Path, request: Value) -> Value {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket_path).expect("connect to JSON API socket");
    serde_json::to_writer(&mut stream, &request).expect("encode JSON API request");
    stream.write_all(b"\n").expect("terminate JSON API request");
    stream.flush().expect("flush JSON API request");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read JSON API response");
    let response: Value = serde_json::from_str(&response).expect("decode JSON API response");
    assert!(
        response.get("error").is_none(),
        "JSON API request failed: {response}"
    );
    response
}

fn create_workspace(socket_path: &Path, label: &str) -> (String, String) {
    let response = send_json_request(
        socket_path,
        serde_json::json!({
            "id": format!("create:{label}"),
            "method": "workspace.create",
            "params": { "label": label }
        }),
    );
    let workspace_id = response
        .pointer("/result/workspace/workspace_id")
        .and_then(Value::as_str)
        .expect("workspace id")
        .to_string();
    let pane_id = response
        .pointer("/result/root_pane/pane_id")
        .and_then(Value::as_str)
        .expect("root pane id")
        .to_string();
    (workspace_id, pane_id)
}

fn focus_workspace(socket_path: &Path, workspace_id: &str) {
    send_json_request(
        socket_path,
        serde_json::json!({
            "id": format!("focus:{workspace_id}"),
            "method": "workspace.focus",
            "params": { "workspace_id": workspace_id }
        }),
    );
}

fn write_pane_marker(socket_path: &Path, pane_id: &str, marker: &str) {
    send_json_request(
        socket_path,
        serde_json::json!({
            "id": format!("marker:{pane_id}"),
            "method": "pane.send_input",
            "params": {
                "pane_id": pane_id,
                "text": format!("printf '{marker}\\n'"),
                "keys": ["Enter"]
            }
        }),
    );
}

fn bridge_launch_count(log_path: &Path) -> usize {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("remote-client-bridge"))
        .count()
}

fn bridge_launch_count_for_target(log_path: &Path, target: &str) -> usize {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("remote-client-bridge"))
        .filter(|line| line.split('\t').nth(1) == Some(target))
        .count()
}

fn wait_for_bridge_launches(log_path: &Path, expected: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if bridge_launch_count(log_path) >= expected {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "timed out waiting for {expected} remote bridge launches; log: {}",
        fs::read_to_string(log_path).unwrap_or_default()
    );
}

fn bridge_parent_pids(log_path: &Path) -> Vec<u32> {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("remote-client-bridge"))
        .filter_map(|line| line.split_once('\t')?.0.parse().ok())
        .collect()
}

fn write_fake_ssh(
    fake_bin: &Path,
    remote_config_home: &Path,
    remote_runtime_dir: &Path,
    ssh_log: &Path,
    fail_bridge: &Path,
    binary_dir: &Path,
) {
    fs::create_dir_all(fake_bin).expect("create fake SSH bin directory");
    let script = format!(
        "#!/bin/sh\nset -eu\nlast=\nfor arg do last=$arg; done\n\
         export XDG_CONFIG_HOME='{config}'\n\
         export XDG_RUNTIME_DIR='{runtime}'\n\
         unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_CONFIG_PATH HERDR_ENV\n\
         export PATH='{binary_dir}:/usr/bin:/bin'\n\
         printf '%s\\t%s\\n' \"$PPID\" \"$last\" >> '{log}'\n\
         case \"$last\" in\n\
           *remote-client-bridge*)\n\
             if [ -e '{fail_bridge}' ]; then exit 91; fi\n\
             ;;\n\
         esac\n\
         if [ \"$last\" = '/bin/sh -s' ]; then\n\
           exec /bin/sh -s\n\
         fi\n\
         exec /bin/sh -c \"$last\"\n",
        config = remote_config_home.display(),
        runtime = remote_runtime_dir.display(),
        binary_dir = binary_dir.display(),
        log = ssh_log.display(),
        fail_bridge = fail_bridge.display(),
    );
    let ssh_path = fake_bin.join("ssh");
    fs::write(&ssh_path, script).expect("write fake SSH executable");
    let mut permissions = fs::metadata(&ssh_path)
        .expect("fake SSH metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&ssh_path, permissions).expect("make fake SSH executable");
}

#[allow(clippy::too_many_arguments)]
fn write_two_member_fake_ssh(
    fake_bin: &Path,
    member_b_config_home: &Path,
    member_b_runtime_dir: &Path,
    member_c_config_home: &Path,
    member_c_runtime_dir: &Path,
    ssh_log: &Path,
    binary_dir: &Path,
) {
    fs::create_dir_all(fake_bin).expect("create fake SSH bin directory");
    let script = format!(
        "#!/bin/sh\nset -eu\ntarget=\nlast=\nfor arg do\n\
           last=$arg\n\
           case \"$arg\" in\n\
             fake-host-b|fake-host-c) target=$arg ;;\n\
           esac\n\
         done\n\
         case \"$target\" in\n\
           fake-host-b)\n\
             export XDG_CONFIG_HOME='{member_b_config}'\n\
             export XDG_RUNTIME_DIR='{member_b_runtime}'\n\
             ;;\n\
           fake-host-c)\n\
             export XDG_CONFIG_HOME='{member_c_config}'\n\
             export XDG_RUNTIME_DIR='{member_c_runtime}'\n\
             ;;\n\
           *) exit 92 ;;\n\
         esac\n\
         unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_CONFIG_PATH HERDR_ENV\n\
         export PATH='{binary_dir}:/usr/bin:/bin'\n\
         printf '%s\\t%s\\t%s\\n' \"$PPID\" \"$target\" \"$last\" >> '{log}'\n\
         if [ \"$last\" = '/bin/sh -s' ]; then\n\
           exec /bin/sh -s\n\
         fi\n\
         exec /bin/sh -c \"$last\"\n",
        member_b_config = member_b_config_home.display(),
        member_b_runtime = member_b_runtime_dir.display(),
        member_c_config = member_c_config_home.display(),
        member_c_runtime = member_c_runtime_dir.display(),
        binary_dir = binary_dir.display(),
        log = ssh_log.display(),
    );
    let ssh_path = fake_bin.join("ssh");
    fs::write(&ssh_path, script).expect("write two-member fake SSH executable");
    let mut permissions = fs::metadata(&ssh_path)
        .expect("two-member fake SSH metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&ssh_path, permissions).expect("make two-member fake SSH executable");
}

#[test]
fn one_client_switches_between_federated_servers_and_recovers_in_process() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let home_base = base.join("home");
    let remote_base = base.join("remote");
    let home_config = home_base.join("config");
    let remote_config = remote_base.join("config");
    let home_runtime = home_base.join("runtime");
    let remote_runtime = remote_base.join("runtime");
    let home_api = home_runtime.join("herdr.sock");
    let remote_api = remote_config
        .join(app_dir_name())
        .join("sessions")
        .join(REMOTE_SESSION)
        .join("herdr.sock");
    let remote_client_socket = remote_api.with_file_name("herdr-client.sock");
    let fake_bin = base.join("fake-bin");
    let ssh_log = base.join("ssh.log");
    let fail_bridge = base.join("fail-remote-client-bridge");
    let binary_dir = Path::new(env!("CARGO_BIN_EXE_herdr"))
        .parent()
        .expect("test binary directory");
    write_fake_ssh(
        &fake_bin,
        &remote_config,
        &remote_runtime,
        &ssh_log,
        &fail_bridge,
        binary_dir,
    );
    let path = format!("{}:/usr/bin:/bin", fake_bin.display());

    write_config(
        &remote_config,
        r#"onboarding = false
[emacs]
enabled = true
[remote]
manage_ssh_config = false
[federation]
enabled = true
member_id = "stl-agents-1"
member_target = "fake-host"
"#,
    );
    let mut remote_server = spawn_server(
        &remote_config,
        &remote_runtime,
        None,
        Some(REMOTE_SESSION),
        &path,
    );
    wait_for_server_socket(&mut remote_server, &remote_api, Duration::from_secs(10));
    wait_for_server_socket(
        &mut remote_server,
        &remote_client_socket,
        Duration::from_secs(10),
    );
    let (remote_workspace, remote_pane) = create_workspace(&remote_api, "federation-test");
    write_pane_marker(&remote_api, &remote_pane, REMOTE_MARKER);
    let _ = create_workspace(&remote_api, "remote-decoy");

    write_config(
        &home_config,
        &format!(
            r#"onboarding = false
[emacs]
enabled = true
[remote]
manage_ssh_config = false
[federation]
enabled = true
member_id = "x1"
member_target = "fake-home"

[[federation.endpoints]]
id = "stl-agents-1"
target = "fake-host"
session = "{REMOTE_SESSION}"
enabled = true
"#
        ),
    );
    let mut home_server = spawn_server(&home_config, &home_runtime, Some(&home_api), None, &path);
    let home_client_socket = home_runtime.join("herdr-client.sock");
    wait_for_server_socket(&mut home_server, &home_api, Duration::from_secs(10));
    wait_for_server_socket(
        &mut home_server,
        &home_client_socket,
        Duration::from_secs(10),
    );
    let (home_workspace, home_pane) = create_workspace(&home_api, "home-test");
    write_pane_marker(&home_api, &home_pane, HOME_MARKER);
    let _ = create_workspace(&home_api, "home-decoy");
    focus_workspace(&home_api, &home_workspace);

    let (mut client, output_rx) = spawn_client(&home_config, &home_runtime, &home_api, &path);
    let client_pid = client.child.process_id().expect("client PID");
    let mut output = String::new();
    let home_client_log = home_config.join(app_dir_name()).join("herdr-client.log");
    let home_server_log = home_config.join(app_dir_name()).join("herdr-server.log");
    let remote_server_log = remote_api
        .parent()
        .expect("named remote session directory")
        .join("herdr-server.log");
    let diagnostic_logs = [
        ("home client", home_client_log.as_path()),
        ("home server", home_server_log.as_path()),
        ("remote server", remote_server_log.as_path()),
    ];
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        REMOTE_VISIBLE_PREFIX,
        Duration::from_secs(15),
        &diagnostic_logs,
    );

    fs::write(&fail_bridge, b"fail the first activation").expect("arm bridge failure");
    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, REMOTE_QUALIFIED);
    wait_for_bridge_launches(&ssh_log, 1, Duration::from_secs(15));
    write_pane_marker(&home_api, &home_pane, HOME_AFTER_FAILED_SWITCH);
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        HOME_AFTER_FAILED_SWITCH,
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    assert_eq!(client.child.process_id(), Some(client_pid));
    assert_eq!(bridge_launch_count(&ssh_log), 1);
    fs::remove_file(&fail_bridge).expect("disarm bridge failure");

    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, REMOTE_QUALIFIED);
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        REMOTE_MARKER,
        Duration::from_secs(20),
        &diagnostic_logs,
    );
    assert_eq!(client.child.process_id(), Some(client_pid));
    assert!(
        bridge_parent_pids(&ssh_log)
            .iter()
            .all(|parent| *parent == client_pid),
        "the TUI must own SSH bridge processes directly, without a nested `herdr --remote` client"
    );
    assert_eq!(bridge_launch_count(&ssh_log), 2);
    assert!(
        !fs::read_to_string(&ssh_log)
            .unwrap_or_default()
            .contains("herdr --remote"),
        "in-process federation must not relaunch a nested `herdr --remote` client"
    );

    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, "home-test");
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        HOME_MARKER,
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    assert_eq!(client.child.process_id(), Some(client_pid));

    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, REMOTE_QUALIFIED);
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        REMOTE_MARKER,
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    assert_eq!(
        bridge_launch_count(&ssh_log),
        2,
        "reselecting a suspended member must reuse its existing connection"
    );

    // A live handoff on the active remote member must recycle only that
    // member's SSH transport. The originating TUI stays alive and remains on
    // the remote workspace instead of falling back to its retained home.
    clear_output(&output_rx, &mut output);
    send_json_request(
        &remote_api,
        serde_json::json!({
            "id": "handoff:active-remote",
            "method": "server.live_handoff",
            "params": {}
        }),
    );
    wait_for_bridge_launches(&ssh_log, 3, Duration::from_secs(15));
    write_pane_marker(&remote_api, &remote_pane, REMOTE_AFTER_LIVE_HANDOFF);
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        REMOTE_AFTER_LIVE_HANDOFF,
        Duration::from_secs(20),
        &diagnostic_logs,
    );
    assert_eq!(client.child.process_id(), Some(client_pid));
    assert!(client.child.try_wait().ok().flatten().is_none());
    assert_eq!(
        bridge_launch_count(&ssh_log),
        3,
        "active remote live handoff must launch exactly one replacement bridge"
    );

    clear_output(&output_rx, &mut output);
    send_json_request(
        &remote_api,
        serde_json::json!({
            "id": "stop:remote",
            "method": "server.stop",
            "params": {}
        }),
    );
    write_pane_marker(&home_api, &home_pane, HOME_AFTER_REMOTE_DISCONNECT);
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        HOME_AFTER_REMOTE_DISCONNECT,
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    assert_eq!(client.child.process_id(), Some(client_pid));
    assert!(client.child.try_wait().ok().flatten().is_none());

    let _ = remote_workspace;
    drop(client);
    drop(home_server);
    drop(remote_server.master.take());
    let _ = remote_server.child.wait();
    drop(remote_server);
    cleanup_test_base(&home_base);
    cleanup_test_base(&remote_base);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn one_client_traverses_three_members_and_obeys_home_directory_revocation() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let home_base = base.join("home");
    let member_b_base = base.join("member-b");
    let member_c_base = base.join("member-c");
    let home_config = home_base.join("config");
    let member_b_config = member_b_base.join("config");
    let member_c_config = member_c_base.join("config");
    let home_runtime = home_base.join("runtime");
    let member_b_runtime = member_b_base.join("runtime");
    let member_c_runtime = member_c_base.join("runtime");
    let home_api = home_runtime.join("herdr.sock");
    let member_b_api = member_b_config
        .join(app_dir_name())
        .join("sessions")
        .join(REMOTE_SESSION)
        .join("herdr.sock");
    let member_c_api = member_c_config
        .join(app_dir_name())
        .join("sessions")
        .join(REMOTE_SESSION)
        .join("herdr.sock");
    let fake_bin = base.join("fake-bin");
    let ssh_log = base.join("ssh.log");
    let binary_dir = Path::new(env!("CARGO_BIN_EXE_herdr"))
        .parent()
        .expect("test binary directory");
    write_two_member_fake_ssh(
        &fake_bin,
        &member_b_config,
        &member_b_runtime,
        &member_c_config,
        &member_c_runtime,
        &ssh_log,
        binary_dir,
    );
    let path = format!("{}:/usr/bin:/bin", fake_bin.display());

    write_config(
        &member_b_config,
        r#"onboarding = false
[emacs]
enabled = true
[remote]
manage_ssh_config = false
[federation]
enabled = true
member_id = "member-b"
member_target = "fake-host-b"
"#,
    );
    write_config(
        &member_c_config,
        r#"onboarding = false
[emacs]
enabled = true
[remote]
manage_ssh_config = false
[federation]
enabled = true
member_id = "member-c"
member_target = "fake-host-c"
"#,
    );
    let mut member_b_server = spawn_server(
        &member_b_config,
        &member_b_runtime,
        None,
        Some(REMOTE_SESSION),
        &path,
    );
    let mut member_c_server = spawn_server(
        &member_c_config,
        &member_c_runtime,
        None,
        Some(REMOTE_SESSION),
        &path,
    );
    wait_for_server_socket(&mut member_b_server, &member_b_api, Duration::from_secs(10));
    wait_for_server_socket(&mut member_c_server, &member_c_api, Duration::from_secs(10));
    let (_, member_b_pane) = create_workspace(&member_b_api, "workspace-b");
    let (_, member_c_pane) = create_workspace(&member_c_api, "workspace-c");
    write_pane_marker(&member_b_api, &member_b_pane, MEMBER_B_MARKER);
    write_pane_marker(&member_c_api, &member_c_pane, MEMBER_C_MARKER);

    let home_config_with_members_b_and_c = format!(
        r#"onboarding = false
[emacs]
enabled = true
[remote]
manage_ssh_config = false
[federation]
enabled = true
member_id = "member-a"
member_target = "fake-home"

[[federation.endpoints]]
id = "member-b"
target = "fake-host-b"
session = "{REMOTE_SESSION}"
enabled = true

[[federation.endpoints]]
id = "member-c"
target = "fake-host-c"
session = "{REMOTE_SESSION}"
enabled = true
"#
    );
    write_config(&home_config, &home_config_with_members_b_and_c);
    let mut home_server = spawn_server(&home_config, &home_runtime, Some(&home_api), None, &path);
    wait_for_server_socket(&mut home_server, &home_api, Duration::from_secs(10));
    let (home_workspace, home_pane) = create_workspace(&home_api, "workspace-a");
    write_pane_marker(&home_api, &home_pane, HOME_MARKER);
    focus_workspace(&home_api, &home_workspace);

    let (mut client, output_rx) = spawn_client(&home_config, &home_runtime, &home_api, &path);
    let client_pid = client.child.process_id().expect("client PID");
    let mut output = String::new();
    let home_client_log = home_config.join(app_dir_name()).join("herdr-client.log");
    let home_server_log = home_config.join(app_dir_name()).join("herdr-server.log");
    let member_b_server_log = member_b_api
        .parent()
        .expect("member B session directory")
        .join("herdr-server.log");
    let member_c_server_log = member_c_api
        .parent()
        .expect("member C session directory")
        .join("herdr-server.log");
    let diagnostic_logs = [
        ("home client", home_client_log.as_path()),
        ("home server", home_server_log.as_path()),
        ("member B server", member_b_server_log.as_path()),
        ("member C server", member_c_server_log.as_path()),
    ];
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        "workspace-b@member-b",
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        "workspace-c@member-c",
        Duration::from_secs(15),
        &diagnostic_logs,
    );

    // A -> B -> C -> B -> A all happens inside this one TUI process. Returning
    // to B must resume the suspended connection rather than launching SSH twice.
    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, "workspace-b@member-b");
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        MEMBER_B_MARKER,
        Duration::from_secs(20),
        &diagnostic_logs,
    );
    assert_eq!(client.child.process_id(), Some(client_pid));
    assert_eq!(bridge_launch_count_for_target(&ssh_log, "fake-host-b"), 1);

    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, "workspace-c@member-c");
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        MEMBER_C_MARKER,
        Duration::from_secs(20),
        &diagnostic_logs,
    );
    assert_eq!(client.child.process_id(), Some(client_pid));
    assert_eq!(bridge_launch_count_for_target(&ssh_log, "fake-host-c"), 1);

    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, "workspace-b@member-b");
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        MEMBER_B_MARKER,
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    assert_eq!(bridge_launch_count_for_target(&ssh_log, "fake-host-b"), 1);

    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, "workspace-a@member-a");
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        HOME_MARKER,
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    assert_eq!(client.child.process_id(), Some(client_pid));

    // Resume B, then revoke C at the pinned home authority while B remains the
    // active server. The replacement directory must flow A -> client -> B.
    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, "workspace-b@member-b");
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        MEMBER_B_MARKER,
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    assert_eq!(bridge_launch_count_for_target(&ssh_log, "fake-host-b"), 1);
    thread::sleep(Duration::from_millis(200));
    clear_output(&output_rx, &mut output);

    write_config(
        &home_config,
        &format!(
            r#"onboarding = false
[emacs]
enabled = true
[remote]
manage_ssh_config = false
[federation]
enabled = true
member_id = "member-a"
member_target = "fake-home"

[[federation.endpoints]]
id = "member-b"
target = "fake-host-b"
session = "{REMOTE_SESSION}"
enabled = true
"#
        ),
    );
    let reload = send_json_request(
        &home_api,
        serde_json::json!({
            "id": "reload:revoke-member-c",
            "method": "server.reload_config",
            "params": {}
        }),
    );
    assert_eq!(
        reload.pointer("/result/status").and_then(Value::as_str),
        Some("applied")
    );
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        "workspace-a@member-a",
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    assert!(
        !strip_terminal_control_sequences(&output).contains("workspace-c@member-c"),
        "the active remote view must replace, rather than merge, the authoritative directory"
    );

    let member_c_launches_before_revoked_selection =
        bridge_launch_count_for_target(&ssh_log, "fake-host-c");
    clear_output(&output_rx, &mut output);
    navigate_to(&mut client, "workspace-c@member-c");
    thread::sleep(Duration::from_millis(700));
    client
        .writer
        .write_all(&[0x07])
        .expect("leave navigator search");
    client.writer.flush().expect("flush navigator search exit");
    thread::sleep(Duration::from_millis(100));
    client.writer.write_all(b"q").expect("close navigator");
    client.writer.flush().expect("flush navigator close");
    thread::sleep(Duration::from_millis(100));
    write_pane_marker(&member_b_api, &member_b_pane, MEMBER_B_AFTER_REVOCATION);
    wait_for_output(
        &mut client,
        &output_rx,
        &mut output,
        MEMBER_B_AFTER_REVOCATION,
        Duration::from_secs(15),
        &diagnostic_logs,
    );
    assert_eq!(client.child.process_id(), Some(client_pid));
    assert_eq!(
        bridge_launch_count_for_target(&ssh_log, "fake-host-c"),
        member_c_launches_before_revoked_selection,
        "a revoked member must not be dialed again"
    );
    assert!(client.child.try_wait().ok().flatten().is_none());

    drop(client);
    drop(home_server);
    drop(member_b_server);
    drop(member_c_server);
    cleanup_test_base(&home_base);
    cleanup_test_base(&member_b_base);
    cleanup_test_base(&member_c_base);
    let _ = fs::remove_dir_all(base);
}
