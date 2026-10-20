use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::thread;

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
    record_event(json!({"event": "invoke", "args": args}))?;

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
    record_event(json!({"event": "attach_started"}))?;

    if env::var("UU_TEST_MODE").as_deref() == Ok("bash") {
        return run_git_bash_attached_terminal();
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

const DSR_RESPONSE: &[u8] = b"\x1b[1;1R";

fn run_git_bash_attached_terminal() -> Result<u8, Box<dyn Error>> {
    let bash = env::var_os("UU_TEST_BASH")
        .ok_or("UU_TEST_BASH must name the Git Bash executable for bash mode")?;
    let mut command = Command::new(bash);
    command
        .args(["--noprofile", "--norc"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn()?;
    let child_stdin = child.stdin.take().ok_or("Git Bash stdin was not piped")?;
    let child_stdout = child.stdout.take().ok_or("Git Bash stdout was not piped")?;
    let child_stderr = child.stderr.take().ok_or("Git Bash stderr was not piped")?;
    let _input_relay = thread::Builder::new()
        .name("fake-uuyc-git-bash-input".to_owned())
        .spawn(move || {
            if let Err(error) = forward_conpty_input(io::stdin(), child_stdin) {
                eprintln!("fake-uuyc: Git Bash input relay failed: {error}");
            }
        })?;
    let stdout_relay = thread::Builder::new()
        .name("fake-uuyc-git-bash-stdout".to_owned())
        .spawn(move || {
            if let Err(error) = relay_bash_output(child_stdout, io::stdout()) {
                eprintln!("fake-uuyc: Git Bash stdout relay failed: {error}");
            }
        })?;
    let stderr_relay = thread::Builder::new()
        .name("fake-uuyc-git-bash-stderr".to_owned())
        .spawn(move || {
            if let Err(error) = relay_bash_output(child_stderr, io::stderr()) {
                eprintln!("fake-uuyc: Git Bash stderr relay failed: {error}");
            }
        })?;
    let status = child.wait()?;
    for relay in [stdout_relay, stderr_relay] {
        if relay.join().is_err() {
            eprintln!("fake-uuyc: Git Bash output relay panicked");
        }
    }
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1))
}

fn relay_bash_output<R: Read, W: Write>(mut input: R, mut output: W) -> io::Result<()> {
    io::copy(&mut input, &mut output)?;
    output.flush()
}
fn forward_conpty_input<R: Read, W: Write>(
    mut input: R,
    mut output: W,
) -> Result<(), Box<dyn Error>> {
    let mut read_buffer = [0_u8; 4096];
    let mut possible_dsr = Vec::with_capacity(DSR_RESPONSE.len());
    let mut swallow_next_lf = false;
    let mut line = Vec::new();

    loop {
        let bytes_read = input.read(&mut read_buffer)?;
        if bytes_read == 0 {
            break;
        }
        for &byte in &read_buffer[..bytes_read] {
            possible_dsr.push(byte);
            while !DSR_RESPONSE.starts_with(&possible_dsr) {
                let forwarded = possible_dsr.remove(0);
                forward_bash_byte(forwarded, &mut swallow_next_lf, &mut line, &mut output)?;
            }
            if possible_dsr.as_slice() == DSR_RESPONSE {
                possible_dsr.clear();
            }
        }
    }

    for byte in possible_dsr {
        forward_bash_byte(byte, &mut swallow_next_lf, &mut line, &mut output)?;
    }
    if !line.is_empty() {
        dispatch_bash_line(&mut line, &mut output, false)?;
    }
    output.flush()?;
    Ok(())
}

fn forward_bash_byte<W: Write>(
    byte: u8,
    swallow_next_lf: &mut bool,
    line: &mut Vec<u8>,
    output: &mut W,
) -> Result<(), Box<dyn Error>> {
    if *swallow_next_lf {
        *swallow_next_lf = false;
        if byte == b'\n' {
            return Ok(());
        }
    }

    match byte {
        b'\r' => {
            dispatch_bash_line(line, output, true)?;
            *swallow_next_lf = true;
        }
        b'\n' => dispatch_bash_line(line, output, true)?,
        _ => line.push(byte),
    }
    Ok(())
}

fn dispatch_bash_line<W: Write>(
    line: &mut Vec<u8>,
    output: &mut W,
    terminated: bool,
) -> Result<(), Box<dyn Error>> {
    let input_bytes = line.len() + if terminated { 1 } else { 0 };
    output.write_all(line)?;
    if terminated {
        output.write_all(b"\n")?;
    }
    output.flush()?;
    if !line.is_empty() {
        record_event(json!({
            "event": "dispatch",
            "transport": "git-bash-pipe",
            "input_bytes": input_bytes
        }))?;
    }
    line.clear();
    Ok(())
}
fn record_dispatch(wrapper: &str) -> Result<(), Box<dyn Error>> {
    record_event(json!({
        "event": "dispatch",
        "wrapper": wrapper
    }))
}

fn record_event(record: Value) -> Result<(), Box<dyn Error>> {
    let Some(path) = env::var_os("UU_TEST_LOG") else {
        return Ok(());
    };
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut log = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut log, &record)?;
    log.write_all(b"\n")?;
    log.flush()?;
    Ok(())
}
