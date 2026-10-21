#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        if let Ok(store) = uu_cli::config::ConfigStore::from_env() {
            let args: Vec<_> = std::env::args().collect();
            if args.get(1).map(String::as_str) == Some("__worker") {
                let result = (|| -> anyhow::Result<()> {
                    let session = serde_json::from_str(
                        args.get(2)
                            .ok_or_else(|| anyhow::anyhow!("missing worker instance"))?,
                    )?;
                    let cli = args
                        .get(3)
                        .ok_or_else(|| anyhow::anyhow!("missing official component"))?;
                    uu_cli::worker_host::run(&store, session, std::path::Path::new(cli))
                })();
                if result.is_err() {
                    std::process::exit(2);
                }
                return;
            }
            let _ = uu_cli::watcher::run(store.path());
        }
    }
}
