use super::{Config, ConfigStore};
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) fn migrate_source(target: &ConfigStore, source: &Path) -> Result<()> {
    let source_parent = source
        .parent()
        .context("legacy config path has no parent directory")?;
    let source_identity = existing_config_identity(source)?;
    let marker = source_identity
        .as_deref()
        .map(|path| marker_path(target.path(), path))
        .transpose()?;

    if marker.as_ref().is_some_and(|path| path.exists()) {
        return Ok(());
    }

    let source_store = ConfigStore::at(source);
    let source_lock = if source_identity.is_some() {
        let lock = source_store.open_lock()?;
        FileExt::lock_exclusive(&lock).context("cannot lock legacy uu-cli config for migration")?;
        Some(lock)
    } else {
        None
    };

    if marker.as_ref().is_some_and(|path| path.exists()) {
        if let Some(lock) = &source_lock {
            FileExt::unlock(lock).context("cannot unlock legacy uu-cli config")?;
        }
        return Ok(());
    }

    check_no_active_jobs(source)?;
    copy_inflight_markers(source_parent, target.path())?;

    if let Some(marker) = marker {
        let legacy = source_store.load_unlocked()?;
        let mut current = target.load_unlocked()?;
        let before = current.clone();
        merge_config(&mut current, &legacy);
        if current != before {
            target.save_unlocked(&current)?;
        }
        write_done_marker(&marker, source_identity.as_deref().unwrap())?;
    }

    if let Some(lock) = &source_lock {
        FileExt::unlock(lock).context("cannot unlock legacy uu-cli config")?;
    }
    Ok(())
}

fn existing_config_identity(source: &Path) -> Result<Option<PathBuf>> {
    match fs::canonicalize(source) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let name = source
                .file_name()
                .context("legacy config path has no filename")?
                .to_string_lossy();
            let backup = source.with_file_name(format!(".{name}.bak"));
            match fs::canonicalize(&backup) {
                Ok(_) => {
                    let parent = fs::canonicalize(source.parent().unwrap()).with_context(|| {
                        format!(
                            "cannot resolve legacy config directory {}",
                            source.parent().unwrap().display()
                        )
                    })?;
                    Ok(Some(parent.join(source.file_name().unwrap())))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error).with_context(|| {
                    format!("cannot resolve legacy config backup {}", backup.display())
                }),
            }
        }
        Err(error) => {
            Err(error).with_context(|| format!("cannot resolve legacy config {}", source.display()))
        }
    }
}

fn marker_path(target_config: &Path, physical_source: &Path) -> Result<PathBuf> {
    let parent = target_config
        .parent()
        .context("target config path has no parent directory")?;
    let source_text = physical_source.to_string_lossy();
    #[cfg(windows)]
    let source_key = source_text.to_lowercase();
    #[cfg(not(windows))]
    let source_key = source_text.into_owned();
    let digest = Sha256::digest(source_key.as_bytes());
    let key: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(parent.join("migration").join(format!("{key}.done")))
}

fn check_no_active_jobs(legacy_config: &Path) -> Result<()> {
    let parent = legacy_config
        .parent()
        .context("legacy config path has no parent directory")?;
    check_lock_tree(&parent.join("workers"))?;
    check_lock_tree(&parent.join("runtime").join("job-queue"))
}

fn check_lock_tree(directory: &Path) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", directory.display()))
        }
    };

    for entry in entries {
        let entry = entry.with_context(|| format!("cannot inspect {}", directory.display()))?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            check_lock_tree(&entry.path())?;
        } else if file_type.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "lock")
        {
            check_lock_is_idle(&entry.path())?;
        }
    }
    Ok(())
}

