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
use uu_cli::model::{Config, SessionInfo, WorkspaceBinding};
use uu_cli::uuyc::OfficialCli;
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
        Command::Run { argv } => run_remote_placeholder(&config, &cwd, argv),
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
        let sessions = snapshot.eligible_sessions_for(&binding.device_identity);
        print_bound_workers(config, &sessions);
        return Ok(());
    }

    let mut total = 0usize;
    let mut device_count = 0usize;
    for device in snapshot.devices.iter().filter(|item| item.device.is_online) {
        let count = device
            .sessions
            .iter()
            .filter(|session| is_eligible_session(session))
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
    println!(
        "Workers    {authorized} authorized / {} open",
        sessions.len()
    );
    for session in sessions {
        let status = if config.is_session_authorized(session) {
            "authorized"
        } else {
            "not authorized"
        };
        println!("  {} · {status}", session.session_name);
    }
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
        candidate.device_name, candidate.eligible_sessions
    );
    let remote_root = prompt_remote_root(previous_root.as_deref())?;

    let sessions = snapshot.eligible_sessions_for(&candidate.device_identity);
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
                        "No reusable UU Terminal is open ({online_devices} online device(s)). Open the target Terminal in official UU Remote; uu is waiting. Ctrl+C cancels."
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
            candidate.eligible_sessions
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

fn run_remote_placeholder(config: &Config, cwd: &Path, argv: Vec<OsString>) -> Result<u8> {
    let binding = resolve_binding(config, cwd).ok_or_else(|| {
        let root = suggest_root(cwd)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| cwd.display().to_string());
        anyhow::anyhow!(
            "workspace {root} is not bound; open a UU Terminal for the target device, then run `uu bind`"
        )
    })?;

    let rendered = argv
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    bail!(
        "remote execution transport is not connected yet ({} · {}): {}",
        binding.device_name,
        binding.remote_root,
        rendered
    )
}
