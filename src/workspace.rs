use crate::model::{Config, WorkspaceBinding};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn canonical(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).with_context(|| format!("cannot resolve path {}", path.display()))
}

fn comparable(path: &Path) -> Option<String> {
    let path = canonical(path).ok()?;
    let mut text = path.to_string_lossy().replace('/', "\\");
    while text.ends_with('\\') && text.len() > 3 {
        text.pop();
    }
    #[cfg(windows)]
    {
        text.make_ascii_lowercase();
    }
    Some(text)
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
    child
        .strip_prefix(root)
        .is_some_and(|rest| rest.starts_with('\\'))
}

pub fn resolve_binding<'a>(config: &'a Config, cwd: &Path) -> Option<&'a WorkspaceBinding> {
    let cwd = comparable(cwd)?;
    config
        .bindings
        .iter()
        .filter_map(|binding| {
            let root = comparable(&binding.local_root)?;
            is_ancestor_key(&root, &cwd).then_some((root.len(), binding))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, binding)| binding)
}

pub fn suggest_root(cwd: &Path) -> Result<PathBuf> {
    let cwd = canonical(cwd)?;
    for ancestor in cwd.ancestors() {
        if ancestor.join(".git").exists() {
            return Ok(ancestor.to_path_buf());
        }
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
    fn git_root_is_preferred() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        let child = root.join("src").join("deep");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();

        assert!(same_path(&suggest_root(&child).unwrap(), &root));
    }

    #[test]
    fn non_git_uses_current_directory() {
        let tmp = TempDir::new().unwrap();
        let child = tmp.path().join("plain");
        std::fs::create_dir_all(&child).unwrap();

        assert!(same_path(&suggest_root(&child).unwrap(), &child));
    }
}
