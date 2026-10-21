use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::env;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::Duration;
use uu_cli::cli::Command;
use uu_cli::config::ConfigStore;
use uu_cli::inventory::{
    discover_inventory, is_eligible_session, is_live_session, InventorySnapshot,
};
use uu_cli::job::argv_to_shell;
use uu_cli::job_queue::QueueTicket;
use uu_cli::model::{SessionInfo, WorkspaceBinding};
use uu_cli::runner::{JobInput, JobRunError};
use uu_cli::uuyc::OfficialCli;
use uu_cli::watcher;
use uu_cli::worker_host::{self, WorkerClient};
use uu_cli::worker_lease::{AcquireResult, WorkerLease};
use uu_cli::workspace::{resolve_binding, suggest_root};

pub fn internal(mode: &str) -> Result<u8> {
    match mode {
        // Compatibility for a legacy watcher launch; no discovery or UI.
        "__watch" => Ok(0),
        _ => bail!("internal setup is disabled; the human must run uu bind in their terminal"),
    }
}

pub fn execute(command: Command, store: &ConfigStore) -> Result<u8> {
    let cwd = env::current_dir()?;
    if matches!(command, Command::List) {
        return list(store, &cwd);
    }
    if matches!(command, Command::Bind) {
        setup(store)?;
        return Ok(0);
    }
    let binding = resolve_binding(&store.load()?, &cwd)
        .cloned()
        .ok_or_else(|| {
            anyhow!("workspace is unbound; ask the human to run uu bind in this workspace")
        })?;

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
                // Ordinary jobs never read binding decisions from caller stdin.

                thread::spawn(move || {
                    let mut input = io::stdin().lock();
                    let mut buffer = [0u8; 8192];
                    loop {
                        match input.read(&mut buffer) {
                            Ok(0) => {
                                let _ = sender.send(JobInput::Eof);
                                break;
                            }
                            Ok(count) => {
                                if sender
                                    .send(JobInput::Bytes(buffer[..count].to_vec()))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(_) => {
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
        bail!("binding input closed");
    }
    Ok(line.trim().to_owned())
}

fn setup(store: &ConfigStore) -> Result<()> {
    let _lock = interaction_lock(store)?;
    let cwd = env::current_dir()?;
    let local_root = uu_cli::workspace::canonical(&cwd)?;
    let cli = OfficialCli::locate()?;
    let config = store.load()?;
    let snapshot = discover_inventory(&cli)?;
    let mut candidates = sessions(&snapshot)?;
    // Never clear quarantine or probe a possibly unfinished job during binding.
    let mut safe = Vec::new();
    for session in candidates.drain(..) {
        match WorkerLease::try_acquire(store.path(), &session)? {
            AcquireResult::Quarantined => {}
            AcquireResult::Acquired(lease) => {
                drop(lease);
                safe.push(session);
            }
            AcquireResult::Busy if config.is_session_authorized(&session) => safe.push(session),
            AcquireResult::Busy => {}
        }
    }
    let new_sessions: Vec<_> = safe
        .iter()
        .filter(|session| !config.is_session_authorized(session))
        .cloned()
        .collect();
    if new_sessions.is_empty() {
        bail!("no available new dedicated Terminal; open one in official UU, close its tab without typing exit, then run uu bind again; unknown workers remain quarantined");
    }
    if new_sessions.len() != 1 {
        bail!("ambiguous Terminal hand-off: expected exactly one new live detached Terminal; leave only the intended Terminal available, then run uu bind again");
    }
    let session = &new_sessions[0];
    let device_id = session.device_identity.clone();
    let device_name = session.device_name.clone();
    // Running bind is the human's explicit hand-off; no redundant accept prompt.
    store.update(|config| {
        config.accept_session(session);
        Ok(())
    })?;
    let config = store.load()?;
    let old_root = config
        .bindings
        .iter()
        .find(|binding| {
            uu_cli::workspace::same_path(&binding.local_root, &local_root)
                && binding.device_identity == device_id
        })
        .map(|b| b.remote_root.clone());
    if let Some(root) = old_root {
        println!(
            "已交给 CLI：{} · {}；保留远端目录 {}",
            device_name, session.session_name, root
        );
        return Ok(());
    }
    let default_root = capture_pwd(store, &cli, session)?;
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

fn capture_pwd(store: &ConfigStore, cli: &OfficialCli, selected: &SessionInfo) -> Result<String> {
    let device = selected.device_identity.as_str();
    let fingerprint = selected.authorization_fingerprint();
    loop {
        let config = store.load()?;
        let snapshot = discover_inventory(cli)?;
        let Some(device_inventory) = snapshot
            .devices
            .iter()
            .find(|entry| entry.device.device_identity == device)
        else {
            bail!(
                "authorized Terminal device disappeared; open a new Terminal and run uu bind again"
            );
        };
        if !device_inventory.device.is_online {
            bail!(
                "authorized Terminal device is offline; open a new Terminal and run uu bind again"
            );
        }
        if let Some(error) = &device_inventory.session_error {
            bail!("cannot discover authorized Terminal: {error}");
        }
        let current: Vec<_> = device_inventory
            .sessions
            .iter()
            .filter(|session| {
                is_live_session(session) && session.authorization_fingerprint() == fingerprint
            })
            .cloned()
            .collect();
        let helper_fingerprints: HashSet<_> = worker_host::live_sessions(store)?
            .into_iter()
            .filter(|helper| helper.device_identity == device)
            .filter_map(|helper| helper.authorization_fingerprint())
            .collect();
        let mut ordered = Vec::new();
        let mut seen = HashSet::new();
        // Use a current inventory record, never metadata contents, for the
        // helper's name/state/instance and require authorization below.
        for session in &current {
            let Some(fingerprint) = session.authorization_fingerprint() else {
                continue;
            };
            if helper_fingerprints.contains(&fingerprint) && config.is_session_authorized(session) {
                seen.insert(fingerprint);
                ordered.push((session.clone(), true));
            }
        }
        for session in &current {
            let Some(fingerprint) = session.authorization_fingerprint() else {
                continue;
            };
            if is_eligible_session(session)
                && config.is_session_authorized(session)
                && seen.insert(fingerprint)
            {
                ordered.push((session.clone(), false));
            }
        }

        let mut quarantined = false;
        for (session, has_helper) in ordered {
            let mut lease = match WorkerLease::try_acquire(store.path(), &session)? {
                AcquireResult::Acquired(lease) => lease,
                AcquireResult::Busy => continue,
                AcquireResult::Quarantined => {
                    quarantined = true;
                    continue;
                }
            };
            let mut transport = if has_helper {
                match WorkerClient::try_connect(store, &session)? {
                    Some(transport) => transport,
                    None if is_eligible_session(&session) => {
                        WorkerClient::connect_or_start(store, cli.path(), &session)?
                    }
                    None => continue,
                }
            } else {
                // Revalidate an attachable inventory candidate immediately
                // before preparing a new helper connection.
                let latest = discover_inventory(cli)?;
                let Some(latest_device) = latest
                    .devices
                    .iter()
                    .find(|entry| entry.device.device_identity == device && entry.device.is_online)
                else {
                    bail!("authorized Terminal device disappeared; open a new Terminal and run uu bind again");
                };
                if !latest_device.sessions.iter().any(|now| {
                    is_eligible_session(now)
                        && now.authorization_fingerprint() == session.authorization_fingerprint()
                }) {
                    continue;
                }
                WorkerClient::connect_or_start(store, cli.path(), &session)?
            };
            lease.mark_in_flight()?;
            let result = transport.capture_pwd();
            drop(transport);
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
        if !current
            .iter()
            .any(|session| config.is_session_authorized(session))
        {
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
    operation: impl FnOnce(&mut WorkerClient) -> std::result::Result<T, JobRunError>,
) -> Result<T> {
    let ticket = QueueTicket::join(store.path(), &binding.device_identity)?;
    let mut announced = None;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            bail!("cancelled before dispatch");
        }
        let ahead = ticket.position()?;
        if ahead > 0 {
            if announced != Some(ahead) {
                eprintln!(
                    "排队等待 {}：前面还有 {} 个等待中的命令。Ctrl+C 取消本次排队。",
                    binding.device_name, ahead
                );
                announced = Some(ahead);
            }
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        // Live helpers own the exact authorized attachment. Routing a new job
        // through one does not need another remote list/connect/attach cycle.
        // A stale cache entry never grants authority or triggers dispatch.
        let config = store.load()?;
        let mut busy_authorized = false;
        for session in worker_host::live_sessions(store)?.iter().filter(|session| {
            session.device_identity == binding.device_identity
                && config.is_session_authorized(session)
        }) {
            let lease = match WorkerLease::try_acquire(store.path(), session)? {
                AcquireResult::Acquired(lease) => lease,
                AcquireResult::Busy => {
                    busy_authorized = true;
                    continue;
                }
                AcquireResult::Quarantined => continue,
            };
            if let Some(transport) = WorkerClient::try_connect(store, session)? {
                drop(ticket);
                return dispatch(lease, transport, cancelled, operation);
            }
        }
        let cli = OfficialCli::locate()?;
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
            let lease = match WorkerLease::try_acquire(store.path(), session)? {
                AcquireResult::Acquired(lease) => lease,
                AcquireResult::Busy => {
                    busy_authorized = true;
                    continue;
                }
                AcquireResult::Quarantined => continue,
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
            let transport = WorkerClient::connect_or_start(store, cli.path(), session)?;
            drop(ticket);
            return dispatch(lease, transport, cancelled, operation);
        }
        if !busy_authorized {
            bail!("no authorized available Terminal for {}; ask the human to open a dedicated Terminal, close its tab, and run uu bind; unknown/may-still-be-running workers are not reused", binding.device_name);
        }
        if announced != Some(0) {
            eprintln!("排队等待 {}：前面还有 0 个等待中的命令，全部已绑定 Terminal 正忙；任务完成后自动执行。Ctrl+C 取消本次排队。", binding.device_name);
            announced = Some(0);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn dispatch<T>(
    mut lease: WorkerLease,
    mut transport: WorkerClient,
    cancelled: &AtomicBool,
    operation: impl FnOnce(&mut WorkerClient) -> std::result::Result<T, JobRunError>,
) -> Result<T> {
    if cancelled.load(Ordering::SeqCst) {
        bail!("cancelled before dispatch");
    }
    lease.mark_in_flight()?;
    // The sole public job execution site: no retry or worker switch afterwards.
    let result = operation(&mut transport);
    // Close this IPC client before making the worker lease available again.
    drop(transport);
    match result {
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
    }
}

fn list(store: &ConfigStore, cwd: &Path) -> Result<u8> {
    let config = store.load()?;
    println!("Workspace  {}", suggest_root(cwd)?.display());
    println!("Config     {}", store.path().display());
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
