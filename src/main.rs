use anyhow::{bail, Context, Result};
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
use uu_cli::transport::{OfficialAttachPty, OfficialNewPty};
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
            println!("Terminals  unavailable");
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
            println!("Terminals  unavailable");
            return Ok(());
        }
    }

    let snapshot = match discover_inventory(&cli) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            println!("Terminals  discovery failed ({error})");
            return Ok(());
        }
    };

    if let Some(binding) = binding {
        let Some(device) = snapshot
            .devices
            .iter()
            .find(|item| item.device.device_identity == binding.device_identity)
        else {
            println!("Terminals  target unavailable");
            return Ok(());
        };
        if let Some(error) = &device.session_error {
            println!("Terminals  unavailable ({error})");
            return Ok(());
        }
        let sessions = snapshot.live_sessions_for(&binding.device_identity);
        print_bound_terminals(config, &sessions);
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
    println!("Terminals  {total} open on {device_count} device(s)");
    Ok(())
}

fn print_bound_terminals(config: &Config, sessions: &[&SessionInfo]) {
    let owned = sessions
        .iter()
        .filter(|session| config.is_owned_worker(session))
        .count();
    let ready = sessions
        .iter()
        .filter(|session| config.is_owned_worker(session) && is_eligible_session(session))
        .count();
    let anchors = sessions.len().saturating_sub(owned);
    println!("Terminals  {anchors} user/anchor · {owned} uu-cli worker(s) · {ready} ready");
    for session in sessions {
        let status = if config.is_owned_worker(session) {
            if is_eligible_session(session) {
                "uu-cli worker · ready"
            } else {
                "uu-cli worker · busy"
            }
        } else {
            "user/anchor · untouched"
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
    let (_snapshot, candidate) = wait_for_bind_target(&cli)?;

    let previous_root = resolve_binding(config, &local_root)
        .filter(|binding| binding.device_identity == candidate.device_identity)
        .map(|binding| binding.remote_root.clone());

    println!(
        "Target     {} · {} open terminal(s)",
        candidate.device_name, candidate.open_sessions
    );
    let remote_root = prompt_remote_root(previous_root.as_deref())?;

    config.upsert_binding(WorkspaceBinding {
        local_root: local_root.clone(),
        device_identity: candidate.device_identity,
        device_name: candidate.device_name.clone(),
        remote_root: remote_root.clone(),
    });
    store.save(config)?;

    println!("Bound      {}", local_root.display());
    println!("Remote     {} · {remote_root}", candidate.device_name);
    println!("Terminal   existing GUI terminals remain untouched");
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
        let mut activation_hint_printed = false;

        loop {
            let snapshot = discover_inventory(&cli)?;
            let device = bound_device(&snapshot, &binding)?;
            if let Some(error) = &device.session_error {
                if needs_terminal_activation(error) {
                    if !activation_hint_printed {
                        eprintln!(
                            "UU terminal channel is not active on {}. Open one Terminal for this device in official UU Remote; uu is waiting. Ctrl+C cancels.",
                            binding.device_name
                        );
                        activation_hint_printed = true;
                    }
                    thread::sleep(Duration::from_millis(900));
                    continue;
                }
                bail!(
                    "cannot query terminal sessions on {}: {error}",
                    binding.device_name
                );
            }

            let live = snapshot.live_sessions_for(&binding.device_identity);
            if config.forget_missing_owned_workers(&binding.device_identity, live.iter().copied()) {
                store.save(config)?;
            }

            let owned_ready = snapshot
                .eligible_sessions_for(&binding.device_identity)
                .into_iter()
                .filter(|session| config.is_owned_worker(session))
                .cloned()
                .collect::<Vec<_>>();

            for worker in &owned_ready {
                if worker.authorization_fingerprint().is_none() {
                    continue;
                }
                let mut lease = match WorkerLease::try_acquire(store.path(), worker)? {
                    AcquireResult::Acquired(lease) => lease,
                    AcquireResult::Busy | AcquireResult::Quarantined => continue,
                };

                let mut transport = match OfficialAttachPty::attach(
                    cli.path(),
                    &binding.device_identity,
                    &worker.session_name,
                ) {
                    Ok(transport) => transport,
                    Err(_) => continue,
                };

                lease.mark_in_flight()?;
                let stdout = io::stdout();
                let mut output = stdout.lock();
                match transport.run_job(&mut output, &binding.remote_root, &shell_command) {
                    Ok(result) => {
                        lease.complete()?;
                        return normalize_exit_code(result.exit_code);
                    }
                    Err(error) if error.is_unknown() => {
                        bail!("{error}; not retrying because the command may already be running");
                    }
                    Err(error) => {
                        lease.complete()?;
                        return Err(anyhow::Error::new(error));
                    }
                }
            }

            match OfficialNewPty::create(cli.path(), &binding.device_identity) {
                Ok(mut transport) => {
                    let session_name = transport.session_name().to_owned();
                    let stdout = io::stdout();
                    let mut output = stdout.lock();
                    match transport.run_job(&mut output, &binding.remote_root, &shell_command) {
                        Ok(result) => {
                            config.remember_owned_worker(&binding.device_identity, &session_name);
                            store.save(config)?;
                            return normalize_exit_code(result.exit_code);
                        }
                        Err(error) if error.is_unknown() => {
                            bail!(
                                "{error}; not retrying because the command may already be running"
                            );
                        }
                        Err(error) => {
                            let _ = cli.kill_session(&binding.device_identity, &session_name);
                            return Err(anyhow::Error::new(error));
                        }
                    }
                }
                Err(error) if needs_terminal_activation(&format!("{error:#}")) => {
                    if !activation_hint_printed {
                        eprintln!(
                            "UU terminal channel is not active on {}. Open one Terminal for this device in official UU Remote; uu is waiting. Ctrl+C cancels.",
                            binding.device_name
                        );
                        activation_hint_printed = true;
                    }
                    thread::sleep(Duration::from_millis(900));
                }
                Err(error) => {
                    bail!(
                        "cannot create a uu-cli worker on {}: {error:#}",
                        binding.device_name
                    );
                }
            }
        }
    }
}

fn normalize_exit_code(exit_code: i32) -> Result<u8> {
    u8::try_from(exit_code)
        .with_context(|| format!("remote shell returned invalid exit code {exit_code}"))
}

fn needs_terminal_activation(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains("版本过低")
        || text.contains("版本不匹配")
        || text.contains("不再兼容")
        || text.contains("协议版本")
        || text.contains("通道未激活")
        || lower.contains("terminal channel")
        || lower.contains("terminal not active")
        || lower.contains("open terminal")
}

#[cfg(windows)]
fn bound_device<'a>(
    snapshot: &'a InventorySnapshot,
    binding: &WorkspaceBinding,
) -> Result<&'a uu_cli::inventory::DeviceInventory> {
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
    Ok(device)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_observed_channel_activation_failures() {
        assert!(needs_terminal_activation(
            "主控端版本过低，被控端不再兼容此协议版本"
        ));
        assert!(needs_terminal_activation("terminal channel not active"));
        assert!(!needs_terminal_activation(
            "Main application is not running"
        ));
        assert!(!needs_terminal_activation("device is offline"));
    }
}
