#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        if let Ok(store) = uu_cli::config::ConfigStore::from_env() {
            let _ = uu_cli::watcher::run(store.path());
        }
    }
}
