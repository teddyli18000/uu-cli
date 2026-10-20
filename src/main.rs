use anyhow::{bail, Context, Result};
#[cfg(windows)]
use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;
use uu_cli::bind::{decide_binding, BindCandidate, BindDecision};
use uu_cli::cli::{self, Command};
use uu_cli::config::ConfigStore;
use uu_cli::inventory::{discover_inventory, is_eligible_session, InventorySnapshot};
use uu_cli::job::argv_to_shell;
use uu_cli::model::{Config, SessionInfo, WorkspaceBinding};
#[cfg(windows)]
use uu_cli::transport::OfficialAttachPty;
use uu_cli::uuyc::OfficialCli;
#[cfg(windows)]
use uu_cli::worker_lease::{AcquireResult, WorkerLease};
use uu_cli::workspace::{resolve_binding, suggest_root};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("uu: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<u8> {
    let command = cli::parse(env::args_os().skip(1)).map_err(anyhow::Error::new)?;
    let store = ConfigStore::from_env()?;
    let mut config = store.load()?;
    let cwd = env::current_dir().context("cannot determine current directory")?;

    match command {
        Command::List => {
            run_list(&config, &cwd)?;
            Ok(0)
        }
        Command::Bind => {
            run_bind(&store, &mut config, &cwd)?;
            Ok(0)
        }
        Command::Put { .. } | Command::Get { .. } => {
            bail!("file transfer is not implemented in this bootstrap build")
        }
        Command::Run { argv } => {
            ensure_binding(&store, &mut config, &cwd)?;
            run_remote(&store, &mut config, &cwd, argv)
        }
    }
}

fn run_list(config: &Config, cwd: &Path) -> Result<()> {
    let suggested = suggest_root(cwd)?;
    let binding = resolve_binding(config, cwd);

    if let Some(binding) = binding {
        println!("Workspace  {}", binding.local_root.display());
        println!(
            "Remote     {} · {}",
            binding.device_name, binding.remote_root
        );
    } else {
        println!("Workspace  {}", suggested.display());
        println!("Remote     unbound");
    }

    let cli = match OfficialCli::locate() {
        Ok(cli) => cli,
        Err(_) => {
            println!("UU         not found");
            println!("Workers    unavailable");
            return Ok(());
        }
    };

    match cli.version() {
        Ok(version) => {
            let status = version.message.as_deref().unwrap_or("available");
            println!("UU         {} · {}", version.cli_version, status);
        }
        Err(error) => {
            println!("UU         found · incompatible ({error})");
            println!("Workers    unavailable");
            return Ok(());
        }
    }

    let snapshot = match discover_inventory(&cli) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            println!("Workers    discovery failed ({error})");
            return Ok(());
        }
    };

    if let Some(binding) = binding {
        if let Some(device) = snapshot
            .devices
            .iter()
            .find(|item| item.device.device_identity == binding.device_identity)
        {
            if let Some(error) = &device.session_error {
                println!("Workers    unavailable ({error})");
                return Ok(());
            }
        }
        let sessions = snapshot.live_sessions_for(&binding.device_identity);
        print_bound_workers(config, &sessions);
        return Ok(());
    }

    let mut total = 0usize;
    let mut device_count = 0usize;
    for device in snapshot.devices.iter().filter(|item| item.device.is_online) {
        let count = device
            .sessions
            .iter()
            .filter(|session| uu_cli::inventory::is_live_session(session))
            .count();
        if count == 0 {
            continue;
        }
        total += count;
        device_count += 1;
        let name = if device.device.device_name.trim().is_empty() {
            "Unnamed device"
        } else {
            &device.device.device_name
        };
        println!("  {name} · {count} terminal(s)");
    }
    println!("Workers    {total} open terminals on {device_count} device(s)");
    Ok(())
}

fn print_bound_workers(config: &Config, sessions: &[&SessionInfo]) {
    let authorized = sessions
        .iter()
        .filter(|session| config.is_session_authorized(session))
        .count();
    let ready = sessions
        .iter()
        .filter(|session| config.is_session_authorized(session) && is_eligible_session(session))
        .count();
    println!(
        "Workers    {ready} ready / {authorized} authorized / {} open",
        sessions.len()
    );
    for session in sessions {
        let status = if !config.is_session_authorized(session) {
            "not authorized"
        } else if is_eligible_session(session) {
            "ready"
        } else {
            "busy"
        };
        println!("  {} · {status}", session.session_name);
    }
}

