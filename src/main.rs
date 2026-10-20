#[cfg(not(windows))]
use anyhow::bail;
use anyhow::Result;
use std::env;
use std::process::ExitCode;
use uu_cli::cli;
#[cfg(not(windows))]
use uu_cli::cli::Command;
use uu_cli::config::ConfigStore;
#[cfg(windows)]
mod windows_app;

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("uu: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<u8> {
    #[cfg(windows)]
    if let Some(mode) = env::args().nth(1) {
        if mode == "__watch" || mode == "__setup" || mode == "__authorize" {
            return windows_app::internal(&mode);
        }
    }
    let command = cli::parse(env::args_os().skip(1)).map_err(anyhow::Error::new)?;
    let store = ConfigStore::from_env()?;
    #[cfg(windows)]
    {
        windows_app::execute(command, &store)
    }
    #[cfg(not(windows))]
    {
        if matches!(command, Command::List) {
            let config = store.load()?;
            let cwd = env::current_dir()?;
            let root = uu_cli::workspace::suggest_root(&cwd)?;
            println!("Workspace  {}", root.display());
            if let Some(binding) = uu_cli::workspace::resolve_binding(&config, &cwd) {
                println!(
                    "Remote     {} · {}",
                    binding.device_name, binding.remote_root
                );
            } else {
                println!("Remote     unbound");
            }
            println!("UU         Windows is required");
            Ok(0)
        } else {
            bail!("uu-cli v1 requires Windows and official UU Remote")
        }
    }
}
