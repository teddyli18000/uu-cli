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
    pub state: String,
}
