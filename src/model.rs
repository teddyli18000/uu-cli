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
pub struct OwnedWorker {
    pub device_identity: String,
    pub session_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub version: u32,
    #[serde(default)]
    pub bindings: Vec<WorkspaceBinding>,
    /// Legacy manual-session authorization retained for config compatibility.
    /// New builds never attach to user GUI sessions automatically.
    #[serde(default)]
    pub authorized_workers: Vec<String>,
    /// Remote terminal sessions created by uu-cli itself. Names are generated with
    /// high-entropy process/time components and scoped by device identity.
    #[serde(default)]
    pub owned_workers: Vec<OwnedWorker>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            bindings: Vec::new(),
            authorized_workers: Vec::new(),
            owned_workers: Vec::new(),
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

    pub fn remember_owned_worker(&mut self, device_identity: &str, session_name: &str) -> bool {
        if device_identity.is_empty() || session_name.is_empty() {
            return false;
        }
        if self.owned_workers.iter().any(|worker| {
            worker.device_identity == device_identity && worker.session_name == session_name
        }) {
            return false;
        }
        self.owned_workers.push(OwnedWorker {
            device_identity: device_identity.to_owned(),
            session_name: session_name.to_owned(),
        });
        true
    }

    pub fn is_owned_worker(&self, session: &SessionInfo) -> bool {
        self.owned_workers.iter().any(|worker| {
            worker.device_identity == session.device_identity
                && worker.session_name == session.session_name
        })
    }

    pub fn forget_missing_owned_workers<'a>(
        &mut self,
        device_identity: &str,
        live_sessions: impl IntoIterator<Item = &'a SessionInfo>,
    ) -> bool {
        let live_names = live_sessions
            .into_iter()
            .map(|session| session.session_name.as_str())
            .collect::<std::collections::HashSet<_>>();
        let before = self.owned_workers.len();
        self.owned_workers.retain(|worker| {
            worker.device_identity != device_identity || live_names.contains(worker.session_name.as_str())
        });
        self.owned_workers.len() != before
    }

    pub fn authorize_session(&mut self, session: &SessionInfo) -> bool {
        let Some(fingerprint) = session.authorization_fingerprint() else {
            return false;
        };
        if self
            .authorized_workers
            .iter()
            .any(|item| item == &fingerprint)
        {
            return true;
        }
        self.authorized_workers.push(fingerprint);
        true
    }

    pub fn is_session_authorized(&self, session: &SessionInfo) -> bool {
        let Some(fingerprint) = session.authorization_fingerprint() else {
            return false;
        };
        self.authorized_workers
            .iter()
            .any(|item| item == &fingerprint)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub device_identity: String,
    pub device_name: String,
    pub is_online: bool,
    pub platform: Option<String>,
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
    /// A conservative instance identity used for cross-process locking of an
    /// already-created worker. Session names can be reused after UU restarts, so
    /// created_at must be present before an old lock/authorization can apply.
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
    fn session_name_alone_is_not_instance_identity() {
        assert_eq!(session(None).authorization_fingerprint(), None);
    }

    #[test]
    fn fingerprint_includes_device_identity_and_creation_instance() {
        let a = session(Some("2026-10-19T17:27:45Z"));
        let mut b = a.clone();
        b.created_at = Some("2026-10-20T17:27:45Z".into());
        assert_ne!(a.authorization_fingerprint(), b.authorization_fingerprint());
    }

    #[test]
    fn owned_worker_is_scoped_by_device_and_name() {
        let mut config = Config::default();
        assert!(config.remember_owned_worker("device-1", "uu-a"));
        assert!(!config.remember_owned_worker("device-1", "uu-a"));

        let mut owned = session(Some("t"));
        owned.session_name = "uu-a".into();
        assert!(config.is_owned_worker(&owned));
        owned.device_identity = "device-2".into();
        assert!(!config.is_owned_worker(&owned));
    }

    #[test]
    fn stale_owned_worker_record_is_pruned_without_touching_other_device() {
        let mut config = Config::default();
        config.remember_owned_worker("device-1", "uu-old");
        config.remember_owned_worker("device-1", "uu-live");
        config.remember_owned_worker("device-2", "uu-other");

        let mut live = session(Some("t"));
        live.session_name = "uu-live".into();
        assert!(config.forget_missing_owned_workers("device-1", [&live]));
        assert_eq!(config.owned_workers.len(), 2);
        assert!(config
            .owned_workers
            .iter()
            .any(|worker| worker.session_name == "uu-live"));
        assert!(config
            .owned_workers
            .iter()
            .any(|worker| worker.session_name == "uu-other"));
    }

    #[test]
    fn legacy_authorization_remains_instance_scoped() {
        let a = session(Some("2026-10-19T17:27:45Z"));
        let mut replacement = a.clone();
        replacement.created_at = Some("2026-10-20T17:27:45Z".into());

        let mut config = Config::default();
        assert!(config.authorize_session(&a));
        assert!(config.is_session_authorized(&a));
        assert!(!config.is_session_authorized(&replacement));
    }
}
