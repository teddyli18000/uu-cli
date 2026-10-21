#![cfg(unix)]
use std::process::Command;
use uu_cli::job::{wrap_job, CompletionDecoder};

#[test]
fn persistent_bash_survives_late_eof_without_breaking_foreground_stdin() {
    let output = Command::new("python3")
        .arg("-c")
        .arg(include_str!("fixtures/bash_eof_pty.py"))
        .arg(wrap_job("/tmp", "printf fast", "fastjob").unwrap())
        .arg(wrap_job("/tmp", "cat; printf 'stdin-complete\\n'", "stdinjob").unwrap())
        .arg(wrap_job("/tmp", "printf worker-alive", "afterjob").unwrap())
        .output()
        .expect("run Linux PTY regression");
    assert!(
        output.status.success(),
        "PTY regression failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

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
            "printf '%s\\n' \"$PWD\"; test -z \"$UU_PRIVATE_VALUE\"; case $- in *m*) exit 11 ;; esac",
            "jobB",
        )
        .unwrap(),
        wrap_job(root, "exec sh -c 'exit 9'", "jobC").unwrap(),
        wrap_job(root, "printf alive", "jobD").unwrap(),
        wrap_job(
            root,
            "trap 'printf user-exit-trap' EXIT; exit 23",
            "jobE",
        )
        .unwrap(),
        wrap_job(root, "printf final", "jobF").unwrap(),
    ]
    .concat();
    let output = Command::new("bash")
        .arg("--noprofile")
        .arg("--norc")
        .arg("-m")
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
    let mut e = CompletionDecoder::new("jobE").unwrap();
    assert_eq!(e.push(&output.stdout).unwrap(), b"user-exit-trap");
    assert_eq!(e.completed(), Some(23));
    let mut f = CompletionDecoder::new("jobF").unwrap();
    assert_eq!(f.push(&output.stdout).unwrap(), b"final");
    assert_eq!(f.completed(), Some(0));
}
