#[cfg(unix)]
mod unix_tests {
    use uu_cli::runner::{run_framed_job, JobResult, JobRunError};
    use uu_cli::transfer;
    use anyhow::anyhow;
    use std::fs;
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "uu-cli-transfer-{}-{stamp:x}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn real_executor(
        remote_root: PathBuf,
        remote_home: PathBuf,
    ) -> impl FnMut(&str, &mut dyn Write) -> Result<JobResult, JobRunError> {
        move |command, output| {
            let mut child = Command::new("bash")
                .args(["--noprofile", "--norc", "-s"])
                .env("HOME", &remote_home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .map_err(|error| {
                    JobRunError::Preparation(anyhow!("cannot start bash fixture: {error}"))
                })?;
            let mut stdin = child.stdin.take().unwrap();
            let mut stdout = child.stdout.take().unwrap();
            let result = {
                let mut output_ref = output;
                run_framed_job(
                    &mut stdout,
                    &mut stdin,
                    &mut output_ref,
                    &remote_root.to_string_lossy(),
                    command,
                )
            };
            drop(stdin);
            drop(stdout);
            let _ = child.wait();
            result
        }
    }

    fn random_bytes(len: usize) -> Vec<u8> {
        let mut data = vec![0u8; len];
        fs::File::open("/dev/urandom")
            .unwrap()
            .read_exact(&mut data)
            .unwrap();
        data
    }

    #[test]
    fn round_trips_large_empty_unicode_absolute_and_tilde_paths() {
        let temp = TempDir::new();
        let root = temp.0.join("è¿ç«¯ workspace");
        let home = temp.0.join("remote home");
        fs::create_dir_all(root.join("ç®å½ with spaces")).unwrap();
        fs::create_dir_all(root.join("absolute target")).unwrap();
        fs::create_dir_all(home.join("nested directory")).unwrap();

        let mut executor = real_executor(root.clone(), home.clone());

        let data = random_bytes(1024 * 1024 + 137);
        let local_source = temp.0.join("æ¬å° source.bin");
        fs::write(&local_source, &data).unwrap();
        let remote_relative = "ç®å½ with spaces/æ°æ®.bin";
        let receipt = transfer::put(&local_source, remote_relative, &mut executor).unwrap();
        assert_eq!(receipt.size, data.len() as u64);
        assert_eq!(fs::read(root.join(remote_relative)).unwrap(), data);

        let local_copy = temp.0.join("ä¸è½½ copy.bin");
        let downloaded = transfer::get(remote_relative, &local_copy, &mut executor).unwrap();
        assert_eq!(downloaded, receipt);
        assert_eq!(fs::read(local_copy).unwrap(), data);

        let empty_source = temp.0.join("empty.bin");
        fs::write(&empty_source, b"").unwrap();
        let empty_receipt =
            transfer::put(&empty_source, "ç®å½ with spaces/ç©º.bin", &mut executor).unwrap();
        assert_eq!(empty_receipt.size, 0);
        let empty_copy = temp.0.join("empty copy.bin");
        transfer::get("ç®å½ with spaces/ç©º.bin", &empty_copy, &mut executor).unwrap();
        assert!(fs::read(empty_copy).unwrap().is_empty());

        let absolute = root.join("absolute target").join("absolute.bin");
        let absolute_text = absolute.to_string_lossy().into_owned();
        transfer::put(&local_source, &absolute_text, &mut executor).unwrap();
        assert_eq!(fs::read(&absolute).unwrap(), data);
        let absolute_copy = temp.0.join("absolute downloaded.bin");
        transfer::get(&absolute_text, &absolute_copy, &mut executor).unwrap();
        assert_eq!(fs::read(absolute_copy).unwrap(), data);

        let tilde_path = "~/nested directory/tilde.bin";
        transfer::put(&local_source, tilde_path, &mut executor).unwrap();
        let tilde_remote = home.join("nested directory").join("tilde.bin");
        assert_eq!(fs::read(&tilde_remote).unwrap(), data);
        let tilde_copy = temp.0.join("tilde downloaded.bin");
        transfer::get(tilde_path, &tilde_copy, &mut executor).unwrap();
        assert_eq!(fs::read(tilde_copy).unwrap(), data);
    }

    #[test]
    fn incomplete_upload_never_replaces_existing_remote_file() {
        let temp = TempDir::new();
        let root = temp.0.join("remote");
        let home = temp.0.join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(root.join("target.bin"), b"old remote contents").unwrap();
        let source = temp.0.join("source.bin");
        fs::write(&source, random_bytes(4096)).unwrap();

        let mut calls = 0usize;
        let mut real = real_executor(root.clone(), home);
        let mut executor = |command: &str, output: &mut dyn Write| {
            calls += 1;
            if calls == 3 {
                return Ok(JobResult { exit_code: 19 });
            }
            real(command, output)
        };

        let error = transfer::put(&source, "target.bin", &mut executor).unwrap_err();
        assert!(matches!(error, JobRunError::Completed(_)));
        assert_eq!(
            fs::read(root.join("target.bin")).unwrap(),
            b"old remote contents"
        );
    }

    #[test]
    fn unknown_upload_error_is_preserved_without_replay_or_publish() {
        let temp = TempDir::new();
        let root = temp.0.join("remote");
        let home = temp.0.join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(root.join("target.bin"), b"old").unwrap();
        let source = temp.0.join("source.bin");
        fs::write(&source, random_bytes(2048)).unwrap();

        let mut calls = 0usize;
        let mut real = real_executor(root.clone(), home);
        let mut executor = |command: &str, output: &mut dyn Write| {
            calls += 1;
            if calls == 2 {
                return Err(JobRunError::Unknown(anyhow!("injected uncertainty")));
            }
            real(command, output)
        };

        let error = transfer::put(&source, "target.bin", &mut executor).unwrap_err();
        match error {
            JobRunError::Unknown(error) => {
                assert_eq!(error.to_string(), "injected uncertainty");
            }
            other => panic!("expected the original Unknown error, got {other}"),
        }
        assert_eq!(calls, 2);
        assert_eq!(fs::read(root.join("target.bin")).unwrap(), b"old");
    }

    #[test]
    fn failed_download_keeps_local_destination_and_removes_partial_file() {
        let temp = TempDir::new();
        let root = temp.0.join("remote");
        let home = temp.0.join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(root.join("source.bin"), random_bytes(4096)).unwrap();
        let destination = temp.0.join("existing.bin");
        fs::write(&destination, b"old local contents").unwrap();

        let mut calls = 0usize;
        let mut real = real_executor(root, home);
        let mut executor = |command: &str, output: &mut dyn Write| {
            calls += 1;
            if calls == 2 {
                output.write_all(b"QUJD\n").map_err(|error| {
                    JobRunError::Preparation(anyhow!("fixture output failed: {error}"))
                })?;
                return Err(JobRunError::Unknown(anyhow!("stream interrupted")));
            }
            real(command, output)
        };

        let error = transfer::get("source.bin", &destination, &mut executor).unwrap_err();
        match error {
            JobRunError::Unknown(error) => assert_eq!(error.to_string(), "stream interrupted"),
            other => panic!("expected the original Unknown error, got {other}"),
        }
        assert_eq!(calls, 2);
        assert_eq!(fs::read(&destination).unwrap(), b"old local contents");
        for entry in fs::read_dir(&temp.0).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(
                !name.contains(".uu-cli-"),
                "partial local file was left behind: {name}"
            );
        }
    }

    #[test]
    fn integrity_failure_does_not_replace_local_destination() {
        let temp = TempDir::new();
        let root = temp.0.join("remote");
        let home = temp.0.join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(root.join("source.bin"), b"correct remote bytes").unwrap();
        let destination = temp.0.join("existing.bin");
        fs::write(&destination, b"old local contents").unwrap();

        let mut calls = 0usize;
        let mut real = real_executor(root, home);
        let mut executor = |command: &str, output: &mut dyn Write| {
            calls += 1;
            if calls == 2 {
                output.write_all(b"Y29ycnVwdGVk\n").map_err(|error| {
                    JobRunError::Preparation(anyhow!("fixture output failed: {error}"))
                })?;
                return Ok(JobResult { exit_code: 0 });
            }
            real(command, output)
        };

        let error = transfer::get("source.bin", &destination, &mut executor).unwrap_err();
        assert!(matches!(error, JobRunError::Completed(_)));
        assert_eq!(fs::read(destination).unwrap(), b"old local contents");
    }
}
