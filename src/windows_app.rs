use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::env;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command as ProcessCommand;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::Duration;
use uu_cli::authorization::{
    binding_candidates, classify_session, fingerprint_baseline, SessionDecision,
};
use uu_cli::cli::Command;
use uu_cli::config::ConfigStore;
use uu_cli::inventory::{discover_inventory, is_eligible_session, InventorySnapshot};
use uu_cli::job::argv_to_shell;
use uu_cli::model::{SessionInfo, WorkspaceBinding};
use uu_cli::runner::{JobInput, JobRunError};
use uu_cli::transport::OfficialAttachPty;
use uu_cli::uuyc::OfficialCli;
use uu_cli::watcher;
use uu_cli::worker_lease::{AcquireResult, WorkerLease};
use uu_cli::workspace::{resolve_binding, suggest_root};

const CREATE_NEW_CONSOLE: u32 = 0x10;

pub fn internal(mode: &str) -> Result<u8> {
    if mode != "__watch" {
        decision_console()?;
    }
    let store = ConfigStore::from_env()?;
    match mode {
        "__watch" => {
            watcher::run(store.path())?;
        }
        "__setup" => {
            setup(&store, env::args().nth(2).as_deref() == Some("force"))?;
        }
        "__authorize" => {
            authorize(&store, env::args().nth(2).as_deref())?;
        }
        _ => bail!("unknown internal mode"),
    }
    Ok(0)
}

pub fn execute(command: Command, store: &ConfigStore) -> Result<u8> {
    let cwd = env::current_dir()?;
    if matches!(command, Command::List) {
        return list(store, &cwd);
    }
    let exe = env::current_exe()?;
    // A failed background launch must be visible; the foreground command can
    // still complete without relying on a watcher for safety.
    if let Err(error) = watcher::spawn_if_needed(&exe, store.path()) {
        eprintln!("uu: automatic Terminal discovery unavailable: {error:#}");
    }
    if matches!(command, Command::Bind) {
        launch_console("__setup", Some("force"))?;
        return Ok(0);
    }
    if resolve_binding(&store.load()?, &cwd).is_none() {
        // Fail with a persistent caller diagnostic before opening a decision window.
        OfficialCli::locate()?.list_devices()?;
        launch_console("__setup", None)?;
    }
    let binding = resolve_binding(&store.load()?, &cwd)
        .cloned()
        .ok_or_else(|| anyhow!("workspace binding was cancelled or not completed"))?;

    let (sender, receiver) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = cancelled.clone();
    let interrupt_sender = sender.clone();
    ctrlc::set_handler(move || {
        signal.store(true, Ordering::SeqCst);
        let _ = interrupt_sender.send(JobInput::Interrupt);
    })
    .context("cannot install Ctrl+C handler")?;

    match command {
        Command::Run { argv } => {
            let shell = argv_to_shell(&argv)?;
            let remote_root = binding.remote_root.clone();
            let result = with_worker(store, &binding, &cancelled, move |transport| {
                // Only the job reader consumes caller stdin. Setup/selection is
                // always hosted in a separate console.
                let fixture_trace = std::env::var("UU_TEST_MODE").as_deref() == Ok("bash")
                    && std::env::var_os("UU_TEST_FIXTURE_EVENTS").is_some();
                thread::spawn(move || {
                    if fixture_trace {
                        eprintln!("uu-test-stdin-reader-start");
                    }
                    let mut input = io::stdin().lock();
                    let mut buffer = [0u8; 8192];
                    loop {
                        match input.read(&mut buffer) {
                            Ok(0) => {
                                if fixture_trace {
                                    eprintln!("uu-test-stdin-reader-eof");
                                }
                                let _ = sender.send(JobInput::Eof);
                                break;
                            }
                            Ok(count) => {
                                if fixture_trace {
                                    let hex = buffer[..count]
                                        .iter()
                                        .map(|byte| format!("{byte:02x}"))
                                        .collect::<String>();
                                    eprintln!("uu-test-stdin-reader-bytes hex={hex}");
                                }
                                if sender
                                    .send(JobInput::Bytes(buffer[..count].to_vec()))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error) => {
                                if fixture_trace {
                                    eprintln!("uu-test-stdin-reader-error={error}");
                                }
                                let _ = sender.send(JobInput::Eof);
                                break;
                            }
                        }
                    }
                });
                transport.run_job_with_input(
                    &mut io::stdout().lock(),
                    &remote_root,
                    &shell,
                    &receiver,
                )
            })?;
            u8::try_from(result.exit_code).context("remote exit status outside 0..255")
        }
        Command::Put { local, remote } => {
            let remote = remote
                .to_str()
                .ok_or_else(|| anyhow!("remote path must be valid Unicode"))?;
            let receipt = with_worker(store, &binding, &cancelled, |transport| {
                let mut executor = |command: &str, output: &mut dyn Write| {
                    if cancelled.load(Ordering::SeqCst) {
                        return Err(JobRunError::Completed(anyhow!(
                            "transfer cancelled between completed chunks"
                        )));
                    }
                    transport.run_job_with_input(output, &binding.remote_root, command, &receiver)
                };
                uu_cli::transfer::put(Path::new(&local), remote, &mut executor)
            })?;
            eprintln!(
                "Uploaded {} bytes · SHA256 {}",
                receipt.size, receipt.sha256
            );
            Ok(0)
        }
        Command::Get { remote, local } => {
            let remote = remote
                .to_str()
                .ok_or_else(|| anyhow!("remote path must be valid Unicode"))?;
            let receipt = with_worker(store, &binding, &cancelled, |transport| {
                let mut executor = |command: &str, output: &mut dyn Write| {
                    if cancelled.load(Ordering::SeqCst) {
                        return Err(JobRunError::Completed(anyhow!(
                            "transfer cancelled between completed chunks"
                        )));
                    }
                    transport.run_job_with_input(output, &binding.remote_root, command, &receiver)
                };
                uu_cli::transfer::get(remote, Path::new(&local), &mut executor)
            })?;
            eprintln!(
                "Downloaded {} bytes · SHA256 {}",
                receipt.size, receipt.sha256
            );
            Ok(0)
        }
        _ => unreachable!(),
    }
}

