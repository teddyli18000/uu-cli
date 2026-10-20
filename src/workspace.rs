use crate::model::{Config, WorkspaceBinding};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn canonical(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).with_context(|| format!("cannot resolve path {}", path.display()))
}

fn comparable(path: &Path) -> Option<String> {
    let resolved = canonical(path).ok()?;
    let mut key = resolved.to_string_lossy().replace('/', "\\");
    trim_trailing_separators(&mut key);

    #[cfg(windows)]
    {
        // Windows paths are case-insensitive. Unicode lowercase also covers
        // non-ASCII path components rather than only drive letters.
        key = key.to_lowercase();
    }

    Some(key)
}

fn trim_trailing_separators(path: &mut String) {
    while path.ends_with('\\') {
        let is_drive_root = path.len() == 3 && path.as_bytes().get(1) == Some(&b':');
        if path.len() == 1 || is_drive_root {
            break;
        }
        path.pop();
    }
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    match (comparable(a), comparable(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

fn is_ancestor_key(root: &str, child: &str) -> bool {
    if root == child {
        return true;
    }
    if root.ends_with('\\') {
        child.starts_with(root)
    } else {
        child
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('\\'))
    }
}

/// Return the nearest repository root, including worktrees and submodules whose
/// .git entry is a file instead of a directory.
fn git_root_boundary(cwd: &Path) -> Option<PathBuf> {
    let cwd = canonical(cwd).ok()?;
    cwd.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

pub fn resolve_binding<'a>(config: &'a Config, cwd: &Path) -> Option<&'a WorkspaceBinding> {
    let resolved_cwd = canonical(cwd).ok()?;
    let cwd = comparable(&resolved_cwd)?;
    let git_boundary = git_root_boundary(&resolved_cwd).and_then(|boundary| comparable(&boundary));

    config
        .bindings
        .iter()
        .filter_map(|binding| {
            let root = comparable(&binding.local_root)?;
            if !is_ancestor_key(&root, &cwd) {
                return None;
            }

            // An inner Git repository or submodule is its own workspace
            // boundary. A binding outside it must not leak into that checkout.
            if git_boundary
                .as_deref()
                .is_some_and(|boundary| !is_ancestor_key(boundary, &root))
            {
                return None;
            }

            Some((root.len(), binding))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, binding)| binding)
}

pub fn suggest_root(cwd: &Path) -> Result<PathBuf> {
    let cwd = canonical(cwd)?;
    if let Some(root) = git_root_boundary(&cwd) {
        return Ok(root);
    }
    Ok(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WorkspaceBinding;
    use tempfile::TempDir;

    fn binding(root: PathBuf, name: &str) -> WorkspaceBinding {
        WorkspaceBinding {
            local_root: root,
            device_identity: name.into(),
            device_name: name.into(),
            remote_root: "/srv/project".into(),
        }
    }

    #[test]
    fn nearest_binding_wins() {
        let tmp = TempDir::new().unwrap();
        let outer = tmp.path().join("outer");
        let inner = outer.join("inner");
        let leaf = inner.join("src");
        std::fs::create_dir_all(&leaf).unwrap();

        let mut config = Config::default();
        config.bindings.push(binding(outer.clone(), "outer"));
        config.bindings.push(binding(inner.clone(), "inner"));

        let got = resolve_binding(&config, &leaf).unwrap();
        assert_eq!(got.device_identity, "inner");
    }

    #[test]
    fn nested_git_repository_does_not_inherit_parent_binding() {
        let tmp = TempDir::new().unwrap();
        let outer = tmp.path().join("outer");
        let nested = outer.join("vendor").join("nested");
        let leaf = nested.join("src");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::create_dir(outer.join(".git")).unwrap();
        std::fs::create_dir(nested.join(".git")).unwrap();

        let mut config = Config::default();
        config.bindings.push(binding(outer, "outer"));

        assert!(resolve_binding(&config, &leaf).is_none());
        assert!(same_path(&suggest_root(&leaf).unwrap(), &nested));
    }

    #[test]
    fn nested_submodule_git_file_is_a_workspace_boundary() {
        let tmp = TempDir::new().unwrap();
        let outer = tmp.path().join("outer");
        let submodule = outer.join("vendor").join("submodule");
        let leaf = submodule.join("src");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::create_dir(outer.join(".git")).unwrap();
        std::fs::write(submodule.join(".git"), "gitdir: ../.git/modules/submodule").unwrap();

        let mut config = Config::default();
        config.bindings.push(binding(outer, "outer"));

        assert!(resolve_binding(&config, &leaf).is_none());
        assert!(same_path(&suggest_root(&leaf).unwrap(), &submodule));
    }

    #[test]
    fn nested_git_repository_uses_its_own_binding() {
        let tmp = TempDir::new().unwrap();
        let outer = tmp.path().join("outer");
        let nested = outer.join("tools").join("repo");
        let leaf = nested.join("src").join("deep");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::create_dir(outer.join(".git")).unwrap();
        std::fs::create_dir(nested.join(".git")).unwrap();

        let mut config = Config::default();
        config.bindings.push(binding(outer, "outer"));
        config.bindings.push(binding(nested, "nested"));

        let got = resolve_binding(&config, &leaf).unwrap();
        assert_eq!(got.device_identity, "nested");
    }

    #[test]
    fn non_git_uses_current_directory() {
        let tmp = TempDir::new().unwrap();
        let child = tmp.path().join("plain");
        std::fs::create_dir_all(&child).unwrap();

        assert!(same_path(&suggest_root(&child).unwrap(), &child));
    }

    #[test]
    fn trailing_separator_does_not_create_a_distinct_workspace_key() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let with_separator =
            PathBuf::from(format!("{}{}", root.display(), std::path::MAIN_SEPARATOR));

        assert!(same_path(&root, &with_separator));
    }

    #[test]
    fn unicode_workspace_paths_resolve_from_nested_directories() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("项目 Ä");
        let leaf = root.join("源码").join("deep");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();

        let mut config = Config::default();
        config.bindings.push(binding(root, "unicode"));

        let got = resolve_binding(&config, &leaf).unwrap();
        assert_eq!(got.device_identity, "unicode");
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_case_and_drive_letter_variants_resolve_to_same_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("Project Ä").join("Inner");
        std::fs::create_dir_all(&root).unwrap();

        let mut variant = root.to_string_lossy().to_string();
        if let Some(letter) = variant.chars().next() {
            let replacement = if letter.is_ascii_lowercase() {
                letter.to_ascii_uppercase()
            } else {
                letter.to_ascii_lowercase()
            };
            variant.replace_range(..letter.len_utf8(), &replacement.to_string());
        }
        variant = variant.replace("Project Ä", "project ä");
        let alternate = PathBuf::from(variant);

        assert!(same_path(&root, &alternate));
        assert!(same_path(
            &root,
            &PathBuf::from(format!("{}\\", alternate.display()))
        ));
    }

    #[test]
    fn git_root_is_preferred() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let child = root.join("src").join("deep");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();

        assert!(same_path(&suggest_root(&child).unwrap(), &root));
    }
}
