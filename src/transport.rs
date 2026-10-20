#![cfg(windows)]

use crate::runner::{run_framed_job, JobResult, JobRunError};
use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct TermPty {
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

impl TermPty {
    fn spawn(cli_path: &Path, args: Vec<OsString>, operation: &str) -> Result<Self> {
        if !cli_path.is_file() {
            bail!("official UU CLI not found at {}", cli_path.display());
        }

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to create local ConPTY for official UU CLI")?;

        let mut command = CommandBuilder::new(cli_path.as_os_str());
        command.args(args);

        let master = pair.master;
        let slave = pair.slave;
        let mut child = slave
            .spawn_command(command)
            .with_context(|| format!("failed to start {} in ConPTY", cli_path.display()))?;
        let mut reader = master
            .try_clone_reader()
            .context("failed to open UU terminal PTY reader")?;
        let writer = master
            .take_writer()
            .context("failed to open UU terminal PTY writer")?;
        drop(slave);

        // Vendor-side argument/ownership failures normally surface quickly. Catch
        // them before caller command bytes are ever written; only pre-send failures
        // are safe to retry on another transport/session.
        thread::sleep(Duration::from_millis(250));
        if let Some(status) = child
            .try_wait()
            .context("failed to inspect official UU terminal process")?
        {
            let mut diagnostic = Vec::new();
            let _ = reader.read_to_end(&mut diagnostic);
            let text = String::from_utf8_lossy(&diagnostic);
            let text = text.trim();
            if text.is_empty() {
                bail!("official UU {operation} exited immediately ({status:?})");
            }
            bail!("official UU {operation} exited immediately ({status:?}): {text}");
        }

        Ok(Self {
            child,
            _master: master,
            reader,
            writer,
        })
    }

    fn run_job<O: Write>(
        &mut self,
        output: &mut O,
        remote_root: &str,
        shell_command: &str,
    ) -> Result<JobResult, JobRunError> {
        run_framed_job(
            &mut self.reader,
            &mut self.writer,
            output,
            remote_root,
            shell_command,
        )
    }
}

impl Drop for TermPty {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
    }
}

/// Attachment to a session the user already opened in the official UU GUI.
/// Kept as a compatibility/fallback path while the newer `term new` path is
/// validated against the Linux host. It must never be used to steal a busy GUI
/// terminal from the user.
pub struct OfficialAttachPty {
    inner: TermPty,
}

impl OfficialAttachPty {
    pub fn attach(cli_path: &Path, device_identity: &str, session_name: &str) -> Result<Self> {
        validate_identity(device_identity, session_name)?;
        let inner = TermPty::spawn(
            cli_path,
            attach_args(device_identity, session_name),
            &format!("attach for session {session_name}"),
        )?;
        Ok(Self { inner })
    }

    pub fn process_id(&self) -> Option<u32> {
        self.inner.child.process_id()
    }

    pub fn has_exited(&mut self) -> std::io::Result<bool> {
        self.inner.child.try_wait().map(|status| status.is_some())
    }

    pub fn run_job<O: Write>(
        &mut self,
        output: &mut O,
        remote_root: &str,
        shell_command: &str,
    ) -> Result<JobResult, JobRunError> {
        self.inner.run_job(output, remote_root, shell_command)
    }
}

/// A uu-cli-owned ephemeral remote terminal created through the official CLI.
///
/// One manually opened GUI Terminal may still be required to activate/keep warm
/// the vendor term channel, but that GUI session is only an anchor: commands run
/// in this fresh uu-cli-owned session and therefore cannot mutate or occupy the
/// user's visible shell context.
pub struct OfficialNewPty {
    session_name: String,
    inner: TermPty,
}

impl OfficialNewPty {
    pub fn create(cli_path: &Path, device_identity: &str) -> Result<Self> {
        if device_identity.is_empty() {
            bail!("UU device identity is empty");
        }
        let session_name = owned_session_name();
        let inner = TermPty::spawn(
            cli_path,
            new_args(device_identity, &session_name),
            &format!("new session {session_name}"),
        )?;
        Ok(Self {
            session_name,
            inner,
        })
    }

    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    pub fn run_job<O: Write>(
        &mut self,
        output: &mut O,
        remote_root: &str,
        shell_command: &str,
    ) -> Result<JobResult, JobRunError> {
        self.inner.run_job(output, remote_root, shell_command)
    }
}

fn validate_identity(device_identity: &str, session_name: &str) -> Result<()> {
    if device_identity.is_empty() {
        bail!("UU device identity is empty");
    }
    if session_name.is_empty() {
        bail!("UU terminal session name is empty");
    }
    Ok(())
}

fn attach_args(device_identity: &str, session_name: &str) -> Vec<OsString> {
    ["term", "-d", device_identity, "attach", session_name]
        .into_iter()
        .map(OsString::from)
        .collect()
}

fn new_args(device_identity: &str, session_name: &str) -> Vec<OsString> {
    ["term", "-d", device_identity, "new", session_name]
        .into_iter()
        .map(OsString::from)
        .collect()
}

fn owned_session_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("uu-{:x}-{tick:x}-{counter:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_official_attach_command_shape_is_exact() {
        assert_eq!(
            attach_args("device-123", "bash-3"),
            vec![
                OsString::from("term"),
                OsString::from("-d"),
                OsString::from("device-123"),
                OsString::from("attach"),
                OsString::from("bash-3"),
            ]
        );
    }

    #[test]
    fn current_official_new_command_shape_is_exact() {
        assert_eq!(
            new_args("device-123", "uu-job"),
            vec![
                OsString::from("term"),
                OsString::from("-d"),
                OsString::from("device-123"),
                OsString::from("new"),
                OsString::from("uu-job"),
            ]
        );
    }

    #[test]
    fn owned_session_names_are_ascii_short_and_unique() {
        let a = owned_session_name();
        let b = owned_session_name();
        assert_ne!(a, b);
        assert!(a.starts_with("uu-"));
        assert!(a.len() < 64);
        assert!(a
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
    }
}
