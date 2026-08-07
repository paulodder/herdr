use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

#[cfg(test)]
use std::path::PathBuf;

use crate::api::schema::RuntimeIdentity;

#[cfg(not(test))]
const SERVER_ID_FILE: &str = "runtime-server-id";
#[cfg(not(test))]
const SESSION_ID_FILE: &str = "runtime-session-id";

#[cfg(not(test))]
static CURRENT: std::sync::OnceLock<RuntimeIdentity> = std::sync::OnceLock::new();

/// Returns the stable identity for the active config root and session.
///
/// The IDs are deliberately stored outside `session.json`: structural session
/// restore may replace that file, while federation identity must survive it.
#[cfg(not(test))]
pub fn current() -> io::Result<RuntimeIdentity> {
    if let Some(identity) = CURRENT.get() {
        return Ok(identity.clone());
    }

    let identity = load_or_create(
        &crate::config::config_dir().join(SERVER_ID_FILE),
        &crate::session::data_dir().join(SESSION_ID_FILE),
        active_session_name(),
    )?;
    let _ = CURRENT.set(identity.clone());
    Ok(CURRENT.get().cloned().unwrap_or(identity))
}

#[cfg(test)]
pub fn current() -> io::Result<RuntimeIdentity> {
    Ok(RuntimeIdentity {
        server_id: "00000000-0000-4000-8000-000000000001".into(),
        session_id: "00000000-0000-4000-8000-000000000002".into(),
        session_name: active_session_name(),
    })
}

fn active_session_name() -> String {
    crate::session::active_name()
        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string())
}

fn load_or_create(
    server_id_path: &Path,
    session_id_path: &Path,
    session_name: String,
) -> io::Result<RuntimeIdentity> {
    Ok(RuntimeIdentity {
        server_id: load_or_create_id(server_id_path)?,
        session_id: load_or_create_id(session_id_path)?,
        session_name,
    })
}

fn load_or_create_id(path: &Path) -> io::Result<String> {
    match read_valid_id(path) {
        Ok(id) => return Ok(id),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let id = uuid::Uuid::new_v4().to_string();
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(id.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            Ok(id)
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => read_valid_id(path),
        Err(err) => Err(err),
    }
}

fn read_valid_id(path: &Path) -> io::Result<String> {
    let raw = fs::read_to_string(path)?;
    let id = raw.trim();
    let parsed = uuid::Uuid::parse_str(id).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "invalid Herdr runtime identity at {}: {err}",
                path.display()
            ),
        )
    })?;
    Ok(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-runtime-identity-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    #[test]
    fn identity_is_stable_and_sessions_are_distinct() {
        let root = unique_dir("stable");
        let server = root.join("server-id");
        let first_session = root.join("sessions/first/session-id");
        let second_session = root.join("sessions/second/session-id");

        let first = load_or_create(&server, &first_session, "first".into()).expect("first");
        let repeated = load_or_create(&server, &first_session, "first".into()).expect("repeat");
        let second = load_or_create(&server, &second_session, "second".into()).expect("second");

        assert_eq!(first, repeated);
        assert_eq!(first.server_id, second.server_id);
        assert_ne!(first.session_id, second.session_id);
        assert_eq!(second.session_name, "second");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_identity_is_rejected_without_replacement() {
        let root = unique_dir("invalid");
        fs::create_dir_all(&root).expect("root");
        let path = root.join("server-id");
        fs::write(&path, "not-an-id\n").expect("write");

        let err = load_or_create_id(&path).expect_err("invalid identity must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read_to_string(&path).expect("read"), "not-an-id\n");
        let _ = fs::remove_dir_all(root);
    }
}
