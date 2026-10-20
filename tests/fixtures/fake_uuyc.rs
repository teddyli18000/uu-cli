use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::process::{Command, ExitCode, Stdio};

const DEVICE_ID: &str = "device-1";
const DEVICE_NAME: &str = "测试设备";
const SESSION_NAME: &str = "bash-3";
const CREATED_AT: &str = "2026-10-20T12:00:00Z";

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("fake-uuyc: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<u8, Box<dyn Error>> {
    let owned_args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();

    if args.len() == 1 && args[0] == "version" {
        println!("cliVersion: 4.43.1");
        println!("appVersion: 4.43.1");
        println!("message: available");
        return Ok(0);
    }

    if args.len() == 3 && args[0] == "device" && args[1] == "list" && args[2] == "--json" {
        return write_json(json!({
            "success": true,
            "data": [{
                "deviceId": DEVICE_ID,
                "deviceName": DEVICE_NAME,
                "isOnline": true,
                "platform": "linux"
            }],
            "error": null
        }));
    }

    if args.len() == 5
        && args[0] == "term"
        && args[1] == "-d"
        && args[3] == "ls"
        && args[4] == "--json"
    {
        return write_json(json!({
            "success": true,
            "data": {
                "items": [{
                    "name": SESSION_NAME,
                    "deviceId": DEVICE_ID,
                    "deviceName": DEVICE_NAME,
                    "shell": "bash",
                    "state": "running",
                    "createdAt": CREATED_AT,
                    "attachable": true
                }],
                "total": 1
            },
            "error": null
        }));
    }

    if args.len() == 5 && args[0] == "term" && args[1] == "-d" && args[3] == "attach" {
        return run_attached_terminal();
    }

    Err(format!("unsupported official CLI arguments: {args:?}").into())
}

fn write_json(value: Value) -> Result<u8, Box<dyn Error>> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(0)
}

fn run_attached_terminal() -> Result<u8, Box<dyn Error>> {
    if env::var("UU_TEST_MODE").as_deref() == Ok("bash") {
        let bash = env::var_os("UU_TEST_BASH")
            .ok_or("UU_TEST_BASH must name the Git Bash executable for bash mode")?;
        let status = Command::new(bash)
            .args(["--noprofile", "--norc"])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;
        return Ok(status.code().and_then(|code| u8::try_from(code).ok()).unwrap_or(1));
    }

    let mut command = String::new();
    let read = io::stdin().lock().read_line(&mut command)?;
    if read == 0 {
        return Ok(0);
    }
    record_dispatch(&command)?;

    let Some(nonce) = command
        .split_once("'UUC' '")
        .and_then(|(_, rest)| rest.split_once("' 'B'").map(|(nonce, _)| nonce))
        .filter(|nonce| {
            !nonce.is_empty() && nonce.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    else {
        return Err("received a terminal line without a valid uu-cli frame".into());
    };

    let mut stdout = io::stdout().lock();
    write!(stdout, "\r\nUUC{nonce}B")?;
    match env::var("UU_TEST_MODE").as_deref() {
        Ok("drop") => {
            stdout.write_all("partial output: 断线 世界".as_bytes())?;
            stdout.flush()?;
            Ok(0)
        }
        Ok("success") | Err(_) => {
            stdout.write_all("fake output: 你好 世界\r\n".as_bytes())?;
            write!(stdout, "\r\nUUC{nonce}E7Z")?;
            stdout.flush()?;
            Ok(0)
        }
        Ok(mode) => Err(format!("unsupported UU_TEST_MODE {mode:?}").into()),
    }
}

fn record_dispatch(wrapper: &str) -> Result<(), Box<dyn Error>> {
    let Some(path) = env::var_os("UU_TEST_LOG") else {
        return Ok(());
    };
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut log = OpenOptions::new().create(true).append(true).open(path)?;
    let record = json!({
        "event": "dispatch",
        "wrapper": wrapper
    });
    serde_json::to_writer(&mut log, &record)?;
    log.write_all(b"\n")?;
    log.flush()?;
    Ok(())
}