fn ensure_binding(store: &ConfigStore, config: &mut Config, cwd: &Path) -> Result<()> {
    if resolve_binding(config, cwd).is_some() {
        return Ok(());
    }

    #[cfg(windows)]
    {
        launch_binding_console(cwd)?;
        *config = store.load()?;
        if resolve_binding(config, cwd).is_none() {
            bail!("workspace binding was not completed");
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        let root = suggest_root(cwd)?;
        let _ = store;
        bail!(
            "workspace {} is not bound; run `uu bind` on Windows first",
            root.display()
        )
    }
}

#[cfg(windows)]
fn launch_binding_console(cwd: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;

    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

    let executable = env::current_exe().context("cannot locate the running uu executable")?;
    eprintln!("Workspace is not bound. Opening a short uu setup window...");
    let status = std::process::Command::new(executable)
        .arg("bind")
        .current_dir(cwd)
        .creation_flags(CREATE_NEW_CONSOLE)
        .status()
        .context("cannot open the uu binding console")?;
    if !status.success() {
        bail!("uu setup was cancelled or failed ({status})");
    }
    Ok(())
}

fn run_bind(store: &ConfigStore, config: &mut Config, cwd: &Path) -> Result<()> {
    let local_root = suggest_root(cwd)?;
    let cli = OfficialCli::locate().context("official UU Remote is required for `uu bind`")?;
    let (snapshot, candidate) = wait_for_bind_target(&cli)?;

    let previous_root = resolve_binding(config, &local_root)
        .filter(|binding| binding.device_identity == candidate.device_identity)
        .map(|binding| binding.remote_root.clone());

    println!(
        "Target     {} · {} open terminal(s)",
        candidate.device_name, candidate.open_sessions
    );
    let remote_root = prompt_remote_root(previous_root.as_deref())?;

    let sessions = snapshot.live_sessions_for(&candidate.device_identity);
    let stable_sessions: Vec<&SessionInfo> = sessions
        .into_iter()
        .filter(|session| session.authorization_fingerprint().is_some())
        .collect();
    if stable_sessions.is_empty() {
        bail!("the open Terminal sessions do not expose a stable identity; refusing to persist an unsafe worker authorization");
    }

    let mut authorized = 0usize;
    for session in stable_sessions {
        if config.authorize_session(session) {
            authorized += 1;
        }
    }

    config.upsert_binding(WorkspaceBinding {
        local_root: local_root.clone(),
        device_identity: candidate.device_identity,
        device_name: candidate.device_name.clone(),
        remote_root: remote_root.clone(),
    });
    store.save(config)?;

    println!("Bound      {}", local_root.display());
    println!("Remote     {} · {remote_root}", candidate.device_name);
    println!("Workers    {authorized} authorized");
    Ok(())
}

fn wait_for_bind_target(cli: &OfficialCli) -> Result<(InventorySnapshot, BindCandidate)> {
    let mut announced_wait = false;

    loop {
        let snapshot = discover_inventory(cli)?;
        match decide_binding(&snapshot) {
            BindDecision::WaitForTerminal { online_devices } => {
                if !announced_wait {
                    eprintln!(
                        "No UU Terminal is open ({online_devices} online device(s)). Open the target Terminal in official UU Remote; uu is waiting. Ctrl+C cancels."
                    );
                    announced_wait = true;
                }
                thread::sleep(Duration::from_millis(750));
            }
            BindDecision::OneDevice(candidate) => return Ok((snapshot, candidate)),
            BindDecision::ChooseDevice(candidates) => {
                let selected = choose_candidate(&candidates)?;
                return Ok((snapshot, candidates[selected].clone()));
            }
        }
    }
}

fn choose_candidate(candidates: &[BindCandidate]) -> Result<usize> {
    println!("Open UU Terminals:");
    for (index, candidate) in candidates.iter().enumerate() {
        println!(
            "  {}) {} · {} terminal(s)",
            index + 1,
            candidate.device_name,
            candidate.open_sessions
        );
    }

    loop {
        let input = prompt("Select target: ")?;
        let Ok(choice) = input.parse::<usize>() else {
            eprintln!("Enter a number from 1 to {}.", candidates.len());
            continue;
        };
        if (1..=candidates.len()).contains(&choice) {
            return Ok(choice - 1);
        }
        eprintln!("Enter a number from 1 to {}.", candidates.len());
    }
}

fn prompt_remote_root(previous: Option<&str>) -> Result<String> {
    loop {
        let label = match previous {
            Some(root) => format!("Remote root [{root}]: "),
            None => "Remote root: ".to_owned(),
        };
        let input = prompt(&label)?;
        if !input.is_empty() {
            return Ok(input);
        }
        if let Some(root) = previous {
            return Ok(root.to_owned());
        }
        eprintln!("Enter the remote project directory, for example /home/pi/aic.");
    }
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush().context("cannot flush prompt")?;
    let mut input = String::new();
    let bytes = io::stdin()
        .read_line(&mut input)
        .context("cannot read selection")?;
    if bytes == 0 {
        bail!("input closed before selection was completed");
    }
    Ok(input.trim().to_owned())
}

fn run_remote(
    store: &ConfigStore,
    config: &mut Config,
    cwd: &Path,
    argv: Vec<OsString>,
) -> Result<u8> {
    let binding = resolve_binding(config, cwd)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("workspace binding disappeared during setup"))?;
    let shell_command = argv_to_shell(&argv)?;

    #[cfg(not(windows))]
    {
        let _ = (store, config, binding, shell_command);
        bail!("remote execution is supported only on Windows in v1")
    }

    #[cfg(windows)]
    {
        let cli =
            OfficialCli::locate().context("official UU Remote is required for remote execution")?;
        let initial = discover_inventory(&cli)?;
        validate_bound_device(&initial, &binding)?;
        let mut seen = initial
            .live_sessions_for(&binding.device_identity)
            .into_iter()
            .map(session_instance_key)
            .collect::<HashSet<_>>();
        let mut local_wait_announced = false;

        loop {
            let workers = wait_for_available_workers(&cli, store, config, &binding, &mut seen)?;
            let mut attach_errors = Vec::new();
            let mut blocked_locally = false;

            for worker in &workers {
                let mut lease = match WorkerLease::try_acquire(store.path(), worker)? {
                    AcquireResult::Acquired(lease) => lease,
                    AcquireResult::Busy | AcquireResult::Quarantined => {
                        blocked_locally = true;
                        continue;
                    }
                };

                let mut transport = match OfficialAttachPty::attach(
                    cli.path(),
                    &binding.device_identity,
                    &worker.session_name,
                ) {
                    Ok(transport) => transport,
                    Err(error) => {
                        attach_errors.push(format!("{}: {error:#}", worker.session_name));
                        continue;
                    }
                };

                lease.mark_in_flight()?;
                let stdout = io::stdout();
                let mut output = stdout.lock();
                match transport.run_job(&mut output, &binding.remote_root, &shell_command) {
                    Ok(result) => {
                        lease.complete()?;
                        return u8::try_from(result.exit_code).with_context(|| {
                            format!(
                                "remote shell returned invalid exit code {}",
                                result.exit_code
                            )
                        });
                    }
                    Err(error) if error.is_unknown() => {
                        bail!(
                            "{error}; not retrying on another Terminal because the command may already be running"
                        );
                    }
                    Err(error) => {
                        lease.complete()?;
                        return Err(anyhow::Error::new(error));
                    }
                }
            }

            if blocked_locally {
                if !local_wait_announced {
                    eprintln!(
                        "All ready uu-cli Terminals on {} are already in use or quarantined. uu is waiting; open another Terminal for more capacity. Ctrl+C cancels.",
                        binding.device_name
                    );
                    local_wait_announced = true;
                }
                thread::sleep(Duration::from_millis(750));
                continue;
            }

            bail!(
                "no authorized Terminal could be attached on {}: {}",
                binding.device_name,
                attach_errors.join(" | ")
            )
        }
    }
}

