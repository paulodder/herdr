use std::io;
use std::sync::mpsc;
use std::time::Duration;

use crate::api::schema::{Method, PaneTarget, Request};
use crate::config::FederationEndpointConfig;
use crate::federation::{EndpointConnectionStatus, EndpointState, EndpointWatch};

pub(super) fn run_federation_command(args: &[String]) -> io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("bridge") if args.len() == 1 => crate::federation::run_stdio_bridge().map(|()| 0),
        Some("list" | "status") => federation_list(&args[1..]),
        Some("watch") => federation_watch(&args[1..]),
        Some("attach") => federation_attach(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print_help();
            Ok(0)
        }
        _ => {
            print_help();
            Ok(2)
        }
    }
}

fn federation_list(args: &[String]) -> io::Result<i32> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => {
            eprintln!("usage: herdr federation list [--json]");
            return Ok(0);
        }
        _ => {
            eprintln!("usage: herdr federation list [--json]");
            return Ok(2);
        }
    };

    let loaded = crate::config::Config::load();
    if !loaded.config.federation.enabled {
        if json {
            println!("[]");
        } else {
            println!("federation is disabled");
        }
        return Ok(0);
    }
    let endpoints = loaded.config.federation.endpoints;
    let (tx, rx) = mpsc::channel();
    let mut count = 0;
    for endpoint in endpoints {
        count += 1;
        let tx = tx.clone();
        std::thread::spawn(move || {
            let state = probe_endpoint(endpoint);
            let _ = tx.send(state);
        });
    }
    drop(tx);

    let mut states = Vec::with_capacity(count);
    for _ in 0..count {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(state) => states.push(state),
            Err(_) => break,
        }
    }
    states.sort_by(|left, right| left.endpoint.id.cmp(&right.endpoint.id));

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&states).map_err(io::Error::other)?
        );
    } else if states.is_empty() {
        println!("no federation endpoints configured");
    } else {
        for state in &states {
            print_human_state(state);
        }
    }
    Ok((states
        .iter()
        .any(|state| state.endpoint.enabled && state.status != EndpointConnectionStatus::Connected))
        as i32)
}

fn federation_watch(args: &[String]) -> io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr federation watch");
        return Ok(2);
    }
    let loaded = crate::config::Config::load();
    if !loaded.config.federation.enabled {
        eprintln!("federation is disabled");
        return Ok(1);
    }
    let endpoints = loaded.config.federation.endpoints;
    if endpoints.is_empty() {
        eprintln!("no federation endpoints configured");
        return Ok(1);
    }

    let (tx, rx) = mpsc::channel();
    for endpoint in endpoints {
        let tx = tx.clone();
        std::thread::spawn(move || {
            crate::federation::run_endpoint_watch(endpoint, |state| tx.send(state).is_ok())
        });
    }
    drop(tx);
    while let Ok(state) = rx.recv() {
        println!(
            "{}",
            serde_json::to_string(&state).map_err(io::Error::other)?
        );
    }
    Ok(0)
}

fn federation_attach(args: &[String]) -> io::Result<i32> {
    let Some(endpoint_id) = args.first() else {
        eprintln!("usage: herdr federation attach <endpoint> [--pane PANE_ID]");
        return Ok(2);
    };
    let pane_id = match &args[1..] {
        [] => None,
        [flag, pane_id] if flag == "--pane" => Some(pane_id.as_str()),
        _ => {
            eprintln!("usage: herdr federation attach <endpoint> [--pane PANE_ID]");
            return Ok(2);
        }
    };
    let loaded = crate::config::Config::load();
    let Some(endpoint) = loaded
        .config
        .federation
        .endpoints
        .iter()
        .find(|endpoint| endpoint.id == *endpoint_id)
    else {
        eprintln!("unknown federation endpoint: {endpoint_id}");
        return Ok(1);
    };
    if !endpoint.enabled {
        eprintln!("federation endpoint {endpoint_id} is disabled");
        return Ok(1);
    }

    if let Some(pane_id) = pane_id {
        crate::federation::request(
            endpoint,
            &Request {
                id: format!("federation:{endpoint_id}:focus"),
                method: Method::PaneFocus(PaneTarget {
                    pane_id: pane_id.to_string(),
                }),
            },
        )?;
    }

    let executable = std::env::current_exe()?;
    let status = std::process::Command::new(executable)
        .arg("--remote")
        .arg(&endpoint.target)
        .arg("--session")
        .arg(&endpoint.session)
        .status()?;
    Ok(status.code().unwrap_or(1))
}

fn probe_endpoint(endpoint: FederationEndpointConfig) -> EndpointState {
    if !endpoint.enabled {
        return EndpointState::configured(endpoint);
    }
    match EndpointWatch::connect(endpoint.clone(), None) {
        Ok(watch) => watch.state.clone(),
        Err(err) => EndpointState {
            endpoint,
            status: classify_error(&err),
            snapshot: None,
            cursor: None,
            error: Some(err.to_string()),
        },
    }
}

fn classify_error(err: &io::Error) -> EndpointConnectionStatus {
    crate::federation::classify_error(err)
}

fn print_human_state(state: &EndpointState) {
    let label = state
        .endpoint
        .label
        .as_deref()
        .unwrap_or(&state.endpoint.id);
    let status = match state.status {
        EndpointConnectionStatus::Disabled => "disabled",
        EndpointConnectionStatus::Connecting => "connecting",
        EndpointConnectionStatus::Connected => "connected",
        EndpointConnectionStatus::Disconnected => "disconnected",
        EndpointConnectionStatus::Incompatible => "incompatible",
    };
    let counts = state.snapshot.as_ref().map(|snapshot| {
        format!(
            " · {} workspaces · {} panes",
            snapshot.workspaces.len(),
            snapshot.panes.len()
        )
    });
    println!(
        "{} ({})  {}{}",
        label,
        state.endpoint.id,
        status,
        counts.as_deref().unwrap_or("")
    );
    if let Some(error) = &state.error {
        println!("  {error}");
    }
}

fn print_help() {
    eprintln!("herdr federation commands:");
    eprintln!("  herdr federation list [--json]");
    eprintln!("  herdr federation watch");
    eprintln!("  herdr federation attach <endpoint> [--pane PANE_ID]");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_watch_errors_are_distinguished() {
        let err = io::Error::other("server does not support federation session.watch");
        assert_eq!(classify_error(&err), EndpointConnectionStatus::Incompatible);
    }
}
