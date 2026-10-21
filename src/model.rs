use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceBinding {
    pub local_root: PathBuf,
    pub device_identity: String,
    pub device_name: String,
    pub remote_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub bindings: Vec<WorkspaceBinding>,
    /// Authorization is scoped to an actual Terminal instance, never its name.
    #[serde(default)]
    pub authorized_workers: Vec<String>,
    #[serde(default)]
    pub declined_workers: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            bindings: Vec::new(),
            authorized_workers: Vec::new(),
            declined_workers: Vec::new(),
        }
    }
}

impl Config {
    pub fn upsert_binding(&mut self, binding: WorkspaceBinding) {
        if let Some(existing) = self
            .bindings
            .iter_mut()
            .find(|item| crate::workspace::same_path(&item.local_root, &binding.local_root))
        {
            *existing = binding;
        } else {
            self.bindings.push(binding);
        }
    }

    pub fn authorize_session(&mut self, session: &SessionInfo) -> bool {
        let Some(key) = session.authorization_fingerprint() else {
            return false;
        };
        if self.declined_workers.contains(&key) {
            return false;
        }
        if !self.authorized_workers.contains(&key) {
            self.authorized_workers.push(key);
        }
        true
    }

    /// Explicit human acceptance can reverse a prior default-deny written before a prompt.
    pub fn accept_session(&mut self, session: &SessionInfo) -> bool {
        if let Some(key) = session.authorization_fingerprint() {
            self.declined_workers.retain(|item| item != &key);
        }
        self.authorize_session(session)
    }

    pub fn decline_session(&mut self, session: &SessionInfo) -> bool {
        let Some(key) = session.authorization_fingerprint() else {
            return false;
        };
        self.authorized_workers.retain(|item| item != &key);
        if !self.declined_workers.contains(&key) {
            self.declined_workers.push(key);
        }
        true
    }

    pub fn is_session_authorized(&self, session: &SessionInfo) -> bool {
        session.authorization_fingerprint().is_some_and(|key| {
            self.authorized_workers.contains(&key) && !self.declined_workers.contains(&key)
        })
    }

    pub fn is_session_declined(&self, session: &SessionInfo) -> bool {
        session
            .authorization_fingerprint()
            .is_some_and(|key| self.declined_workers.contains(&key))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub device_identity: String,
    pub device_name: String,
    pub is_online: bool,
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInfo {
    pub device_identity: String,
    pub device_name: String,
    pub session_name: String,
    pub shell: Option<String>,
    pub state: String,
    pub created_at: Option<String>,
    pub attachable: Option<bool>,
}

impl SessionInfo {
    pub fn authorization_fingerprint(&self) -> Option<String> {
        let created_at = self.created_at.as_deref()?;
        if [
            self.device_identity.as_str(),
            self.session_name.as_str(),
            created_at,
        ]
        .iter()
        .any(|part| part.trim().is_empty() || part.contains('\u{1f}'))
        {
            return None;
        }
        Some(format!(
            "{}\u{1f}{}\u{1f}{}",
            self.device_identity, self.session_name, created_at
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(created_at: Option<&str>) -> SessionInfo {
        SessionInfo {
            device_identity: "device-1".into(),
            device_name: "car".into(),
            session_name: "bash-3".into(),
            shell: Some("/bin/bash".into()),
            state: "running".into(),
            created_at: created_at.map(str::to_owned),
            attachable: Some(true),
        }
    }
    #[test]
    fn identity_requires_nonempty_creation_instance() {
        assert_eq!(session(None).authorization_fingerprint(), None);
        assert_eq!(session(Some("")).authorization_fingerprint(), None);
    }
    #[test]
    fn replacement_cannot_inherit_authorization() {
        let mut config = Config::default();
        let a = session(Some("t1"));
        assert!(config.authorize_session(&a));
        assert!(config.is_session_authorized(&a));
        assert!(!config.is_session_authorized(&session(Some("t2"))));
    }
    #[test]
    fn declined_instance_stays_manual() {
        let mut config = Config::default();
        let a = session(Some("t1"));
        config.decline_session(&a);
        assert!(!config.authorize_session(&a));
        assert!(config.is_session_declined(&a));
        assert!(!config.is_session_authorized(&a));
    }
    #[test]
    fn legacy_name_only_records_are_not_authority() {
        let config: Config = serde_json::from_str(r#"{"version":1,"owned_workers":[{"device_identity":"device-1","session_name":"bash-3"}]}"#).unwrap();
        assert!(!config.is_session_authorized(&session(Some("t1"))));
    }
}
