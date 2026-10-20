use fs2::FileExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::process::{Command, ExitCode, Stdio};
use std::thread;

const DEVICE_ID: &str = "device-1";
const DEVICE_NAME: &str = "测试设备";
const SESSION_NAME: &str = "bash-3";
const CREATED_AT: &str = "2026-10-20T12:00:00Z";

#[derive(Debug, Deserialize)]
struct Fixture {
    devices: Vec<FixtureDevice>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureDevice {
    device_id: String,
    device_name: String,
    #[serde(default = "default_online")]
    is_online: bool,
    #[serde(default)]
    platform: Option<Value>,
    #[serde(default)]
    terminals: Vec<FixtureTerminal>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureTerminal {
    name: String,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default = "default_terminal_state")]
    state: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default = "default_attachable")]
    attachable: bool,
}

#[derive(Clone, Debug)]
struct AttachIdentity {
    device_id: String,
    session_name: String,
    created_at: Option<String>,
}

fn default_online() -> bool {
    true
}

fn default_terminal_state() -> String {
    "running".to_owned()
}

fn default_attachable() -> bool {
    true
}

fn session_is_available(device: &FixtureDevice, terminal: &FixtureTerminal) -> bool {
    device.is_online
        && (terminal.state.eq_ignore_ascii_case("running")
            || terminal.state.eq_ignore_ascii_case("detached"))
        && terminal.attachable
}
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
    let fixture = load_fixture()?;
    record_event(invocation_event(&args, &fixture))?;

    if args.len() == 1 && args[0] == "version" {
        println!("cliVersion: 4.43.1");
        println!("appVersion: 4.43.1");
        println!("message: available");
        return Ok(0);
    }

    if args.len() == 3 && args[0] == "device" && args[1] == "list" && args[2] == "--json" {
        let device_ids = fixture
            .devices
            .iter()
            .map(|device| device.device_id.as_str())
            .collect::<Vec<_>>();
        record_event(json!({"event": "device_list", "deviceIds": device_ids}))?;
        return write_json(device_list_json(&fixture));
    }

    if args.len() == 5
        && args[0] == "term"
        && args[1] == "-d"
        && args[3] == "ls"
        && args[4] == "--json"
    {
        record_terminal_list(&fixture, args[2])?;
        return write_json(session_list_json(&fixture, args[2]));
    }

    if args.len() == 5 && args[0] == "term" && args[1] == "-d" && args[3] == "attach" {
        return run_attached_terminal(&fixture, args[2], args[4]);
    }

    Err(format!("unsupported official CLI arguments: {args:?}").into())
}

fn load_fixture() -> Result<Fixture, Box<dyn Error>> {
    let Some(path) = env::var_os("UU_TEST_FIXTURE_JSON") else {
        return Ok(Fixture {
            devices: vec![FixtureDevice {
                device_id: DEVICE_ID.to_owned(),
                device_name: DEVICE_NAME.to_owned(),
                is_online: true,
                platform: Some(json!("linux")),
                terminals: vec![FixtureTerminal {
                    name: SESSION_NAME.to_owned(),
                    shell: Some("bash".to_owned()),
                    state: "running".to_owned(),
                    created_at: Some(CREATED_AT.to_owned()),
                    attachable: true,
                }],
            }],
        });
    };

    let path = std::path::PathBuf::from(path);
    let bytes = fs::read(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot read UU_TEST_FIXTURE_JSON {}: {error}",
                path.display()
            ),
        )
    })?;
    let fixture = serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid UU_TEST_FIXTURE_JSON {}: {error}", path.display()),
        )
    })?;
    Ok(fixture)
}

fn device_list_json(fixture: &Fixture) -> Value {
    let devices = fixture
        .devices
        .iter()
        .map(|device| {
            json!({
                "deviceId": &device.device_id,
                "deviceName": &device.device_name,
                "isOnline": device.is_online,
                "platform": &device.platform
            })
        })
        .collect::<Vec<_>>();
    json!({"success": true, "data": devices, "error": null})
}

fn sessions_for_device<'a>(fixture: &'a Fixture, device_id: &str) -> Option<&'a FixtureDevice> {
    fixture
        .devices
        .iter()
        .find(|device| device.device_id == device_id)
}

