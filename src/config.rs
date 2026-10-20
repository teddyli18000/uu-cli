use crate::model::Config;
use anyhow::{anyhow, Context, Result};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn from_env() -> Result<Self> {
        if let Some(path) = env::var_os("UU_CLI_CONFIG") {
            return Ok(Self { path: path.into() });
        }

        let base = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or_else(|| anyhow!("cannot determine per-user config directory"))?;

        Ok(Self {
            path: base.join("uu-cli").join("config.json"),
        })
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Config> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid config {}", self.path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(err).with_context(|| format!("cannot read {}", self.path.display())),
        }
    }

    pub fn save(&self, config: &Config) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }

        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent"))?;
        let file_name = self
            .path
            .file_name()
            .ok_or_else(|| anyhow!("config path has no file name"))?
            .to_string_lossy();
        let temp = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
        let backup = parent.join(format!(".{file_name}.bak"));
        let data = serde_json::to_vec_pretty(config)?;

        {
            let mut file = fs::File::create(&temp)
                .with_context(|| format!("cannot create {}", temp.display()))?;
            file.write_all(&data)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }

        if self.path.exists() {
            let _ = fs::remove_file(&backup);
            fs::rename(&self.path, &backup).with_context(|| {
                format!(
                    "cannot stage old config {} -> {}",
                    self.path.display(),
                    backup.display()
                )
            })?;
        }

        if let Err(err) = fs::rename(&temp, &self.path) {
            if backup.exists() && !self.path.exists() {
                let _ = fs::rename(&backup, &self.path);
            }
            let _ = fs::remove_file(&temp);
            return Err(err).with_context(|| format!("cannot replace {}", self.path.display()));
        }

        let _ = fs::remove_file(&backup);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WorkspaceBinding;
    use tempfile::TempDir;

    #[test]
    fn missing_config_is_default() {
        let tmp = TempDir::new().unwrap();
        let store = ConfigStore::at(tmp.path().join("config.json"));
        assert_eq!(store.load().unwrap(), Config::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let store = ConfigStore::at(tmp.path().join("cfg").join("config.json"));
        let mut config = Config::default();
        config.bindings.push(WorkspaceBinding {
            local_root: root,
            device_identity: "device-1".into(),
            device_name: "Car".into(),
            remote_root: "/home/aic".into(),
        });

        store.save(&config).unwrap();
        assert_eq!(store.load().unwrap(), config);
    }
}
