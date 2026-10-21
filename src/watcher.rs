//! Compatibility entry points for the removed background watcher, plus the
//! official UU process lifetime handle used by active terminal attachments.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::Duration;

/// Runtime data is stored beside the per-user config, outside the workspace.
pub fn runtime_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("runtime")
}

/// Shared by inline setup and binding interactions.
/// The returned file must stay alive for the full interactive operation.
pub fn interaction_lock_path(config_path: &Path) -> PathBuf {
    runtime_dir(config_path).join("interaction.lock")
}

/// Try to acquire the shared human-interaction lock.
///
/// Ok(None) means another setup or binding interaction is already active.
pub fn try_interaction_lock(config_path: &Path) -> Result<Option<File>> {
    try_lock_file(&interaction_lock_path(config_path))
}

fn try_lock_file(path: &Path) -> Result<Option<File>> {
    let parent = path
        .parent()
        .context("runtime lock path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("cannot create runtime directory {}", parent.display()))?;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot open runtime lock {}", path.display()))?;

    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(file)),
        Err(error) if is_lock_contended(&error) => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("cannot acquire runtime lock {}", path.display()))
        }
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
        error.raw_os_error() == Some(33)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Kept as a compatibility shim for older callers. Terminal discovery and
/// human choices belong to explicit foreground commands; ordinary commands
/// never start background work.
pub fn spawn_if_needed(_executable: &Path, _config_path: &Path) -> Result<()> {
    Ok(())
}

/// Kept so an obsolete `__watch` invocation exits immediately and harmlessly.
pub fn run(_config_path: &Path) -> Result<()> {
    Ok(())
}

/// A waitable handle to the official UU GUI process captured by process name.
/// The owned file closes the native handle when the lifetime leaves scope.
#[cfg(windows)]
pub(crate) struct OfficialUuLifetime(File);

#[cfg(windows)]
impl OfficialUuLifetime {
    /// Wait for the captured process to exit. Returns false when the timeout elapses.
    pub(crate) fn wait(&self, timeout: Duration) -> Result<bool> {
        windows_process::wait_for_exit(&self.0, timeout)
    }
}

/// Capture a waitable process handle for the currently running official UU GUI.
/// Returns `None` when no matching process is present.
#[cfg(windows)]
pub(crate) fn official_lifetime() -> Result<Option<OfficialUuLifetime>> {
    windows_process::official_lifetime("GameViewer.exe")
}

#[cfg(windows)]
pub fn is_official_uu_running() -> Result<bool> {
    windows_process::has_process_named("GameViewer.exe")
}

#[cfg(not(windows))]
pub fn is_official_uu_running() -> Result<bool> {
    Ok(false)
}

#[cfg(windows)]
mod windows_process {
    use anyhow::{Context, Result};
    use std::ffi::c_void;
    use std::fs::File;
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::time::Duration;

    type Handle = *mut c_void;

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const MAX_PATH: usize = 260;
    const INVALID_HANDLE_VALUE: Handle = (-1isize) as Handle;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; MAX_PATH],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> Handle;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    struct Snapshot(Handle);

    impl Drop for Snapshot {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn has_process_named(expected_name: &str) -> Result<bool> {
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error()).context("cannot enumerate Windows processes");
        }
        let _snapshot = Snapshot(raw);

        let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        if unsafe { Process32FirstW(raw, &mut entry) } == 0 {
            return Err(io::Error::last_os_error()).context("cannot read Windows process list");
        }

        loop {
            let end = entry
                .sz_exe_file
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(MAX_PATH);
            let process_name = String::from_utf16_lossy(&entry.sz_exe_file[..end]);
            if process_name.eq_ignore_ascii_case(expected_name) {
                return Ok(true);
            }
            if unsafe { Process32NextW(raw, &mut entry) } == 0 {
                break;
            }
        }

        Ok(false)
    }

    pub fn official_lifetime(expected_name: &str) -> Result<Option<super::OfficialUuLifetime>> {
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error()).context("cannot enumerate Windows processes");
        }
        let _snapshot = Snapshot(raw);

        let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
        if unsafe { Process32FirstW(raw, &mut entry) } == 0 {
            return Err(io::Error::last_os_error()).context("cannot read Windows process list");
        }

        loop {
            let end = entry
                .sz_exe_file
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(MAX_PATH);
            let process_name = String::from_utf16_lossy(&entry.sz_exe_file[..end]);
            if process_name.eq_ignore_ascii_case(expected_name) {
                if let Some(lifetime) = lifetime_for_pid(entry.th32_process_id)? {
                    return Ok(Some(lifetime));
                }
            }
            if unsafe { Process32NextW(raw, &mut entry) } == 0 {
                break;
            }
        }

        Ok(None)
    }

    pub fn lifetime_for_pid(process_id: u32) -> Result<Option<super::OfficialUuLifetime>> {
        let raw = unsafe { OpenProcess(SYNCHRONIZE, 0, process_id) };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            // The process can exit between the process snapshot and OpenProcess.
            if error.raw_os_error() == Some(87) {
                return Ok(None);
            }
            return Err(error)
                .with_context(|| format!("cannot open process {process_id} for wait"));
        }

        Ok(Some(super::OfficialUuLifetime(unsafe {
            File::from_raw_handle(raw)
        })))
    }

    pub fn wait_for_exit(process: &File, timeout: Duration) -> Result<bool> {
        let milliseconds = timeout.as_millis().min((u32::MAX - 1) as u128) as u32;
        match unsafe { WaitForSingleObject(process.as_raw_handle(), milliseconds) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()).context("cannot wait for official UU process"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{run, runtime_dir, spawn_if_needed, try_interaction_lock};
    #[cfg(windows)]
    use std::path::Path;
    #[cfg(windows)]
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    #[test]
    fn interaction_lock_is_exclusive_until_released() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join("config.json");
        let lock = try_interaction_lock(&config_path)
            .unwrap()
            .expect("acquire interaction lock");

        assert!(try_interaction_lock(&config_path).unwrap().is_none());

        drop(lock);
        assert!(try_interaction_lock(&config_path).unwrap().is_some());
    }

    #[test]
    fn legacy_background_entry_points_return_without_starting_work() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join("config.json");
        let impossible_executable = temp.path().join("missing-uu.exe");

        spawn_if_needed(&impossible_executable, &config_path).unwrap();
        run(&config_path).unwrap();

        assert!(!runtime_dir(&config_path).exists());
    }

    #[cfg(windows)]
    #[test]
    fn process_lifetime_waits_for_own_short_child() {
        use std::time::Duration;

        let mut child = Command::new("cmd.exe")
            .args(["/C", "ping -n 3 127.0.0.1 > NUL"])
            .spawn()
            .expect("spawn short child process");
        let lifetime = super::windows_process::lifetime_for_pid(child.id())
            .unwrap()
            .expect("open child process lifetime");

        assert!(!lifetime.wait(Duration::ZERO).unwrap());
        assert!(lifetime.wait(Duration::from_secs(10)).unwrap());
        assert!(child.wait().unwrap().success());
    }

    #[cfg(windows)]
    #[test]
    fn watcher_returns_when_no_official_uu_process_is_present() {
        if super::official_lifetime()
            .expect("query official UU process")
            .is_some()
        {
            return;
        }
        let temp = tempdir().unwrap();
        super::run(temp.path().join("config.json").as_path()).unwrap();
    }
}
