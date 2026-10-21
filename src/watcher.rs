//! Low-idle Windows watcher for newly opened official UU terminals.
//!
//! The GUI process-name probe is a cheap capability check based on the currently
//! observed GameViewer.exe host. A renamed or materially different client may
//! not be detected; in that case the watcher sleeps and does not query uuyc-cli.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

#[cfg(windows)]
const ACTIVE_POLL: std::time::Duration = std::time::Duration::from_secs(20);
#[cfg(windows)]
const NO_UU_POLL: std::time::Duration = std::time::Duration::from_secs(60);

/// Runtime data is stored beside the per-user config, outside the workspace.
pub fn runtime_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("runtime")
}

/// Shared by short setup/authorization consoles and the watcher prompt check.
/// The returned file must stay alive for the full interactive operation.
pub fn interaction_lock_path(config_path: &Path) -> PathBuf {
    runtime_dir(config_path).join("interaction.lock")
}

/// Try to acquire the shared human-interaction lock.
///
/// Ok(None) means another setup or authorization interaction is already
/// active. A console mode must recheck its session candidates after acquiring
/// this lock, because a waiting uu invocation may have authorized them meanwhile.
pub fn try_interaction_lock(config_path: &Path) -> Result<Option<File>> {
    try_lock_file(&interaction_lock_path(config_path))
}

