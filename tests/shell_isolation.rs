#![cfg(unix)]
use std::process::Command;
use uu_cli::job::{wrap_job, CompletionDecoder};

#[test]
fn persistent_shell_keeps_workspace_and_shell_state_isolated() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("project 空格 'quoted'");
    std::fs::create_dir(&root).unwrap();
    let root = root.to_str().unwrap();
    let script = [
        wrap_job(
            root,
            "cd /tmp; export UU_PRIVATE_VALUE=leak; exit 7",
            "jobA",
        )
        .unwrap(),
        wrap_job(
            root,
            "printf '%s\\n' \"$PWD\"; test -z \"$UU_PRIVATE_VALUE\"",
            "jobB",
        )
        .unwrap(),
        wrap_job(root, "exec sh -c 'exit 9'", "jobC").unwrap(),
        wrap_job(root, "printf alive", "jobD").unwrap(),
    ]
    .concat();
    let output = Command::new("bash")
        .arg("--noprofile")
        .arg("--norc")
        .arg("-c")
        .arg(script)
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut a = CompletionDecoder::new("jobA").unwrap();
    assert!(a.push(&output.stdout).unwrap().is_empty());
    assert_eq!(a.completed(), Some(7));
    let mut b = CompletionDecoder::new("jobB").unwrap();
    assert_eq!(
        String::from_utf8(b.push(&output.stdout).unwrap()).unwrap(),
        format!("{root}\n")
    );
    assert_eq!(b.completed(), Some(0));
    let mut c = CompletionDecoder::new("jobC").unwrap();
    c.push(&output.stdout).unwrap();
    assert_eq!(c.completed(), Some(9));
    let mut d = CompletionDecoder::new("jobD").unwrap();
    assert_eq!(d.push(&output.stdout).unwrap(), b"alive");
    assert_eq!(d.completed(), Some(0));
}
