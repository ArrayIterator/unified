// service_monitor module -- Registered SSH servers for remote library checks.

use unified_core::error::{UnifiedError, UnifiedResult};
use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, RwLock};

// Remote checks over SSH.
//
// There is no SSH client library in the dependency set, and no
// provider or cluster record holds SSH credentials (cluster nodes
// authenticate with HMAC over HTTP; provider connections are API
// credentials), so remote checks run through the system `ssh` binary
// -- the same approach the backup module uses for `pg_dump` and the
// mail module uses for `sendmail`. The app never runs as root for
// this: the check runs as the application user, against servers the
// operator configured.
//
// Credentials: a registered server stores only the host, port,
// username, auth kind, and (for keyfile auth) the keyfile *path*.
// A password is never stored anywhere -- not in the registry, not in
// a file, not in a log. For password auth the caller supplies a
// one-time password in the re-check request; it lives only in that
// request, is passed to `sshpass` through the `SSHPASS` environment
// variable (never on a command line, where it would show in a
// process listing), and is dropped when the request ends. When the
// `sshpass` helper is not installed, password auth is refused with
// a clear error and keyfile auth is the way in.

/// One configured remote server. No secret material is stored here:
/// keyfile auth stores the keyfile path only, and password auth
/// stores nothing at all (see the note above).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshServer {
    /// Operator-chosen id, used as the re-check target name.
    pub id: String,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    /// `"keyfile"` or `"password"`.
    pub auth_kind: String,
    /// Absolute path to the private key file (keyfile auth only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyfile: Option<String>,
}

pub(crate) fn default_ssh_port() -> u16 {
    22
}

pub(crate) fn valid_ssh_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('-')
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
}

pub(crate) fn valid_ssh_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= 64
        && !username.starts_with('-')
        && username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

pub(crate) fn valid_keyfile_path(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\0') && path.len() <= 4096
}

pub(crate) fn valid_server_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Validate a server record. The same checks guard both the registry
/// endpoints and inline re-check targets.
pub(crate) fn validate_ssh_server(server: &SshServer) -> UnifiedResult<()> {
    if !valid_server_id(&server.id) {
        return Err(UnifiedError::bad_request(
            "SSH server id must be 1-64 characters of letters, digits, '-' or '_'",
        ));
    }
    validate_ssh_connection(
        &server.host,
        server.port,
        &server.username,
        &server.auth_kind,
        server.keyfile.as_deref(),
    )
}

/// Validate the connection fields of a server (host, port, username,
/// auth kind, keyfile) without an id, for inline re-check targets
/// whose label is derived from the host and never registered.
pub(crate) fn validate_ssh_connection(
    host: &str,
    port: u16,
    username: &str,
    auth_kind: &str,
    keyfile: Option<&str>,
) -> UnifiedResult<()> {
    if !valid_ssh_host(host) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid SSH host: \"{host}\""
        )));
    }
    if port == 0 {
        return Err(UnifiedError::bad_request("SSH port must be 1-65535"));
    }
    if !valid_ssh_username(username) {
        return Err(UnifiedError::bad_request(format!(
            "Invalid SSH username: \"{username}\""
        )));
    }
    match auth_kind {
        "keyfile" => {
            let Some(keyfile) = keyfile else {
                return Err(UnifiedError::bad_request(
                    "Keyfile authentication requires a keyfile path",
                ));
            };
            if !valid_keyfile_path(keyfile) {
                return Err(UnifiedError::bad_request(
                    "Keyfile path must be an absolute path",
                ));
            }
        }
        "password" => {
            if keyfile.is_some() {
                return Err(UnifiedError::bad_request(
                    "Password authentication does not take a keyfile path",
                ));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(
                "SSH auth_kind must be \"keyfile\" or \"password\"",
            ));
        }
    }
    Ok(())
}


pub(crate) static SSH_SERVERS: LazyLock<RwLock<Vec<SshServer>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Registered remote servers (no secret material is ever in these
/// records). Held in memory; a restart clears the registry.

pub fn list_ssh_servers() -> Vec<SshServer> {
    let servers = SSH_SERVERS.read().expect("ssh servers lock poisoned");
    let mut out = servers.clone();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Register (or replace, by id) a remote server for library checks.
pub fn register_ssh_server(server: SshServer) -> UnifiedResult<SshServer> {
    validate_ssh_server(&server)?;
    let mut servers = SSH_SERVERS.write().expect("ssh servers lock poisoned");
    if let Some(existing) = servers.iter_mut().find(|entry| entry.id == server.id) {
        *existing = server.clone();
    } else {
        servers.push(server.clone());
    }
    Ok(server)
}

pub fn remove_ssh_server(id: &str) -> UnifiedResult<()> {
    let mut servers = SSH_SERVERS.write().expect("ssh servers lock poisoned");
    let before = servers.len();
    servers.retain(|entry| entry.id != id);
    if servers.len() == before {
        return Err(UnifiedError::not_found(format!(
            "SSH server \"{id}\" is not registered"
        )));
    }
    Ok(())
}

pub(crate) fn find_ssh_server(id: &str) -> Option<SshServer> {
    SSH_SERVERS
        .read()
        .expect("ssh servers lock poisoned")
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyfile_server() -> SshServer {
        SshServer {
            id: "srv-1".to_string(),
            host: "example.com".to_string(),
            port: 22,
            username: "deploy".to_string(),
            auth_kind: "keyfile".to_string(),
            keyfile: Some("/home/deploy/.ssh/id_ed25519".to_string()),
        }
    }

    #[test]
    fn default_ssh_port_is_22() {
        assert_eq!(default_ssh_port(), 22);
    }

    #[test]
    fn valid_ssh_host_rules() {
        assert!(valid_ssh_host("example.com"));
        assert!(valid_ssh_host("10.0.0.1"));
        assert!(!valid_ssh_host(""));
        assert!(!valid_ssh_host("-bad.example.com"));
        assert!(!valid_ssh_host("bad host"));
    }

    #[test]
    fn valid_ssh_username_rules() {
        assert!(valid_ssh_username("deploy"));
        assert!(valid_ssh_username("user_name-1.2"));
        assert!(!valid_ssh_username(""));
        assert!(!valid_ssh_username("-deploy"));
        assert!(!valid_ssh_username("user name"));
    }

    #[test]
    fn valid_keyfile_path_requires_absolute_path() {
        assert!(valid_keyfile_path("/home/u/.ssh/id_ed25519"));
        assert!(!valid_keyfile_path("relative/path"));
        assert!(!valid_keyfile_path(""));
    }

    #[test]
    fn valid_server_id_rules() {
        assert!(valid_server_id("srv-1_x"));
        assert!(!valid_server_id(""));
        assert!(!valid_server_id("bad id"));
        assert!(!valid_server_id(&"x".repeat(65)));
    }

    #[test]
    fn validate_ssh_server_accepts_keyfile_server() {
        assert!(validate_ssh_server(&keyfile_server()).is_ok());
    }

    #[test]
    fn validate_ssh_server_rejects_bad_auth() {
        let mut server = keyfile_server();
        server.auth_kind = "token".to_string();
        assert!(validate_ssh_server(&server).is_err());
        let mut password_server = keyfile_server();
        password_server.auth_kind = "password".to_string();
        assert!(validate_ssh_server(&password_server).is_err());
        let mut no_key = keyfile_server();
        no_key.keyfile = None;
        assert!(validate_ssh_server(&no_key).is_err());
    }
}
