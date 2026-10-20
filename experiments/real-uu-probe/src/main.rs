use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(feature = "candidate")]
#[allow(dead_code)]
mod candidate_job {
    include!(env!("CANDIDATE_JOB_PATH"));
}

struct Logs {
    human: File,
    raw: File,
    start: Instant,
}

impl Logs {
    fn new(path: &Path) -> Result<Self> {
        fs::create_dir_all(path)?;
        Ok(Self {
            human: File::create(path.join("human.log"))?,
            raw: File::create(path.join("raw.jsonl"))?,
            start: Instant::now(),
        })
    }
    fn event(&mut self, event: &str, details: Value) -> Result<()> {
        let record = json!({
            "unix_ms": SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            "elapsed_ms": self.start.elapsed().as_millis(),
            "event": event, "details": details
        });
        serde_json::to_writer(&mut self.raw, &record)?;
        writeln!(self.raw)?;
        self.raw.flush()?;
        Ok(())
    }
    fn note(&mut self, note: &str) -> Result<()> {
        let line = format!("[{:>8.3}s] {note}", self.start.elapsed().as_secs_f64());
        println!("{line}");
        writeln!(self.human, "{line}")?;
        self.human.flush()?;
        Ok(())
    }
    fn bytes(&mut self, direction: &str, bytes: &[u8]) -> Result<()> {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        self.event(direction, json!({"length": bytes.len(), "hex": hex}))?;
        self.note(&format!(
            "{direction} {bytes:?} | {:?}",
            String::from_utf8_lossy(bytes)
        ))
    }
}