#[cfg(windows)]
fn watcher_lock_path(config_path: &Path) -> PathBuf {
    runtime_dir(config_path).join("watcher.lock")
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

/// Start one hidden watcher when the official UU GUI is already running.
///
/// Each uu invocation may call this function. The cross-process watcher lock
/// prevents a second long-lived watcher; the process only polls at 20 seconds
/// while UU is active and sleeps for 60 seconds while it is absent.
pub fn spawn_if_needed(executable: &Path, config_path: &Path) -> Result<()> {
    if std::env::var("UU_CLI_NO_BACKGROUND").is_ok_and(|value| value == "1") {
        return Ok(());
    }

    #[cfg(windows)]
    {
        if !matches!(is_official_uu_running(), Ok(true)) {
            return Ok(());
        }

        // This probe avoids spawning helpers on ordinary uu invocations while a
        // watcher is alive. A simultaneous startup race is resolved by the
        // child's lifetime lock in run().
        if try_lock_file(&watcher_lock_path(config_path))?.is_none() {
            return Ok(());
        }

        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        Command::new(executable)
            .arg("__watch")
            .env("UU_CLI_CONFIG", config_path.as_os_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
            .spawn()
            .with_context(|| format!("cannot start hidden watcher {}", executable.display()))?;
    }

    #[cfg(not(windows))]
    {
        let _ = (executable, config_path);
    }

    Ok(())
}

/// Run the hidden watcher mode. The process keeps an exclusive runtime lock
/// until it exits, so only one inventory poller is active per user config.
pub fn run(config_path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        run_windows(config_path)
    }

    #[cfg(not(windows))]
    {
        let _ = config_path;
        Ok(())
    }
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
fn authorization_executable(current_executable: &Path) -> Result<PathBuf> {
    let is_background_helper = current_executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("uu-background.exe"));

    if is_background_helper {
        let directory = current_executable
            .parent()
            .context("uu-background.exe has no parent directory")?;
        Ok(directory.join("uu.exe"))
    } else {
        Ok(current_executable.to_path_buf())
    }
}

#[cfg(windows)]
fn run_windows(config_path: &Path) -> Result<()> {
    use crate::config::ConfigStore;
    use crate::inventory::discover_inventory;
    use crate::uuyc::OfficialCli;
    use std::process::{Child, Command};
    use std::thread;

    use std::os::windows::process::CommandExt;

    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

    let Some(_watcher_lock) = try_lock_file(&watcher_lock_path(config_path))? else {
        return Ok(());
    };

    let config_store = ConfigStore::at(config_path.to_path_buf());
    let current_executable = std::env::current_exe().context("cannot locate uu executable")?;
    let authorization_executable = authorization_executable(&current_executable)?;
    let mut official_cli = None;
    let mut authorization_console: Option<Child> = None;

    loop {
        let authorization_finished = match authorization_console.as_mut() {
            Some(child) => child.try_wait()?.is_some(),
            None => false,
        };
        if authorization_finished {
            authorization_console = None;
        }

        if !matches!(is_official_uu_running(), Ok(true)) {
            official_cli = None;
            thread::sleep(NO_UU_POLL);
            continue;
        }

        if authorization_console.is_none() {
            if official_cli.is_none() {
                official_cli = OfficialCli::locate().ok();
            }

            if let Some(cli) = official_cli.as_ref() {
                // Foreground waiting/decision flows own this lock and perform
                // their own discovery; skip a duplicate official CLI query.
                let Some(interaction_lock) = try_interaction_lock(config_path)? else {
                    thread::sleep(ACTIVE_POLL);
                    continue;
                };

                let snapshot = match discover_inventory(cli) {
                    Ok(snapshot) => snapshot,
                    Err(_) => {
                        official_cli = None;
                        thread::sleep(ACTIVE_POLL);
                        continue;
                    }
                };

                let config = config_store.load()?;
                if has_promptable_sessions(&snapshot, &config) {
                    // Release before launch so the child can acquire the lock
                    // and recheck the sessions before prompting.
                    drop(interaction_lock);
                    authorization_console = Command::new(&authorization_executable)
                        .arg("__authorize")
                        .env("UU_CLI_CONFIG", config_path.as_os_str())
                        .creation_flags(CREATE_NEW_CONSOLE)
                        .spawn()
                        .ok();
                }
            }
        }

        thread::sleep(ACTIVE_POLL);
    }
}

#[cfg(windows)]
fn has_promptable_sessions(
    snapshot: &crate::inventory::InventorySnapshot,
    config: &crate::model::Config,
) -> bool {
    use crate::authorization::{classify_session, SessionDecision};

    snapshot.eligible_sessions().into_iter().any(|session| {
        classify_session(
            session,
            &config.authorized_workers,
            &config.declined_workers,
            None,
        ) == SessionDecision::Prompt
    })
}

#[cfg(windows)]
mod windows_process {
    use anyhow::{Context, Result};
    use std::ffi::c_void;
    use std::io;

    type Handle = *mut c_void;

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const MAX_PATH: usize = 260;
    const INVALID_HANDLE_VALUE: Handle = (-1isize) as Handle;

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
}

#[cfg(test)]
mod tests {
    use super::{runtime_dir, try_interaction_lock, try_lock_file};
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn runtime_lock_is_exclusive_across_processes_and_released() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join("config.json");
        let lock_path = runtime_dir(&config_path).join("watcher.lock");
        let lock = try_lock_file(&lock_path)
            .unwrap()
            .expect("acquire watcher lock");

        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "watcher::tests::runtime_lock_child_probe"])
            .env("UU_CLI_TEST_LOCK_PATH", &lock_path)
            .status()
            .unwrap();
        assert!(status.success(), "child process could acquire held lock");

        drop(lock);
        assert!(try_lock_file(&lock_path).unwrap().is_some());
    }

    #[test]
    fn runtime_lock_child_probe() {
        let Some(path) = std::env::var_os("UU_CLI_TEST_LOCK_PATH") else {
            return;
        };
        assert!(try_lock_file(Path::new(&path)).unwrap().is_none());
    }

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

    #[cfg(windows)]
    #[test]
    fn background_helper_authorizes_with_sibling_cli() {
        let current = Path::new(r"C:\Tools\uu-background.exe");
        assert_eq!(
            super::authorization_executable(current).unwrap(),
            Path::new(r"C:\Tools\uu.exe")
        );
        let normal = Path::new(r"C:\Tools\UU.EXE");
        assert_eq!(super::authorization_executable(normal).unwrap(), normal);
    }
}
