use anyhow::{bail, Result};
use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};

const CLI_NAME: &str = "uuyc-cli.exe";

/// Locate the official UU command-line component without relying on a developer
/// machine path. Explicit override wins, then PATH, registry-derived install
/// roots on Windows, then conservative Program Files fallbacks.
pub fn locate_official_cli() -> Result<PathBuf> {
    let candidates = candidate_paths();
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "official UU command-line component was not found; install/start 网易UU远程 or set UUYC_CLI for diagnostics"
    )
}

pub fn candidate_paths() -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    if let Some(explicit) = env::var_os("UUYC_CLI") {
        push_unique(&mut out, &mut seen, PathBuf::from(explicit));
    }

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            push_unique(&mut out, &mut seen, dir.join(CLI_NAME));
        }
    }

    #[cfg(windows)]
    for root in windows_registry_install_roots() {
        push_unique(&mut out, &mut seen, root.join("bin").join(CLI_NAME));
    }

    #[cfg(windows)]
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(root) = env::var_os(variable) {
            push_unique(
                &mut out,
                &mut seen,
                PathBuf::from(root)
                    .join("Netease")
                    .join("GameViewer")
                    .join("bin")
                    .join(CLI_NAME),
            );
        }
    }

    out
}

fn push_unique(out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        out.push(path);
    }
}

#[cfg(windows)]
fn windows_registry_install_roots() -> Vec<PathBuf> {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    let hives = [
        RegKey::predef(HKEY_LOCAL_MACHINE),
        RegKey::predef(HKEY_CURRENT_USER),
    ];
    let views = [KEY_READ | KEY_WOW64_64KEY, KEY_READ | KEY_WOW64_32KEY];
    let mut seen = HashSet::new();
    let mut roots = Vec::new();

    for hive in hives {
        for flags in views {
            let Ok(uninstall) = hive.open_subkey_with_flags(UNINSTALL, flags) else {
                continue;
            };
            for subkey_name in uninstall.enum_keys().flatten() {
                let Ok(entry) = uninstall.open_subkey_with_flags(&subkey_name, flags) else {
                    continue;
                };
                let display_name = entry.get_value::<String, _>("DisplayName").unwrap_or_default();
                let publisher = entry.get_value::<String, _>("Publisher").unwrap_or_default();
                if !looks_like_uu_remote(&display_name, &publisher) {
                    continue;
                }

                if let Ok(location) = entry.get_value::<String, _>("InstallLocation") {
                    let location = location.trim();
                    if !location.is_empty() {
                        let root = PathBuf::from(location);
                        if seen.insert(root.clone()) {
                            roots.push(root);
                        }
                    }
                }

                if let Ok(uninstall_string) = entry.get_value::<String, _>("UninstallString") {
                    if let Some(uninstaller) = parse_uninstall_executable(&uninstall_string) {
                        if let Some(parent) = uninstaller.parent() {
                            let root = parent.to_path_buf();
                            if seen.insert(root.clone()) {
                                roots.push(root);
                            }
                        }
                    }
                }
            }
        }
    }

    roots
}

fn looks_like_uu_remote(display_name: &str, publisher: &str) -> bool {
    let display = display_name.to_ascii_lowercase();
    let publisher = publisher.to_ascii_lowercase();
    (display.contains("uu") && (display.contains("remote") || display_name.contains("远程")))
        || (publisher.contains("netease") && display.contains("uu"))
}

fn parse_uninstall_executable(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }

    if let Some(rest) = command.strip_prefix('"') {
        let end = rest.find('"')?;
        return Some(PathBuf::from(&rest[..end]));
    }

    let lower = command.to_ascii_lowercase();
    let end = lower.find(".exe")? + 4;
    Some(PathBuf::from(command[..end].trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_observed_uu_remote_registry_entry() {
        assert!(looks_like_uu_remote("UU远程", "Netease"));
        assert!(looks_like_uu_remote("UU Remote", "NetEase (Hangzhou) Network Co., Ltd"));
        assert!(!looks_like_uu_remote("Some Other Tool", "Example Corp"));
    }

    #[test]
    fn parses_quoted_uninstaller_path() {
        assert_eq!(
            parse_uninstall_executable(r#""C:\Program Files\Netease\GameViewer\Uninstall.exe" /S"#),
            Some(PathBuf::from(
                r"C:\Program Files\Netease\GameViewer\Uninstall.exe"
            ))
        );
    }

    #[test]
    fn parses_unquoted_uninstaller_path() {
        assert_eq!(
            parse_uninstall_executable(r"C:\Netease\GameViewer\Uninstall.exe /S"),
            Some(PathBuf::from(r"C:\Netease\GameViewer\Uninstall.exe"))
        );
    }

    #[test]
    fn program_files_candidate_shape_matches_observed_install() {
        let root = Path::new(r"C:\Program Files")
            .join("Netease")
            .join("GameViewer")
            .join("bin")
            .join(CLI_NAME);
        assert!(root.to_string_lossy().ends_with(r"Netease\GameViewer\bin\uuyc-cli.exe"));
    }
}