fn official(cli: &str, args: &[&str]) -> Result<String> {
    let child = Command::new(cli)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Query operations only. Never create, kill or rename an official session.
    let output = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        bail!(
            "official query {:?}: {} {}",
            output.status,
            text,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(text)
}

fn query(cli: &str, args: &[&str]) -> Result<Value> {
    let text = official(cli, args)?;
    // UU emits connection progress before the management JSON.
    let start = text.find('{').context("official query returned no JSON")?;
    let value: Value = serde_json::from_str(text[start..].trim())?;
    if value["success"] != true {
        bail!("official query failed: {}", value["error"]);
    }
    Ok(value)
}

fn items(value: &Value) -> Vec<Value> {
    value["data"]["items"]
        .as_array()
        .or_else(|| value["data"].as_array())
        .cloned()
        .unwrap_or_default()
}

fn list(cli: &str) -> Result<()> {
    for device in items(&query(cli, &["device", "list", "--json"])?) {
        println!(
            "Device: {} | online={} | identity={}",
            device["deviceName"], device["isOnline"], device["deviceId"]
        );
        if device["isOnline"] == true {
            let id = device["deviceId"]
                .as_str()
                .context("device identity missing")?;
            for session in items(&query(cli, &["term", "-d", id, "ls", "--json"])?) {
                println!(
                    "  Terminal: {} | shell={} | state={} | createdAt={}",
                    session["name"], session["shell"], session["state"], session["createdAt"]
                );
            }
        }
    }
    Ok(())
}

fn transmit(writer: &mut dyn Write, logs: &mut Logs, bytes: &[u8]) -> Result<()> {
    logs.bytes("TX_ATTEMPT", bytes)?;
    writer.write_all(bytes)?;
    writer.flush()?;
    logs.event("TX_WRITTEN", json!({"length": bytes.len()}))
}

fn attach(cli: &str, device: &str, session: &str, created: &str, log_dir: &Path) -> Result<()> {
    let selected = items(&query(cli, &["term", "-d", device, "ls", "--json"])?)
        .into_iter()
        .find(|item| item["name"] == session && item["createdAt"] == created)
        .context("selected terminal instance disappeared/recreated; refusing attach")?;
    let mut logs = Logs::new(log_dir)?;
    logs.event("environment", json!({"os":std::env::consts::OS, "cli":cli, "version":official(cli, &["version"]).unwrap_or_else(|e| e.to_string()), "session":selected, "pty":{"rows":24,"cols":120}, "candidate_head":"ae772ea9", "observer_revision":option_env!("PROBE_BUILD_SHA").unwrap_or("local")}))?;
    logs.note("Starting one official attach; startup/liveness alone is not attach success")?;
    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let master = pair.master;
    let slave = pair.slave;
    let mut command = CommandBuilder::new(cli);
    command.args(["term", "-d", device, "attach", session]);
    let mut child = slave.spawn_command(command)?;
    let mut writer = master.take_writer()?;
    let mut reader = master.try_clone_reader()?;
    drop(slave);
    logs.event("attach_spawned", json!({"pid":child.process_id()}))?;

    let (rx_tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = rx_tx.send(Ok(Vec::new()));
                    break;
                }
                Ok(count) => {
                    if rx_tx.send(Ok(buffer[..count].to_vec())).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = rx_tx.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });
    let (input_tx, input) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            match line {
                Ok(line) => {
                    if input_tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    logs.note(
        "Local controls: pwd hello exit7 second unicode stream sleep interrupt interrupt-key after status quit; guard-<command> or candidate-<command> selects an experiment",
    )?;
    let mut startup = Vec::new();
    let mut all_rx = Vec::new();
    let mut pending: Option<(String, String, Instant)> = None;
    #[cfg(feature = "candidate")]
    let mut candidate_decoder: Option<candidate_job::CompletionDecoder> = None;
    let mut sequence = 0u32;
    let mut any_command_sent = false;
    let mut win32_input_requested = false;
    let mut last_status = Instant::now();
    let mut process_exited = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(bytes)) if bytes.is_empty() => {
                logs.note("RX EOF; no inferred command result")?;
                break;
            }
            Ok(Ok(bytes)) => {
                // Log before any framing or startup interpretation.
                logs.bytes("RX", &bytes)?;
                all_rx.extend_from_slice(&bytes);
                #[cfg(feature = "candidate")]
                let candidate_active = candidate_decoder.is_some();
                #[cfg(not(feature = "candidate"))]
                let candidate_active = false;
                #[cfg(feature = "candidate")]
                if let Some(decoder) = &mut candidate_decoder {
                    let output = match decoder.push(&bytes) {
                        Ok(output) => output,
                        Err(error) => {
                            logs.event(
                                "candidate_decode_error",
                                json!({"error":error.to_string()}),
                            )?;
                            logs.note(
                                "Candidate protocol error; outcome unknown, no further send",
                            )?;
                            break;
                        }
                    };
                    if !output.is_empty() {
                        logs.bytes("CANDIDATE_OUTPUT", &output)?;
                    }
                    if let Some(code) = decoder.completed() {
                        if let Some((nonce, label, sent)) = pending.take() {
                            logs.event("candidate_completion", json!({"label":label,"nonce":nonce,"exit":code,"elapsed_ms":sent.elapsed().as_millis(),"candidate_sha":env!("PROBE_CANDIDATE_SHA")}))?;
                            logs.note(&format!("Candidate decoder: {label} exit={code}; production wrapper/decoder, not the visible-token observer"))?;
                        }
                    }
                }
                if !any_command_sent {
                    startup.extend_from_slice(&bytes);
                    if !win32_input_requested
                        && startup.windows(8).any(|part| part == b"\x1b[?9001h")
                    {
                        win32_input_requested = true;
                        logs.note("Observed ConPTY Win32 input-mode request")?;
                    }
                    while let Some(index) = startup.windows(4).position(|part| part == b"\x1b[6n") {
                        startup.drain(..index + 4);
                        transmit(&mut *writer, &mut logs, b"\x1b[1;1R")?;
                        logs.note(
                            "Answered ConPTY inherited-cursor query; not evidence of remote attach",
                        )?;
                    }
                    if startup.len() > 64 {
                        startup.drain(..startup.len() - 64);
                    }
                }
                if let Some((nonce, label, sent)) = pending.as_ref().filter(|_| !candidate_active) {
                    // Observation only: ConPTY may replace the separator CRLF
                    // with cursor addressing. Retain that failure independently.
                    let begin = format!("UUC{nonce}B");
                    let end = format!("UUC{nonce}E");
                    if let Some(begin_index) = all_rx
                        .windows(begin.len())
                        .position(|p| p == begin.as_bytes())
                    {
                        let payload_start = begin_index + begin.len();
                        if let Some(relative_index) = all_rx[payload_start..]
                            .windows(end.len())
                            .position(|p| p == end.as_bytes())
                        {
                            let index = payload_start + relative_index;
                            let rest = &all_rx[index + end.len()..];
                            if let Some(finish) = rest.iter().position(|b| *b == b'Z') {
                                if let Ok(code) = std::str::from_utf8(&rest[..finish])
                                    .unwrap_or("")
                                    .parse::<u16>()
                                {
                                    if code <= 255 {
                                        let literal_begin = begin_index >= 2
                                            && &all_rx[begin_index - 2..begin_index] == b"\r\n";
                                        let literal_end =
                                            index >= 2 && &all_rx[index - 2..index] == b"\r\n";
                                        let payload_hex: String = all_rx[payload_start..index]
                                            .iter()
                                            .map(|b| format!("{b:02x}"))
                                            .collect();
                                        logs.event("visible_nonce_completion", json!({"label":label,"nonce":nonce,"exit":code,"elapsed_ms":sent.elapsed().as_millis(),"literal_crlf_begin":literal_begin,"literal_crlf_end":literal_end,"raw_payload_hex":payload_hex}))?;
                                        logs.note(&format!("Visible ordered nonce markers: {label} exit={code}, elapsed={:.3}s; original CRLF decoder compatible={} (raw output is unmodified)", sent.elapsed().as_secs_f64(), literal_begin && literal_end))?;
                                        pending = None;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(Err(error)) => {
                logs.note(&format!("RX error: {error}; command outcome is unknown"))?;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                logs.note("RX reader ended")?;
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if last_status.elapsed() >= Duration::from_secs(1) {
            let status = child.try_wait()?;
            logs.event(
                "process_state",
                json!({"pid":child.process_id(), "status":format!("{status:?}")}),
            )?;
            if status.is_some() && !process_exited {
                process_exited = true;
                logs.note(&format!(
                    "Attach child exited: {status:?}; no replay; awaiting any final RX"
                ))?;
            }
            last_status = Instant::now();
        }
        while let Ok(control) = input.try_recv() {
            let control = control.trim();
            if control == "quit" {
                logs.note("Operator ended probe: close only this local ConPTY attachment; no remote exit/kill/rename command")?;
                logs.event(
                    "probe_end",
                    json!({"pending":pending.as_ref().map(|p|&p.1)}),
                )?;
                drop(writer);
                drop(master);
                return Ok(());
            }
            if control == "status" {
                logs.note(&format!(
                    "Process={:?}; pending={:?}",
                    child.try_wait()?,
                    pending.as_ref().map(|p| &p.1)
                ))?;
                continue;
            }
            if process_exited {
                logs.note("No send: attach process has exited")?;
                continue;
            }
            if control == "interrupt" || control == "interrupt-key" {
                if pending.is_none() {
                    logs.note("No interrupt sent: no active probe command")?;
                    continue;
                }
                if control == "interrupt-key" {
                    if !win32_input_requested {
                        logs.note("No key events sent: Win32 input-mode request not observed")?;
                        continue;
                    }
                    // Microsoft Terminal's CSI Vk;Sc;Uc;Kd;Cs;Rc_ protocol:
                    // left Ctrl down, C down/up with ETX, then left Ctrl up.
                    // This writes only our own ConPTY input, not a GUI key event.
                    transmit(
                        &mut *writer,
                        &mut logs,
                        b"\x1b[17;29;0;1;8;1_\x1b[67;46;3;1;8;1_\x1b[67;46;3;0;8;1_\x1b[17;29;0;0;0;1_",
                    )?;
                    logs.note("Win32 Ctrl+C key sequence sent; actual remote interruption still requires observation")?;
                } else {
                    transmit(&mut *writer, &mut logs, &[3])?;
                    logs.note(
                        "One ETX byte sent; actual remote interruption still requires observation",
                    )?;
                }
                continue;
            }
            if pending.is_some() {
                logs.note("No send: previous command completion has not been observed")?;
                continue;
            }
            let candidate = control.starts_with("candidate-");
            let guarded = control.starts_with("guard-");
            let command_name = control
                .strip_prefix("candidate-")
                .or_else(|| control.strip_prefix("guard-"))
                .unwrap_or(control);
            let shell = match command_name {
                "pwd" => "pwd",
                "hello" => "printf 'hello-uu\\n'",
                "exit7" => "sh -c 'exit 7'",
                "second" => "printf 'second-command\\n'",
                "unicode" => "printf '你好-UU\\n'",
                "stream" => "printf 'tick-1\\n'; sleep 2; printf 'tick-2\\n'",
                "sleep" => "sleep 30",
                "after" => "printf 'after-interrupt\\n'",
                _ => {
                    logs.note("Unknown local control; nothing sent")?;
                    continue;
                }
            };
            sequence += 1;
            let nonce = format!("probe{:x}{sequence:x}", std::process::id());
            let escaped = format!("'{}'", shell.replace('\'', "'\"'\"'"));
            // Baseline candidate retained for comparison. The guard experiment
            // changes only this dedicated shell's monitor baseline and per-job
            // traps. Inner eval cannot replace the shell that owns the EXIT trap.
            #[cfg(feature = "candidate")]
            if !candidate {
                candidate_decoder = None;
            }
            let mut wrapper = if candidate {
                #[cfg(feature = "candidate")]
                {
                    candidate_decoder =
                        Some(candidate_job::CompletionDecoder::new_console(&nonce)?);
                    logs.note(
                        "Candidate job uses explicit test remote root /; no binding or pool",
                    )?;
                    candidate_job::wrap_job("/", shell, &nonce)?
                }
                #[cfg(not(feature = "candidate"))]
                {
                    logs.note(
                        "Candidate validation was not compiled into this probe; nothing sent",
                    )?;
                    continue;
                }
            } else if guarded {
                format!("set +m; ( trap '__uu_status=$?; printf \"\\r\\n%s%s%s%d%s\\r\\n\" UUC {nonce} E \"$__uu_status\" Z' EXIT; trap 'exit 130' INT; printf '\\r\\n%s%s%s\\r\\n' UUC {nonce} B; ( eval {escaped} ) )\r")
            } else {
                format!("printf '\\r\\n%s%s%s' 'UUC' '{nonce}' 'B'; ( eval {escaped} ); __uu_status=$?; printf '\\r\\n%s%s%s%d%s' 'UUC' '{nonce}' 'E' \"$__uu_status\" 'Z'\r")
            };
            // Match the product transport's Enter representation, not raw LF.
            if wrapper.ends_with('\n') {
                wrapper.pop();
                wrapper.push('\r');
            }
            logs.note(&format!(
                "Command {control} sending once: {shell}; nonce={nonce}"
            ))?;
            logs.event(
                "command",
                json!({"label":control,"shell":shell,"nonce":nonce,"guarded":guarded,"candidate":candidate}),
            )?;
            pending = Some((nonce, control.to_string(), Instant::now()));
            any_command_sent = true;
            all_rx.clear();
            transmit(&mut *writer, &mut logs, wrapper.as_bytes())?;
        }
    }
    logs.note("Probe receiver stopped; no automatic repair, reconnect or command replay")?;
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("list") if args.len() == 3 => list(&args[2]),
        Some("attach") if args.len() == 7 => attach(&args[2], &args[3], &args[4], &args[5], Path::new(&args[6])),
        _ => bail!("usage: real-uu-probe list <cli> | attach <cli> <device> <session> <createdAt> <log-directory>"),
    }
}