fn check_lock_is_idle(path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot inspect legacy job lock {}", path.display()))?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => FileExt::unlock(&file)
            .with_context(|| format!("cannot release legacy job lock {}", path.display())),
        Err(error) if is_lock_contended(&error) => {
            bail!(
                "legacy uu-cli activity is still active; wait for old jobs to finish before upgrading, then retry ({})",
                path.display()
            )
        }
        Err(error) => {
            Err(error).with_context(|| format!("cannot check legacy job lock {}", path.display()))
        }
    }
}

fn is_lock_contended(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(33)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn copy_inflight_markers(legacy_config_dir: &Path, target_config: &Path) -> Result<()> {
    let source_workers = legacy_config_dir.join("workers");
    let entries = match fs::read_dir(&source_workers) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot inspect {}", source_workers.display()))
        }
    };
    let target_parent = target_config
        .parent()
        .context("target config path has no parent directory")?;
    let target_workers = target_parent.join("workers");

    for entry in entries {
        let entry =
            entry.with_context(|| format!("cannot inspect {}", source_workers.display()))?;
        if !entry.file_type()?.is_file()
            || entry
                .path()
                .extension()
                .map_or(true, |extension| extension != "inflight")
        {
            continue;
        }
        copy_file_if_absent(&entry.path(), &target_workers.join(entry.file_name()))?;
    }
    Ok(())
}

