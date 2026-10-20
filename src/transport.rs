#![cfg(windows)]

use crate::runner::{run_framed_job, JobResult, JobRunError};
use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;

/// One local ConPTY attachment to an already-open official UU terminal session.
///
/// This adapter deliberately does not create remote sessions. It only gives the
/// official `uuyc-cli term ... attach` process a real interactive terminal, then
/// exposes its byte stream to the framed job runner. Whether the current UU
/// release accepts an external attach to a GUI-created session is a runtime
/// vendor behavior and must not be inferred from successful compilation here.
pub struct OfficialAttachPty {
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}

impl OfficialAttachPty {
    pub fn attach(cli_path: &Path, device_identity: &str, session_name: &str) -> Result<Self> {
        if !cli_path.is_file() {
            bail!("official UU CLI not found at {}", cli_path.display());
        }
        if device_identity.is_empty() {
            bail!("UU device identity is empty");
        }
        if session_name.is_empty() {
            bail!("UU terminal session name is empty");
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
        command.args(attach_args(device_identity, session_name));

        let master = pair.master;
        let slave = pair.slave;
        let child = slave
            .spawn_command(command)
            .with_context(|| format!("failed to start {} in ConPTY", cli_path.display()))?;
        let reader = master
            .try_clone_reader()
            .context("failed to open UU terminal PTY reader")?;
        let writer = master
            .take_writer()
            .context("failed to open UU terminal PTY writer")?;
        drop(slave);

        Ok(Self {
            child,
            _master: master,
            reader,
            writer,
        })
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    pub fn has_exited(&mut self) -> std::io::Result<bool> {
        self.child.try_wait().map(|status| status.is_some())
    }

    pub fn run_job<O: Write>(
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

impl Drop for OfficialAttachPty {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
    }
}

fn attach_args(device_identity: &str, session_name: &str) -> Vec<OsString> {
    ["term", "-d", device_identity, "attach", session_name]
        .into_iter()
        .map(OsString::from)
        .collect()
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
}
