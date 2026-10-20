use crate::model::Config;
use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use std::env;
use std::fs::{self, File, OpenOptions};
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
        let lock = self.open_lock()?;
        lock.lock_shared().context("cannot lock uu-cli config for reading")?;
        let result = self.load_unlocked();
        let unlock = FileExt::unlock(&lock).context("cannot unlock uu-cli config");
        result.and(unlock.map(|_| ())).and_then(|_| self.load_unlocked())
    }

    pub fn save(&self, config: &Config) -> Result<()> {
        let lock = self.open_lock()?;
        lock.lock_exclusive()
            .context("cannot lock uu-cli config for writing")?;
        let result = self.save_unlocked(config);
        let unlock = FileExt::unlock(&lock).context("cannot unlock uu-cli config");
        match (result, unlock) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    /// Atomically reload, mutate, and save configuration under one cross-process
    /// lock. Runtime paths that add/remove workers or workspace bindings must use
    /// this instead of saving a stale snapshot, otherwise concurrent `uu` calls
    /// can silently overwrite each other's state.
    pub fn update<F>(&self, mutate: F) -> Result<Config>
    where
        F: FnOnce(&mut Config) -> Result<()>,
    {
        let lock = self.open_lock()?;
        lock.lock_exclusive()
            .context("cannot lock uu-cli config for update")?;

        let result = (|| {
            let mut config = self.load_unlocked()?;
            mutate(&mut config)?;
            self.save_unlocked(&config)?;
            Ok(config)
        })();

        let unlock = FileExt::unlock(&lock).context("cannot unlock uu-cli config");
        match (result, unlock) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(config), Ok(())) => Ok(config),
        }
    }

    fn open_lock(&self) -> Result<File> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent"))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
        let file_name = self
            .path
            .file_name()
            .ok_or_else(|| anyhow!("config path has no file name"))?
            .to_string_lossy();
        let lock_path = parent.join(format!(".{file_name}.lock"));
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("cannot open config lock {}", lock_path.display()))
    }

    fn load_unlocked(&self) -> Result<Config> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("invalid config {}", self.path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(err).with_context(|| format!("cannot read {}", self.path.display())),
        }
    }

    fn save_unlocked(&self, config: &Config) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent"))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;

        let file_name = self
            .path
            .file_name()
            .ok_or_else(|| anyhow!("config path has no file name"))?
            .to_string_lossy();
        let temp = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            unique_suffix()
        ));
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

fn unique_suffix() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
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

    #[test]
    fn transactional_updates_preserve_prior_mutations() {
        let tmp = TempDir::new().unwrap();
        let store = ConfigStore::at(tmp.path().join("cfg").join("config.json"));

        store
            .update(|config| {
                config.remember_owned_worker("device-1", "uu-one");
                Ok(())
            })
            .unwrap();
        store
            .update(|config| {
                config.remember_owned_worker("device-1", "uu-two");
                Ok(())
            })
            .unwrap();

        let config = store.load().unwrap();
        assert_eq!(config.owned_workers.len(), 2);
        assert!(config
            .owned_workers
            .iter()
            .any(|worker| worker.session_name == "uu-one"));
        assert!(config
            .owned_workers
            .iter()
            .any(|worker| worker.session_name == "uu-two"));
    }
}