fn copy_file_if_absent(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Ok(());
    }
    let parent = destination
        .parent()
        .context("migration marker path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let temp = temporary_path(destination);
    let result = (|| {
        let mut input = fs::File::open(source).with_context(|| {
            format!("cannot read legacy quarantine marker {}", source.display())
        })?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .with_context(|| format!("cannot create migration file {}", temp.display()))?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        match fs::rename(&temp, destination) {
            Ok(()) => Ok(()),
            Err(_) if destination.exists() => {
                let _ = fs::remove_file(&temp);
                Ok(())
            }
            Err(error) => Err(error)
                .with_context(|| format!("cannot preserve quarantine marker {}", source.display())),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_done_marker(path: &Path, physical_source: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let parent = path
        .parent()
        .context("migration marker path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let temp = temporary_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .with_context(|| format!("cannot create migration marker {}", temp.display()))?;
        writeln!(file, "{}", physical_source.display())?;
        file.sync_all()?;
        match fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(_) if path.exists() => {
                let _ = fs::remove_file(&temp);
                Ok(())
            }
            Err(error) => Err(error)
                .with_context(|| format!("cannot commit migration marker {}", path.display())),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn temporary_path(path: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), nonce))
}

fn merge_config(current: &mut Config, legacy: &Config) {
    current.version = current.version.max(legacy.version);
    dedupe_bindings(&mut current.bindings);
    for binding in &legacy.bindings {
        if !current
            .bindings
            .iter()
            .any(|existing| crate::workspace::same_path(&existing.local_root, &binding.local_root))
        {
            current.bindings.push(binding.clone());
        }
    }

    let mut declined = Vec::new();
    let mut seen_declined = HashSet::new();
    for item in current
        .declined_workers
        .iter()
        .chain(legacy.declined_workers.iter())
    {
        if seen_declined.insert(item.clone()) {
            declined.push(item.clone());
        }
    }
    let declined_set: HashSet<_> = declined.iter().cloned().collect();
    let mut authorized = Vec::new();
    let mut seen_authorized = HashSet::new();
    for item in current
        .authorized_workers
        .iter()
        .chain(legacy.authorized_workers.iter())
    {
        if !declined_set.contains(item) && seen_authorized.insert(item.clone()) {
            authorized.push(item.clone());
        }
    }
    current.declined_workers = declined;
    current.authorized_workers = authorized;
}

fn dedupe_bindings(bindings: &mut Vec<crate::model::WorkspaceBinding>) {
    let mut unique = Vec::with_capacity(bindings.len());
    for binding in bindings.drain(..) {
        if !unique
            .iter()
            .any(|existing: &crate::model::WorkspaceBinding| {
                crate::workspace::same_path(&existing.local_root, &binding.local_root)
            })
        {
            unique.push(binding);
        }
    }
    *bindings = unique;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job_queue::QueueTicket;
    use crate::model::{SessionInfo, WorkspaceBinding};
    use crate::worker_lease::{AcquireResult, WorkerLease};
    use tempfile::TempDir;

    fn binding(root: &Path, device: &str, remote_root: &str) -> WorkspaceBinding {
        WorkspaceBinding {
            local_root: root.to_path_buf(),
            device_identity: device.into(),
            device_name: format!("{device} name"),
            remote_root: remote_root.into(),
        }
    }

    fn session(name: &str, created_at: &str) -> SessionInfo {
        SessionInfo {
            device_identity: "device-1".into(),
            device_name: "test device".into(),
            session_name: name.into(),
            shell: Some("bash".into()),
            state: "detached".into(),
            created_at: Some(created_at.into()),
            attachable: Some(true),
        }
    }

    fn fingerprint_key(fingerprint: &str) -> String {
        let digest = Sha256::digest(fingerprint.as_bytes());
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn store_with_source(tmp: &TempDir, source: &Path) -> ConfigStore {
        ConfigStore::at_with_legacy_sources(
            tmp.path().join("new").join("config.json"),
            [source.to_path_buf()],
        )
    }

    #[test]
    fn imports_each_legacy_context_once_and_merges_their_state() {
        let tmp = TempDir::new().unwrap();
        let workspace_one = tmp.path().join("workspace-one");
        let workspace_two = tmp.path().join("workspace-two");
        fs::create_dir_all(&workspace_one).unwrap();
        fs::create_dir_all(&workspace_two).unwrap();
        let source_one = tmp
            .path()
            .join("context-one")
            .join("uu-cli")
            .join("config.json");
        let source_two = tmp
            .path()
            .join("context-two")
            .join("uu-cli")
            .join("config.json");
        let mut first = Config::default();
        first
            .bindings
            .push(binding(&workspace_one, "device-1", "/one"));
        first.authorized_workers.push("worker-one".into());
        ConfigStore::at(&source_one).save(&first).unwrap();
        let mut second = Config::default();
        second
            .bindings
            .push(binding(&workspace_two, "device-2", "/two"));
        second.authorized_workers.push("worker-two".into());
        ConfigStore::at(&source_two).save(&second).unwrap();

        let target = ConfigStore::at_with_legacy_sources(
            tmp.path().join("new").join("config.json"),
            [source_one.clone(), source_two.clone()],
        );
        let merged = target.load().unwrap();
        assert_eq!(merged.bindings.len(), 2);
        assert_eq!(merged.authorized_workers, ["worker-one", "worker-two"]);
        assert!(
            marker_path(target.path(), &fs::canonicalize(&source_one).unwrap())
                .unwrap()
                .is_file()
        );
        assert!(
            marker_path(target.path(), &fs::canonicalize(&source_two).unwrap())
                .unwrap()
                .is_file()
        );

        let mut changed_first = first;
        let late_workspace = tmp.path().join("late-workspace");
        fs::create_dir_all(&late_workspace).unwrap();
        changed_first
            .bindings
            .push(binding(&late_workspace, "device-3", "/late"));
        ConfigStore::at(&source_one).save(&changed_first).unwrap();
        let merged_again = target.load().unwrap();
        assert_eq!(
            merged_again.bindings.len(),
            2,
            "a completed source is imported once"
        );
        assert!(!merged_again
            .bindings
            .iter()
            .any(|item| item.remote_root == "/late"));
    }

    #[test]
    fn legacy_decline_takes_precedence_over_authorized_worker() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("legacy").join("config.json");
        let fingerprint = "device-1\u{1f}bash-1\u{1f}created";
        let mut legacy = Config::default();
        legacy.authorized_workers.push(fingerprint.into());
        ConfigStore::at(&source).save(&legacy).unwrap();
        let mut current = Config::default();
        current.declined_workers.push(fingerprint.into());
        let target = ConfigStore::at_with_legacy_sources(
            tmp.path().join("new").join("config.json"),
            [source],
        );
        ConfigStore::at(target.path()).save(&current).unwrap();

        let merged = target.load().unwrap();
        assert!(merged.authorized_workers.is_empty());
        assert_eq!(merged.declined_workers, [fingerprint]);
    }

    #[test]
    fn existing_binding_wins_over_conflicting_legacy_binding() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let source = tmp.path().join("legacy").join("config.json");
        let mut legacy = Config::default();
        legacy
            .bindings
            .push(binding(&workspace, "device-1", "/legacy-root"));
        ConfigStore::at(&source).save(&legacy).unwrap();
        let mut current = Config::default();
        current
            .bindings
            .push(binding(&workspace, "device-2", "/current-root"));
        let target = ConfigStore::at_with_legacy_sources(
            tmp.path().join("new").join("config.json"),
            [source],
        );
        ConfigStore::at(target.path()).save(&current).unwrap();

        let merged = target.load().unwrap();
        assert_eq!(merged.bindings.len(), 1);
        assert_eq!(merged.bindings[0].device_identity, "device-2");
        assert_eq!(merged.bindings[0].remote_root, "/current-root");
    }

    #[test]
    fn legacy_inflight_marker_is_preserved_as_quarantine() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("legacy").join("config.json");
        let worker = session("bash-1", "created");
        let mut legacy = Config::default();
        legacy
            .authorized_workers
            .push(worker.authorization_fingerprint().unwrap());
        ConfigStore::at(&source).save(&legacy).unwrap();
        let fingerprint = worker.authorization_fingerprint().unwrap();
        let old_marker = source
            .parent()
            .unwrap()
            .join("workers")
            .join(format!("{}.inflight", fingerprint_key(&fingerprint)));
        fs::create_dir_all(old_marker.parent().unwrap()).unwrap();
        fs::write(&old_marker, b"pid=99\nworker=unknown\n").unwrap();
        let target = store_with_source(&tmp, &source);

        target.load().unwrap();

        let new_marker = target
            .path()
            .parent()
            .unwrap()
            .join("workers")
            .join(old_marker.file_name().unwrap());
        assert_eq!(fs::read(new_marker).unwrap(), b"pid=99\nworker=unknown\n");
        assert!(matches!(
            WorkerLease::try_acquire(target.path(), &worker).unwrap(),
            AcquireResult::Quarantined
        ));
    }

    #[test]
    fn active_legacy_worker_or_queue_job_blocks_migration_without_marker() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("legacy").join("config.json");
        let worker = session("bash-1", "created");
        let mut legacy = Config::default();
        legacy
            .authorized_workers
            .push(worker.authorization_fingerprint().unwrap());
        ConfigStore::at(&source).save(&legacy).unwrap();
        let target = store_with_source(&tmp, &source);
        let source_marker =
            marker_path(target.path(), &fs::canonicalize(&source).unwrap()).unwrap();

        let active_worker = match WorkerLease::try_acquire(&source, &worker).unwrap() {
            AcquireResult::Acquired(lease) => lease,
            _ => panic!("expected to acquire legacy worker lock for fixture"),
        };
        let error = target.load().unwrap_err();
        assert!(format!("{error:#}").contains("wait for old jobs to finish before upgrading"));
        assert!(!target.path().exists());
        assert!(!source_marker.exists());
        drop(active_worker);

        let active_queue = QueueTicket::join(&source, &worker.device_identity).unwrap();
        let error = target.load().unwrap_err();
        assert!(format!("{error:#}").contains("wait for old jobs to finish before upgrading"));
        assert!(!target.path().exists());
        assert!(!source_marker.exists());
        drop(active_queue);

        assert!(
            target.load().is_ok(),
            "migration should resume after old work ends"
        );
        assert!(source_marker.is_file());
    }
}
