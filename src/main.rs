use anyhow::{bail, Context, Result};
use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use uu_cli::cli::{self, Command};
use uu_cli::config::ConfigStore;
use uu_cli::model::{Config, DeviceInfo, SessionInfo, WorkspaceBinding};
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

    if let Some(binding) = binding {
        match cli.list_sessions(&binding.device_identity) {
            Ok(sessions) => print_bound_workers(config, &sessions),
            Err(error) => println!("Workers    unavailable ({error})"),
        }
        return Ok(());
    }

    match discover_candidates(&cli) {
        Ok(candidates) if candidates.is_empty() => {
            println!("Workers    0 open terminals");
        }
        Ok(candidates) => {
            let total: usize = candidates.iter().map(|item| item.sessions.len()).sum();
            println!(
                "Workers    {total} open terminals on {} device(s)",
                candidates.len()
            );
            for candidate in candidates {
                println!(
                    "  {} · {} terminal(s)",
                    display_device_name(&candidate.device),
                    candidate.sessions.len()
                );
            }
        }
        Err(error) => println!("Workers    discovery failed ({error})"),
    }

    Ok(())
}

fn print_bound_workers(config: &Config, sessions: &[SessionInfo]) {
    let usable: Vec<&SessionInfo> = sessions
        .iter()
        .filter(|session| is_usable(session))
        .collect();
    let authorized = usable
        .iter()
        .filter(|session| config.is_session_authorized(session))
        .count();
    println!("Workers    {authorized} authorized / {} open", usable.len());
    for session in usable {
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
    let candidates = discover_candidates(&cli)?;

    let mut candidates: Vec<DeviceCandidate> = candidates
        .into_iter()
        .map(|mut candidate| {
            candidate
                .sessions
                .retain(|session| session.authorization_fingerprint().is_some());
            candidate
        })
        .filter(|candidate| !candidate.sessions.is_empty())
        .collect();

    if candidates.is_empty() {
        bail!(
            "no reusable UU Terminal is open; manually open a Terminal for the target device in official UU Remote, then run `uu bind` again"
        );
    }

    let selected = choose_candidate(&candidates)?;
    let candidate = candidates.swap_remove(selected);
    let device_name = display_device_name(&candidate.device).to_owned();
    let previous_root = resolve_binding(config, &local_root)
        .filter(|binding| binding.device_identity == candidate.device.device_identity)
        .map(|binding| binding.remote_root.clone());

    println!(
        "Target     {} · {} open terminal(s)",
        device_name,
        candidate.sessions.len()
    );
    let remote_root = prompt_remote_root(previous_root.as_deref())?;

    let mut authorized = 0usize;
    for session in &candidate.sessions {
        if config.authorize_session(session) {
            authorized += 1;
        }
    }
    if authorized == 0 {
        bail!("the open Terminal sessions do not expose a stable identity; refusing to persist an unsafe worker authorization");
    }

    config.upsert_binding(WorkspaceBinding {
        local_root: local_root.clone(),
        device_identity: candidate.device.device_identity,
        device_name: device_name.clone(),
        remote_root: remote_root.clone(),
    });
    store.save(config)?;

    println!("Bound      {}", local_root.display());
    println!("Remote     {device_name} · {remote_root}");
    println!("Workers    {authorized} authorized");
    Ok(())
}

#[derive(Debug)]
struct DeviceCandidate {
    device: DeviceInfo,
    sessions: Vec<SessionInfo>,
}

fn discover_candidates(cli: &OfficialCli) -> Result<Vec<DeviceCandidate>> {
    let devices = cli.list_devices()?;
    let mut candidates = Vec::new();

    for device in devices.into_iter().filter(|device| device.is_online) {
        let sessions = match cli.list_sessions(&device.device_identity) {
            Ok(sessions) => sessions,
            Err(_) => continue,
        };
        let sessions: Vec<SessionInfo> = sessions.into_iter().filter(is_usable).collect();
        if !sessions.is_empty() {
            candidates.push(DeviceCandidate { device, sessions });
        }
    }

    Ok(candidates)
}

fn is_usable(session: &SessionInfo) -> bool {
    session.state.eq_ignore_ascii_case("running") && session.attachable != Some(false)
}

fn display_device_name(device: &DeviceInfo) -> &str {
    if device.device_name.trim().is_empty() {
        "Unnamed device"
    } else {
        &device.device_name
    }
}

fn choose_candidate(candidates: &[DeviceCandidate]) -> Result<usize> {
    if candidates.len() == 1 {
        return Ok(0);
    }

    println!("Open UU Terminals:");
    for (index, candidate) in candidates.iter().enumerate() {
        println!(
            "  {}) {} · {} terminal(s)",
            index + 1,
            display_device_name(&candidate.device),
            candidate.sessions.len()
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