fn session_list_json(fixture: &Fixture, device_id: &str) -> Value {
    let items: Vec<Value> = if let Some(device) = sessions_for_device(fixture, device_id) {
        device
            .terminals
            .iter()
            .map(|terminal| {
                json!({
                    "name": &terminal.name,
                    "deviceId": &device.device_id,
                    "deviceName": &device.device_name,
                    "shell": &terminal.shell,
                    "state": &terminal.state,
                    "createdAt": &terminal.created_at,
                    "attachable": terminal.attachable
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let total = items.len();
    json!({
        "success": true,
        "data": {"items": items, "total": total},
        "error": null
    })
}

fn session_event_value(device: &FixtureDevice, terminal: &FixtureTerminal) -> Value {
    json!({
        "deviceId": &device.device_id,
        "sessionName": &terminal.name,
        "createdAt": &terminal.created_at,
        "state": &terminal.state,
        "attachable": terminal.attachable,
        "available": session_is_available(device, terminal)
    })
}

fn record_terminal_list(fixture: &Fixture, device_id: &str) -> Result<(), Box<dyn Error>> {
    let sessions = sessions_for_device(fixture, device_id)
        .map(|device| {
            device
                .terminals
                .iter()
                .map(|terminal| session_event_value(device, terminal))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    record_event(json!({
        "event": "terminal_list",
        "deviceId": device_id,
        "sessions": sessions
    }))
}

fn resolve_attach(
    fixture: &Fixture,
    device_id: &str,
    session_name: &str,
) -> (Option<AttachIdentity>, bool, &'static str) {
    let Some(device) = sessions_for_device(fixture, device_id) else {
        return (None, false, "device not present in current fixture");
    };
    let mut matches = device
        .terminals
        .iter()
        .filter(|terminal| terminal.name == session_name);
    let Some(terminal) = matches.next() else {
        return (None, false, "session not present in current fixture");
    };
    if matches.next().is_some() {
        return (None, false, "session name is ambiguous on this device");
    }

    let identity = AttachIdentity {
        device_id: device.device_id.clone(),
        session_name: terminal.name.clone(),
        created_at: terminal.created_at.clone(),
    };
    let available = session_is_available(device, terminal);
    let reason = if available {
        "available"
    } else {
        "session is not online, running, and attachable"
    };
    (Some(identity), available, reason)
}

fn invocation_event(args: &[&str], fixture: &Fixture) -> Value {
    let mut event = json!({"event": "invoke", "args": args});
    if args.len() >= 3 && args[0] == "term" && args[1] == "-d" {
        event["deviceId"] = json!(args[2]);
        if args.len() == 5 && args[3] == "attach" {
            event["sessionName"] = json!(args[4]);
            let (identity, _, _) = resolve_attach(fixture, args[2], args[4]);
            event["createdAt"] = json!(identity.and_then(|identity| identity.created_at));
        }
    }
    event
}

fn write_json(value: Value) -> Result<u8, Box<dyn Error>> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(0)
}

fn run_attached_terminal(
    fixture: &Fixture,
    device_id: &str,
    session_name: &str,
) -> Result<u8, Box<dyn Error>> {
    let (identity, available, reason) = resolve_attach(fixture, device_id, session_name);
    record_event(json!({
        "event": "attach_started",
        "deviceId": device_id,
        "sessionName": session_name,
        "createdAt": identity.as_ref().and_then(|identity| identity.created_at.as_deref()),
        "available": available,
        "result": if available { "accepted" } else { "rejected" },
        "reason": reason
    }))?;
    let Some(identity) = identity.filter(|_| available) else {
        return Err(format!(
            "requested terminal {device_id}/{session_name} is unavailable: {reason}"
        )
        .into());
    };

    if env::var("UU_TEST_MODE").as_deref() == Ok("bash") {
        return run_git_bash_attached_terminal(identity);
    }

    let mut command = String::new();
    let read = io::stdin().lock().read_line(&mut command)?;
    if read == 0 {
        return Ok(0);
    }
    record_dispatch(&command, &identity)?;

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

#[cfg(windows)]
fn raw_fixture_console_input() -> io::Result<()> {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(kind: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }
    let input = unsafe { GetStdHandle(-10_i32 as u32) };
    let mut mode = 0;
    if unsafe { GetConsoleMode(input, &mut mode) } == 0 {
        // Direct fixture invocations may use a pipe instead of ConPTY.
        return Ok(());
    }
    // The fake attachment forwards bytes, just like a terminal transport.
    // Cooked console input would hold LF-terminated caller stdin until Enter.
    if unsafe { SetConsoleMode(input, mode & !0x0007) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn run_git_bash_attached_terminal(identity: AttachIdentity) -> Result<u8, Box<dyn Error>> {
    #[cfg(windows)]
    raw_fixture_console_input()?;
    let bash = env::var_os("UU_TEST_BASH")
        .ok_or("UU_TEST_BASH must name the Git Bash executable for bash mode")?;
    let mut command = Command::new(bash);
    if let Some(cwd) = env::var_os("UU_TEST_BASH_CWD") {
        command.current_dir(cwd);
    }
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
    let input_identity = identity.clone();
    let _input_relay = thread::Builder::new()
        .name("fake-uuyc-git-bash-input".to_owned())
        .spawn(move || {
            if let Err(error) = forward_conpty_input(io::stdin(), child_stdin, input_identity) {
                eprintln!("fake-uuyc: Git Bash input relay failed: {error}");
            }
        })?;
    let stdout_identity = identity.clone();
    let stdout_relay = thread::Builder::new()
        .name("fake-uuyc-git-bash-stdout".to_owned())
        .spawn(move || {
            if let Err(error) =
                relay_bash_output(child_stdout, io::stdout(), stdout_identity, "stdout")
            {
                eprintln!("fake-uuyc: Git Bash stdout relay failed: {error}");
            }
        })?;
    let stderr_identity = identity.clone();
    let stderr_relay = thread::Builder::new()
        .name("fake-uuyc-git-bash-stderr".to_owned())
        .spawn(move || {
            if let Err(error) =
                relay_bash_output(child_stderr, io::stderr(), stderr_identity, "stderr")
            {
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

fn relay_bash_output<R: Read, W: Write>(
    mut input: R,
    mut output: W,
    identity: AttachIdentity,
    stream: &str,
) -> Result<(), Box<dyn Error>> {
    let mut buffer = [0_u8; 8192];
    loop {
        let bytes_read = input.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        output.write_all(&buffer[..bytes_read])?;
        output.flush()?;
        record_fixture_diagnostic(json!({
            "event": "bash_output",
            "deviceId": &identity.device_id,
            "sessionName": &identity.session_name,
            "createdAt": &identity.created_at,
            "stream": stream,
            "hex": hex_encode(&buffer[..bytes_read])
        }))?;
    }
    output.flush()?;
    Ok(())
}
fn forward_conpty_input<R: Read, W: Write>(
    mut input: R,
    mut output: W,
    identity: AttachIdentity,
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
                forward_bash_byte(
                    forwarded,
                    &mut swallow_next_lf,
                    &mut line,
                    &mut output,
                    &identity,
                )?;
            }
            if possible_dsr.as_slice() == DSR_RESPONSE {
                possible_dsr.clear();
            }
        }
    }

    for byte in possible_dsr {
        forward_bash_byte(
            byte,
            &mut swallow_next_lf,
            &mut line,
            &mut output,
            &identity,
        )?;
    }
    if !line.is_empty() {
        dispatch_bash_line(&mut line, &mut output, false, &identity)?;
    }
    output.flush()?;
    Ok(())
}

fn forward_bash_byte<W: Write>(
    byte: u8,
    swallow_next_lf: &mut bool,
    line: &mut Vec<u8>,
    output: &mut W,
    identity: &AttachIdentity,
) -> Result<(), Box<dyn Error>> {
    if *swallow_next_lf {
        *swallow_next_lf = false;
        if byte == b'\n' {
            return Ok(());
        }
    }

    match byte {
        b'\r' => {
            dispatch_bash_line(line, output, true, identity)?;
            *swallow_next_lf = true;
        }
        b'\n' => dispatch_bash_line(line, output, true, identity)?,
        _ => line.push(byte),
    }
    Ok(())
}

fn dispatch_bash_line<W: Write>(
    line: &mut Vec<u8>,
    output: &mut W,
    terminated: bool,
    identity: &AttachIdentity,
) -> Result<(), Box<dyn Error>> {
    let input_bytes = line.len() + if terminated { 1 } else { 0 };
    output.write_all(line)?;
    if terminated {
        output.write_all(b"\n")?;
    }
    output.flush()?;
    let mut forwarded = line.to_vec();
    if terminated {
        forwarded.push(b'\n');
    }
    record_fixture_diagnostic(json!({
        "event": "terminal_input",
        "deviceId": &identity.device_id,
        "sessionName": &identity.session_name,
        "createdAt": &identity.created_at,
        "hex": hex_encode(&forwarded),
        "terminated": terminated
    }))?;
    if !line.is_empty() && is_terminal_wrapper(line) {
        record_event(json!({
            "event": "dispatch",
            "deviceId": &identity.device_id,
            "sessionName": &identity.session_name,
            "createdAt": &identity.created_at,
            "result": "sent",
            "transport": "git-bash-pipe",
            "input_bytes": input_bytes,
            "wrapper": String::from_utf8_lossy(line)
        }))?;
    }
    line.clear();
    Ok(())
}
fn is_terminal_wrapper(line: &[u8]) -> bool {
    line.starts_with(b"set +m; ( trap '__uu_status=$?;")
        || line.starts_with(b"printf '\\r\\n%s%s%s' 'UUC' '")
}

fn record_dispatch(wrapper: &str, identity: &AttachIdentity) -> Result<(), Box<dyn Error>> {
    record_event(json!({
        "event": "dispatch",
        "deviceId": &identity.device_id,
        "sessionName": &identity.session_name,
        "createdAt": &identity.created_at,
        "result": "sent",
        "wrapper": wrapper
    }))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn record_fixture_diagnostic(record: Value) -> Result<(), Box<dyn Error>> {
    let Some(path) = env::var_os("UU_TEST_FIXTURE_EVENTS") else {
        return Ok(());
    };
    append_fixture_event(std::path::Path::new(&path), record)
}

fn record_event(record: Value) -> Result<(), Box<dyn Error>> {
    let legacy_path = env::var_os("UU_TEST_LOG").map(std::path::PathBuf::from);
    let fixture_path = env::var_os("UU_TEST_FIXTURE_EVENTS").map(std::path::PathBuf::from);
    match (legacy_path, fixture_path) {
        (Some(legacy), Some(fixture)) if legacy == fixture => {
            append_fixture_event(&fixture, record)?;
        }
        (Some(legacy), Some(fixture)) => {
            append_legacy_event(&legacy, &record)?;
            append_fixture_event(&fixture, record)?;
        }
        (Some(legacy), None) => append_legacy_event(&legacy, &record)?,
        (None, Some(fixture)) => append_fixture_event(&fixture, record)?,
        (None, None) => {}
    }
    Ok(())
}

fn append_legacy_event(path: &std::path::Path, record: &Value) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut log = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut log, record)?;
    log.write_all(b"\n")?;
    log.flush()?;
    Ok(())
}

fn append_fixture_event(path: &std::path::Path, record: Value) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut log = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    FileExt::lock_exclusive(&log)?;

    let mut contents = String::new();
    log.read_to_string(&mut contents)?;
    let previous = contents
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|event| event.get("sequence").and_then(Value::as_u64))
        .max()
        .unwrap_or(0);
    let sequence = previous
        .checked_add(1)
        .ok_or("fixture event sequence overflow")?;

    let mut event = record;
    if let Value::Object(fields) = &mut event {
        fields.insert("sequence".to_owned(), json!(sequence));
    }
    log.seek(SeekFrom::End(0))?;
    let mut bytes = serde_json::to_vec(&event)?;
    bytes.push(b'\n');
    log.write_all(&bytes)?;
    log.flush()?;
    FileExt::unlock(&log)?;
    Ok(())
}