#[cfg(windows)]
fn wait_for_available_workers(
    cli: &OfficialCli,
    store: &ConfigStore,
    config: &mut Config,
    binding: &WorkspaceBinding,
    seen: &mut HashSet<String>,
) -> Result<Vec<SessionInfo>> {
    let mut announced_wait = false;

    loop {
        let snapshot = discover_inventory(cli)?;
        validate_bound_device(&snapshot, binding)?;

        let live = snapshot.live_sessions_for(&binding.device_identity);
        let mut changed = false;
        for session in live {
            let key = session_instance_key(session);
            if !seen.insert(key) {
                continue;
            }
            if session.authorization_fingerprint().is_some() && config.authorize_session(session) {
                changed = true;
            }
        }
        if changed {
            store.save(config)?;
        }

        let ready = snapshot
            .eligible_sessions_for(&binding.device_identity)
            .into_iter()
            .filter(|session| config.is_session_authorized(session))
            .cloned()
            .collect::<Vec<_>>();
        if !ready.is_empty() {
            return Ok(ready);
        }

        if !announced_wait {
            let authorized_live = snapshot
                .live_sessions_for(&binding.device_identity)
                .into_iter()
                .any(|session| config.is_session_authorized(session));
            if authorized_live {
                eprintln!(
                    "All uu-cli Terminals on {} are busy. uu is waiting; open another Terminal in official UU Remote for more capacity. Ctrl+C cancels.",
                    binding.device_name
                );
            } else {
                eprintln!(
                    "No uu-cli Terminal is available on {}. Open a new Terminal in official UU Remote now; uu will use it for this request. Ctrl+C cancels.",
                    binding.device_name
                );
            }
            announced_wait = true;
        }

        thread::sleep(Duration::from_millis(750));
    }
}

#[cfg(windows)]
fn validate_bound_device(snapshot: &InventorySnapshot, binding: &WorkspaceBinding) -> Result<()> {
    let device = snapshot
        .devices
        .iter()
        .find(|item| item.device.device_identity == binding.device_identity)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "bound target {} is not present in the current UU device list",
                binding.device_name
            )
        })?;
    if !device.device.is_online {
        bail!("bound target {} is offline", binding.device_name);
    }
    if let Some(error) = &device.session_error {
        bail!(
            "cannot query terminal workers on {}: {error}",
            binding.device_name
        );
    }
    Ok(())
}

#[cfg(windows)]
fn session_instance_key(session: &SessionInfo) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        session.device_identity,
        session.session_name,
        session.created_at.as_deref().unwrap_or("")
    )
}
