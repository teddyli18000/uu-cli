use anyhow::{bail, Context, Result};
use std::env;
use std::ffi::OsString;
use std::process::ExitCode;
use uu_cli::cli::{self, Command};
use uu_cli::config::ConfigStore;
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
    let config = store.load()?;
    let cwd = env::current_dir().context("cannot determine current directory")?;

    match command {
        Command::List => {
            let suggested = suggest_root(&cwd)?;
            if let Some(binding) = resolve_binding(&config, &cwd) {
                println!("Workspace  {}", binding.local_root.display());
                println!(
                    "Remote     {} · {}",
                    binding.device_name, binding.remote_root
                );
                println!("Workers    discovery not connected in bootstrap build");
            } else {
                println!("Workspace  {}", suggested.display());
                println!("Remote     unbound");
                println!("Workers    0");
            }
            print_official_uu_status();
            Ok(0)
        }
        Command::Bind => {
            let suggested = suggest_root(&cwd)?;
            bail!(
                "workspace {} is not bound yet; UU Terminal discovery is the next implementation step",
                suggested.display()
            )
        }
        Command::Put { .. } | Command::Get { .. } => {
            bail!("file transfer is not implemented in this bootstrap build")
        }
        Command::Run { argv } => run_remote_placeholder(&config, &cwd, argv),
    }
}

fn print_official_uu_status() {
    match OfficialCli::locate() {
        Ok(cli) => match cli.version() {
            Ok(version) => {
                let status = version.message.as_deref().unwrap_or("available");
                println!(
                    "UU         {} · {} · {}",
                    version.cli_version,
                    cli.path().display(),
                    status
                );
            }
            Err(error) => println!("UU         found · incompatible ({error})"),
        },
        Err(_) => println!("UU         not found"),
    }
}

fn run_remote_placeholder(
    config: &uu_cli::model::Config,
    cwd: &std::path::Path,
    argv: Vec<OsString>,
) -> Result<u8> {
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
