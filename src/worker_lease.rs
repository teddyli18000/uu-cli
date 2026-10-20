use crate::model::SessionInfo;
use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum AcquireResult {
    Acquired(WorkerLease),
    Busy,
    Quarantined,
}

#[derive(Debug)]
pub struct WorkerLease {
    lock_file: File,
    state_path: PathBuf,
    fingerprint: String,
    in_flight: bool,
}

impl WorkerLease {
    /// Try to reserve one UU Terminal instance across all `uu` processes for the
    /// current Windows user. The lock is process-scoped and disappears on crash;
    /// the state marker intentionally does not, so a crashed in-flight job can
    /// never make the same PTY look safely idle again.
    pub fn try_acquire(config_path: &Path, session: &SessionInfo) -> Result<AcquireResult> {
        let fingerprint = session
            .authorization_fingerprint()
            .ok_or_else(|| anyhow!("terminal session has no stable instance identity"))?;
        let runtime_dir = runtime_dir(config_path)?;
        fs::create_dir_all(&runtime_dir).with_context(|| {
            format!(
                "cannot create worker state directory {}",
                runtime_dir.display()
            )
        })?;

        let key = fingerprint_key(&fingerprint);
        let lock_path = runtime_dir.join(format!("{key}.lock"));
        let state_path = runtime_dir.join(format!("{key}.inflight"));
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("cannot open worker lease {}", lock_path.display()))?;

        match lock_file.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if is_lock_contended(&error) => return Ok(AcquireResult::Busy),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot lock worker lease {}", lock_path.display()))
            }
        }

        if state_path.exists() {
            let _ = FileExt::unlock(&lock_file);
            return Ok(AcquireResult::Quarantined);
        }

        Ok(AcquireResult::Acquired(Self {
            lock_file,
            state_path,
            fingerprint,
            in_flight: false,
        }))
    }

    /// Must be called immediately before bytes that can execute remotely are
    /// written to the PTY. From this point until `complete`, a crash leaves the
    /// worker quarantined rather than risking accidental command replay/reuse.
    pub fn mark_in_flight(&mut self) -> Result<()> {
        if self.in_flight {
            return Ok(());
        }

        let mut state = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.state_path)
            .with_context(|| format!("cannot create {}", self.state_path.display()))?;
        writeln!(state, "pid={}", std::process::id())?;
        writeln!(state, "worker={}", self.fingerprint)?;
        state.sync_all()?;
        self.in_flight = true;
        Ok(())
    }

    /// Mark a known-complete job safe for reuse. Unknown/aborted jobs deliberately
    /// never call this; their marker survives the process and quarantines the PTY.
    pub fn complete(mut self) -> Result<()> {
        if self.in_flight {
            match fs::remove_file(&self.state_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("cannot clear worker state {}", self.state_path.display())
                    })
                }
            }
            self.in_flight = false;
        }
        FileExt::unlock(&self.lock_file).context("cannot release worker lease")?;
        Ok(())
    }
}

impl Drop for WorkerLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock_file);
    }
}

fn is_lock_contended(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(windows)]
    {
        // ERROR_LOCK_VIOLATION: Windows reports a non-blocking file-lock
        // collision as raw OS error 33 rather than ErrorKind::WouldBlock.
        return error.raw_os_error() == Some(33);
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn runtime_dir(config_path: &Path) -> Result<PathBuf> {
    let parent = config_path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent directory"))?;
    Ok(parent.join("workers"))
}

fn fingerprint_key(fingerprint: &str) -> String {
    let digest = Sha256::digest(fingerprint.as_bytes());
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn session(name: &str, created_at: &str) -> SessionInfo {
        SessionInfo {
            device_identity: "device-1".into(),
            device_name: "car".into(),
            session_name: name.into(),
            shell: Some("/bin/bash".into()),
            state: "running".into(),
            created_at: Some(created_at.into()),
            attachable: Some(true),
        }
    }

    fn config_path(tmp: &TempDir) -> PathBuf {
        tmp.path().join("uu-cli").join("config.json")
    }

    #[test]
    fn same_worker_cannot_be_leased_twice() {
        let tmp = TempDir::new().unwrap();
        let path = config_path(&tmp);
        let first = match WorkerLease::try_acquire(&path, &session("bash-1", "t1")).unwrap() {
            AcquireResult::Acquired(lease) => lease,
            other => panic!("unexpected acquisition result: {other:?}"),
        };

        assert!(matches!(
            WorkerLease::try_acquire(&path, &session("bash-1", "t1")).unwrap(),
            AcquireResult::Busy
        ));
        drop(first);
    }

    #[test]
    fn separate_session_instances_have_separate_leases() {
        let tmp = TempDir::new().unwrap();
        let path = config_path(&tmp);
        let first = WorkerLease::try_acquire(&path, &session("bash-1", "t1")).unwrap();
        let second = WorkerLease::try_acquire(&path, &session("bash-2", "t2")).unwrap();
        assert!(matches!(first, AcquireResult::Acquired(_)));
        assert!(matches!(second, AcquireResult::Acquired(_)));
    }

    #[test]
    fn dropped_inflight_lease_quarantines_same_instance() {
        let tmp = TempDir::new().unwrap();
        let path = config_path(&tmp);
        let worker = session("bash-1", "t1");
        {
            let mut lease = match WorkerLease::try_acquire(&path, &worker).unwrap() {
                AcquireResult::Acquired(lease) => lease,
                other => panic!("unexpected acquisition result: {other:?}"),
            };
            lease.mark_in_flight().unwrap();
        }

        assert!(matches!(
            WorkerLease::try_acquire(&path, &worker).unwrap(),
            AcquireResult::Quarantined
        ));
    }

    #[test]
    fn completed_job_is_reusable() {
        let tmp = TempDir::new().unwrap();
        let path = config_path(&tmp);
        let worker = session("bash-1", "t1");
        let mut lease = match WorkerLease::try_acquire(&path, &worker).unwrap() {
            AcquireResult::Acquired(lease) => lease,
            other => panic!("unexpected acquisition result: {other:?}"),
        };
        lease.mark_in_flight().unwrap();
        lease.complete().unwrap();

        assert!(matches!(
            WorkerLease::try_acquire(&path, &worker).unwrap(),
            AcquireResult::Acquired(_)
        ));
    }
}