fn launch_console(mode: &str, argument: Option<&str>) -> Result<()> {
    let mut command = ProcessCommand::new(env::current_exe()?);
    command.arg(mode).creation_flags(CREATE_NEW_CONSOLE);
    if let Some(argument) = argument {
        command.arg(argument);
    }
    let status = command
        .status()
        .context("cannot open the short uu decision window")?;
    if !status.success() {
        bail!("uu setup was cancelled or failed");
    }
    Ok(())
}

fn interaction_lock(store: &ConfigStore) -> Result<File> {
    loop {
        if let Some(lock) = watcher::try_interaction_lock(store.path())? {
            return Ok(lock);
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn sessions(snapshot: &InventorySnapshot) -> Result<Vec<SessionInfo>> {
    let eligible: Vec<_> = snapshot.eligible_sessions().into_iter().cloned().collect();
    check_instance_support(&eligible)?;
    Ok(eligible
        .into_iter()
        .filter(|s| s.authorization_fingerprint().is_some())
        .collect())
}

fn check_instance_support(current: &[SessionInfo]) -> Result<()> {
    let eligible: Vec<_> = current.iter().filter(|s| is_eligible_session(s)).collect();
    if !eligible.is_empty()
        && eligible
            .iter()
            .all(|s| s.authorization_fingerprint().is_none())
    {
        bail!("official UU Terminal instance identity is unavailable (createdAt/device/name); this client cannot safely authorize these Terminals");
    }
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line)? == 0 {
        bail!("decision window input closed");
    }
    Ok(line.trim().to_owned())
}

fn accept_one(store: &ConfigStore, session: &SessionInfo) -> Result<bool> {
    // Persist default-deny before showing the question. Esc/close/crash therefore
    // cannot cause the same Terminal instance to prompt again or become eligible.
    store.update(|config| {
        config.decline_session(session);
        Ok(())
    })?;
    println!(
        "{} 新终端\n[Enter] 交给 uu-cli  [N/Esc] 保留手动使用",
        session.device_name
    );
    if read_accept_key()? {
        store.update(|config| {
            config.accept_session(session);
            Ok(())
        })?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn authorize(store: &ConfigStore, device: Option<&str>) -> Result<()> {
    // A child that lost a race must simply close, not queue another question.
    let Some(_lock) = watcher::try_interaction_lock(store.path())? else {
        return Ok(());
    };
    let cli = OfficialCli::locate()?;
    let snapshot = discover_inventory(&cli)?;
    for session in sessions(&snapshot)?
        .into_iter()
        .filter(|s| device.map_or(true, |id| s.device_identity == id))
    {
        let config = store.load()?;
        if !config.is_session_authorized(&session) && !config.is_session_declined(&session) {
            accept_one(store, &session)?;
        }
    }
    Ok(())
}

fn choose_device(candidates: &[SessionInfo]) -> Result<String> {
    let mut devices = Vec::<(String, String)>::new();
    for candidate in candidates {
        if !devices
            .iter()
            .any(|(id, _)| id == &candidate.device_identity)
        {
            devices.push((
                candidate.device_identity.clone(),
                candidate.device_name.clone(),
            ));
        }
    }
    if devices.len() == 1 {
        return Ok(devices[0].0.clone());
    }
    for (index, (_, name)) in devices.iter().enumerate() {
        if devices[index + 1..].iter().any(|(_, other)| other == name) {
            bail!("多个设备具有相同名称；请仅保留目标设备的 Terminal 后重试，uu 不会猜测设备");
        }
    }
    println!("请选择远端设备：");
    for (index, (_, name)) in devices.iter().enumerate() {
        // Ordinals are used only for this explicit human decision.
        println!(
            "  {}) {}",
            index + 1,
            if name.is_empty() {
                "未命名设备"
            } else {
                name
            }
        );
    }
    loop {
        let answer = prompt("设备编号 (Ctrl+C取消): ")?;
        if answer.contains('\u{1b}') {
            bail!("device selection cancelled");
        }
        if let Ok(index) = answer.parse::<usize>() {
            if index > 0 && index <= devices.len() {
                return Ok(devices[index - 1].0.clone());
            }
        }
    }
}

fn setup(store: &ConfigStore, force: bool) -> Result<()> {
    let _lock = interaction_lock(store)?;
    let cwd = env::current_dir()?;
    let local_root = suggest_root(&cwd)?;
    if !force && resolve_binding(&store.load()?, &cwd).is_some() {
        return Ok(());
    }
    let cli = OfficialCli::locate()?;
    let initial = sessions(&discover_inventory(&cli)?)?;
    let mut baseline = fingerprint_baseline(&initial);
    let mut waiting = false;
    let (device_id, device_name) = loop {
        let snapshot = discover_inventory(&cli)?;
        let current = sessions(&snapshot)?;
        let config = store.load()?;
        let candidates = binding_candidates(
            &current,
            &config.authorized_workers,
            &config.declined_workers,
            waiting.then_some(&baseline),
        );
        if candidates.is_empty() {
            if !waiting {
                println!("请在官方 UU 中打开目标设备的 Terminal；接下来打开的 Terminal 将用于本次请求。Ctrl+C取消。");
                baseline = fingerprint_baseline(&current);
                waiting = true;
            }
            thread::sleep(Duration::from_secs(2));
            continue;
        }
        let selected = choose_device(&candidates)?;
        let selected_sessions: Vec<_> = candidates
            .iter()
            .filter(|s| s.device_identity == selected)
            .collect();
        let name = selected_sessions[0].device_name.clone();
        let mut accepted = selected_sessions
            .iter()
            .any(|s| config.is_session_authorized(s));
        for session in selected_sessions {
            if config.is_session_authorized(session) {
                continue;
            }
            if waiting
                && classify_session(
                    session,
                    &config.authorized_workers,
                    &config.declined_workers,
                    Some(&baseline),
                ) == SessionDecision::AutoAuthorizeForPendingRequest
            {
                store.update(|config| {
                    config.authorize_session(session);
                    Ok(())
                })?;
                accepted = true;
            } else if !accepted && accept_one(store, session)? {
                accepted = true;
            }
        }
        if accepted {
            break (selected, name);
        }
        waiting = false;
    };
    let config = store.load()?;
    let old_root = resolve_binding(&config, &local_root)
        .filter(|binding| binding.device_identity == device_id)
        .map(|b| b.remote_root.clone());
    let default_root = if let Some(root) = old_root {
        root
    } else {
        capture_pwd(store, &cli, &device_id)?
    };
    let remote_root = loop {
        let answer = prompt(&format!("Remote root [{default_root}] (Ctrl+C取消): "))?;
        if answer.contains('\u{1b}') {
            bail!("binding cancelled");
        }
        let root = if answer.is_empty() {
            default_root.clone()
        } else {
            answer
        };
        if root.starts_with('/') && !root.contains(['\0', '\r', '\n']) {
            break root;
        }
        println!("请输入远端绝对路径。");
    };
    store.update(|config| {
        config.upsert_binding(WorkspaceBinding {
            local_root: local_root.clone(),
            device_identity: device_id.clone(),
            device_name: device_name.clone(),
            remote_root: remote_root.clone(),
        });
        Ok(())
    })?;
    println!(
        "已绑定 {} → {} · {}",
        local_root.display(),
        device_name,
        remote_root
    );
    Ok(())
}

fn capture_pwd(store: &ConfigStore, cli: &OfficialCli, device: &str) -> Result<String> {
    loop {
        let config = store.load()?;
        let current = cli.list_sessions(device)?;
        let mut quarantined = false;
        for session in current
            .iter()
            .filter(|s| is_eligible_session(s) && config.is_session_authorized(s))
        {
            let mut lease = match WorkerLease::try_acquire(store.path(), session)? {
                AcquireResult::Acquired(lease) => lease,
                AcquireResult::Busy => continue,
                AcquireResult::Quarantined => {
                    quarantined = true;
                    continue;
                }
            };
            let mut transport =
                OfficialAttachPty::attach(cli.path(), device, &session.session_name)?;
            lease.mark_in_flight()?;
            let result = transport.capture_pwd();
            match result {
                Ok(root) => {
                    lease.complete()?;
                    return Ok(root);
                }
                Err(error) => {
                    if !error.is_unknown() {
                        lease.complete()?;
                    }
                    return Err(error.into());
                }
            }
        }
        if quarantined {
            bail!("worker state unknown/may-still-be-running; open a new Terminal in official UU");
        }
        if !current.iter().any(|s| config.is_session_authorized(s)) {
            bail!("authorized Terminal disappeared; open a new Terminal and run uu bind again");
        }
        eprintln!("等待已授权 Terminal 空闲。Ctrl+C取消。");
        thread::sleep(Duration::from_secs(2));
    }
}

fn with_worker<T>(
    store: &ConfigStore,
    binding: &WorkspaceBinding,
    cancelled: &AtomicBool,
    operation: impl FnOnce(&mut OfficialAttachPty) -> std::result::Result<T, JobRunError>,
) -> Result<T> {
    let cli = OfficialCli::locate()?;
    let mut wait_guard: Option<File> = None;
    let mut baseline = HashSet::new();
    let mut announced = false;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            bail!("cancelled before dispatch");
        }
        let devices = cli.list_devices()?;
        let Some(device) = devices
            .iter()
            .find(|d| d.device_identity == binding.device_identity)
        else {
            bail!(
                "bound device {} is unavailable; binding was retained",
                binding.device_name
            );
        };
        if !device.is_online {
            bail!("bound device {} is offline", binding.device_name);
        }
        let current = cli.list_sessions(&binding.device_identity).context(
            "cannot discover Terminal channel; open a Terminal in official UU if needed",
        )?;
        check_instance_support(&current)?;
        let config = store.load()?;
        for session in current
            .iter()
            .filter(|s| is_eligible_session(s) && config.is_session_authorized(s))
        {
            let mut lease = match WorkerLease::try_acquire(store.path(), session)? {
                AcquireResult::Acquired(lease) => lease,
                AcquireResult::Busy | AcquireResult::Quarantined => continue,
            };
            // Recheck instance under our lease immediately before attach.
            if !cli
                .list_sessions(&binding.device_identity)?
                .iter()
                .any(|now| {
                    is_eligible_session(now)
                        && now.authorization_fingerprint() == session.authorization_fingerprint()
                })
            {
                continue;
            }
            let mut transport = OfficialAttachPty::attach(
                cli.path(),
                &binding.device_identity,
                &session.session_name,
            )?;
            drop(wait_guard);
            lease.mark_in_flight()?;
            // This is the only execution site. No branch retries after calling it.
            return match operation(&mut transport) {
                Ok(value) => {
                    lease.complete()?;
                    Ok(value)
                }
                Err(error) => {
                    if !error.is_unknown() {
                        lease.complete()?;
                    }
                    Err(error.into())
                }
            };
        }
        if wait_guard.is_none() {
            if let Some(lock) = watcher::try_interaction_lock(store.path())? {
                let undecided = current.iter().any(|s| {
                    is_eligible_session(s)
                        && s.authorization_fingerprint().is_some()
                        && !config.is_session_authorized(s)
                        && !config.is_session_declined(s)
                });
                if undecided {
                    drop(lock);
                    launch_console("__authorize", Some(&binding.device_identity))?;
                    continue;
                }
                baseline = fingerprint_baseline(&current);
                eprintln!("等待 {} 的可用 Terminal；接下来打开的匹配 Terminal 将用于本次请求。unknown/may-still-be-running 的 worker 不会复用。Ctrl+C取消。", binding.device_name);
                announced = true;
                wait_guard = Some(lock);
            }
        } else if announced {
            let newly_opened: Vec<_> = current
                .iter()
                .filter(|s| {
                    is_eligible_session(s)
                        && classify_session(
                            s,
                            &config.authorized_workers,
                            &config.declined_workers,
                            Some(&baseline),
                        ) == SessionDecision::AutoAuthorizeForPendingRequest
                })
                .cloned()
                .collect();
            if !newly_opened.is_empty() {
                store.update(|config| {
                    for session in &newly_opened {
                        config.authorize_session(session);
                    }
                    Ok(())
                })?;
                continue;
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn list(store: &ConfigStore, cwd: &Path) -> Result<u8> {
    let config = store.load()?;
    println!("Workspace  {}", suggest_root(cwd)?.display());
    let binding = resolve_binding(&config, cwd);
    if let Some(binding) = binding {
        println!(
            "Remote     {} · {}",
            binding.device_name, binding.remote_root
        );
    } else {
        println!("Remote     unbound");
    }
    let cli = match OfficialCli::locate() {
        Ok(cli) => cli,
        Err(error) => {
            println!("UU         not found ({error})");
            return Ok(0);
        }
    };
    let version = match cli.version() {
        Ok(version) => version,
        Err(error) => {
            println!("UU         incompatible ({error})");
            return Ok(0);
        }
    };
    println!(
        "UU         {} · {}",
        version.cli_version,
        version.message.as_deref().unwrap_or("available")
    );
    let snapshot = match discover_inventory(&cli) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            println!("Terminals  unavailable ({error})");
            return Ok(0);
        }
    };
    for device in &snapshot.devices {
        if binding.is_some_and(|b| b.device_identity != device.device.device_identity) {
            continue;
        }
        if let Some(error) = &device.session_error {
            println!("{} · unavailable ({error})", device.device.device_name);
            continue;
        }
        let mut ready = 0;
        let mut busy = 0;
        let mut unknown = 0;
        let mut manual = 0;
        for session in &device.sessions {
            if !config.is_session_authorized(session) {
                manual += 1;
                continue;
            }
            if !is_eligible_session(session) {
                busy += 1;
                continue;
            }
            match WorkerLease::try_acquire(store.path(), session)? {
                AcquireResult::Acquired(lease) => {
                    drop(lease);
                    ready += 1;
                }
                AcquireResult::Busy => busy += 1,
                AcquireResult::Quarantined => unknown += 1,
            }
        }
        println!(
            "{} · {ready} ready · {busy} busy · {unknown} unknown · {manual} manual",
            device.device.device_name
        );
    }
    Ok(0)
}

fn decision_console() -> Result<()> {
    use std::ffi::c_void;
    use std::fs::OpenOptions;
    use std::os::windows::io::IntoRawHandle;
    #[link(name = "kernel32")]
    extern "system" {
        fn SetStdHandle(kind: u32, handle: *mut c_void) -> i32;
        fn SetConsoleOutputCP(code_page: u32) -> i32;
    }
    // A console created by the hidden watcher may inherit null std handles.
    // Reopen only this dedicated decision console, never the caller's job I/O.
    let input = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .context("no interactive console is available for a human decision")?
        .into_raw_handle();
    let output = OpenOptions::new()
        .write(true)
        .open("CONOUT$")?
        .into_raw_handle();
    unsafe {
        if SetConsoleOutputCP(65001) == 0
            || SetStdHandle(-10i32 as u32, input) == 0
            || SetStdHandle(-11i32 as u32, output) == 0
            || SetStdHandle(-12i32 as u32, output) == 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn read_accept_key() -> Result<bool> {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(kind: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
        fn ReadConsoleW(
            handle: *mut c_void,
            buffer: *mut u16,
            count: u32,
            read: *mut u32,
            reserved: *mut c_void,
        ) -> i32;
    }
    let handle = unsafe { GetStdHandle(-10i32 as u32) };
    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        bail!("cannot read human decision from a console");
    }
    struct Restore(*mut c_void, u32);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                SetConsoleMode(self.0, self.1);
            }
        }
    }
    let _restore = Restore(handle, mode);
    // Disable line input and echo; N/Esc cancel immediately without Enter.
    if unsafe { SetConsoleMode(handle, mode & !(0x2 | 0x4)) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut key = 0u16;
    let mut count = 0;
    if unsafe { ReadConsoleW(handle, &mut key, 1, &mut count, std::ptr::null_mut()) } == 0
        || count == 0
    {
        bail!("decision console closed");
    }
    println!();
    Ok(key == 13)
}
