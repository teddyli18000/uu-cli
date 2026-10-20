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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            bindings: Vec::new(),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// A conservative local identity for remembering a user's authorization.
    ///
    /// Session names can be reused after UU restarts. `created_at` is therefore
    /// part of the fingerprint when the official CLI supplies it. A missing
    /// timestamp intentionally produces no fingerprint: callers must not carry
    /// an old authorization onto an instance they cannot distinguish safely.
    pub fn authorization_fingerprint(&self) -> Option<String> {
        let created_at = self.created_at.as_deref()?;
        Some(format!(
            "{}\u{1f}{}\u{1f}{}",
            self.device_identity, self.session_name, created_at
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_alone_is_not_authorization_identity() {
        let session = SessionInfo {
            device_identity: "device-1".into(),
            device_name: "car".into(),
            session_name: "bash-3".into(),
            shell: Some("/bin/bash".into()),
            state: "running".into(),
            created_at: None,
            attachable: Some(true),
        };
        assert_eq!(session.authorization_fingerprint(), None);
    }

    #[test]
    fn fingerprint_includes_device_name_and_creation_instance() {
        let a = SessionInfo {
            device_identity: "device-1".into(),
            device_name: "car".into(),
            session_name: "bash-3".into(),
            shell: Some("/bin/bash".into()),
            state: "running".into(),
            created_at: Some("2026-10-19T17:27:45Z".into()),
            attachable: Some(true),
        };
        let mut b = a.clone();
        b.created_at = Some("2026-10-20T17:27:45Z".into());
        assert_ne!(
            a.authorization_fingerprint(),
            b.authorization_fingerprint()
        );
    }
}
