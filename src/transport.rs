#![cfg(windows)]

use crate::runner::{
    run_framed_job, run_framed_job_with_input, run_framed_pwd_probe, JobInput,
    JobResult, JobRunError, ReadPump,
};
use anyhow::{anyhow, bail, Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

struct TermPty {
    child: Box<dyn Child + Send + Sync>,
    master: Option<Box<dyn MasterPty + Send>>,
    reader: ReadPump,
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
        let raw_reader = master
            .try_clone_reader()
            .context("failed to open UU terminal PTY reader")?;
        let writer = master
            .take_writer()
            .context("failed to open UU terminal PTY writer")?;
        drop(slave);
        let mut reader = ReadPump::spawn(raw_reader);


        // Catch argument/ownership failures before command bytes can be sent.
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                drop(writer);
                drop(master);
                reader.shutdown_and_join();
                return Err(error).context("failed to inspect official UU terminal process");
            }
        };
        if let Some(status) = status {
            let mut diagnostic = Vec::new();
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                match reader.read_event_timeout(Duration::from_millis(25)) {
                    Ok(crate::runner::PumpRead::Data(bytes)) => {
                        diagnostic.extend_from_slice(&bytes);
                    }
                    Ok(crate::runner::PumpRead::Eof) | Err(_) => break,
                    Ok(crate::runner::PumpRead::Timeout) => {}
                }
            }
            drop(writer);
            drop(master);
            reader.shutdown_and_join();
            let text = String::from_utf8_lossy(&diagnostic);
            let text = text.trim();
            if text.is_empty() {
                bail!("official UU {operation} exited immediately ({status:?})");
            }
            bail!("official UU {operation} exited immediately ({status:?}): {text}");
        }
        Ok(Self {
            child,
            master: Some(master),
            reader,
            writer,
        })
    }

    fn run_job<O: Write + ?Sized>(
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

    fn run_job_with_input<O: Write + ?Sized>(
        &mut self,
        output: &mut O,
        remote_root: &str,
        shell_command: &str,
        input: &Receiver<JobInput>,
    ) -> Result<JobResult, JobRunError> {
        run_framed_job_with_input(
            &mut self.reader,
            &mut self.writer,
            output,
            remote_root,
            shell_command,
            input,
        )
    }

    fn capture_pwd(&mut self) -> Result<String, JobRunError> {
        let mut output = Vec::new();
        let result = run_framed_pwd_probe(&mut self.reader, &mut self.writer, &mut output)?;
        if result.exit_code != 0 {
            return Err(JobRunError::Completed(anyhow!(
                "remote pwd exited with code {}",
                result.exit_code
            )));
        }

        let text = String::from_utf8(output).map_err(|error| {
            JobRunError::Completed(anyhow!("remote pwd output is not valid UTF-8: {error}"))
        })?;
        let path = text.trim_end_matches(['\r', '\n']);
        if path.is_empty() {
            return Err(JobRunError::Completed(anyhow!(
                "remote pwd returned an empty path"
            )));
        }
        Ok(path.to_owned())
    }
}

impl Drop for TermPty {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        // Releasing ConPTY handles unblocks the reader. Its join is bounded so
        // shutdown cannot hang if an underlying Windows read ignores closure.
        drop(std::mem::replace(
            &mut self.writer,
            Box::new(std::io::sink()),
        ));
        self.master.take();
        self.reader.shutdown_and_join();
    }
}

/// Attach only to an existing terminal explicitly authorized by the caller.
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

    pub fn run_job<O: Write + ?Sized>(
        &mut self,
        output: &mut O,
        remote_root: &str,
        shell_command: &str,
    ) -> Result<JobResult, JobRunError> {
        self.inner.run_job(output, remote_root, shell_command)
    }

    pub fn run_job_with_input<O: Write + ?Sized>(
        &mut self,
        output: &mut O,
        remote_root: &str,
        shell_command: &str,
        input: &Receiver<JobInput>,
    ) -> Result<JobResult, JobRunError> {
        self.inner
            .run_job_with_input(output, remote_root, shell_command, input)
    }

    /// Read the attached shell's current directory without changing its state.
    pub fn capture_pwd(&mut self) -> Result<String, JobRunError> {
        self.inner.capture_pwd()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::CompletionDecoder;
    use crate::runner::{initialize_conpty, PumpRead};
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use std::time::{Duration, Instant};

    const PROBE: &[u8] =
        b"\r\nUUCconptyprobeBpayload\r\nUUCconptyprobeE0Z\r\n";

    #[test]
    fn current_official_attach_command_targets_the_existing_authorized_session() {
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
    fn windows_conpty_preserves_ascii_completion_markers() {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let master = pty.master;
        let slave = pty.slave;
        let mut command = CommandBuilder::new("cmd.exe");
        command.args([
            "/d",
            "/c",
            "echo.&echo UUCconptyprobeBpayload&echo UUCconptyprobeE0Z",
        ]);
        let mut child = slave.spawn_command(command).unwrap();
        let mut writer = master.take_writer().unwrap();
        let reader = master.try_clone_reader().unwrap();
        drop(slave);
        let mut reader = ReadPump::spawn(reader);

        let (startup_output, cursor_queries) =
            initialize_conpty(&mut reader, &mut writer, Duration::from_millis(500)).unwrap();
        eprintln!("ConPTY startup cursor queries answered: {cursor_queries}");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = startup_output;
        while Instant::now() < deadline
            && !observed.windows(PROBE.len()).any(|window| window == PROBE)
        {
            match reader
                .read_event_timeout(Duration::from_millis(100))
                .unwrap()
            {
                PumpRead::Data(bytes) => observed.extend_from_slice(&bytes),
                PumpRead::Eof | PumpRead::Timeout => {}
            }
        }

        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
        }
        drop(writer);
        drop(master);
        reader.shutdown_and_join();

        assert!(
            observed.windows(PROBE.len()).any(|window| window == PROBE),
            "ConPTY did not preserve the complete ASCII marker payload: {observed:?}"
        );

        let mut decoder = CompletionDecoder::new("conptyprobe").unwrap();
        assert_eq!(decoder.push(&observed).unwrap(), b"payload");
        assert_eq!(decoder.completed(), Some(0));
    }
}
