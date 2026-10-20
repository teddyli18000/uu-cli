use crate::discovery::locate_official_cli;
use crate::model::{DeviceInfo, SessionInfo};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliVersionInfo {
    pub cli_version: String,
    pub app_version: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OfficialCli {
    path: PathBuf,
}

impl OfficialCli {
    pub fn locate() -> Result<Self> {
        Self::new(locate_official_cli()?)
    }

    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if !path.is_file() {
            bail!("official UU CLI not found at {}", path.display());
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn version(&self) -> Result<CliVersionInfo> {
        let output = self.run([OsStr::new("version")])?;
        parse_version_output(&output)
    }

    pub fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let output = self.run([
            OsStr::new("device"),
            OsStr::new("list"),
            OsStr::new("--json"),
        ])?;
        parse_device_output(&output)
    }

    pub fn list_sessions(&self, device_identity: &str) -> Result<Vec<SessionInfo>> {
        let output = self.run([
            OsStr::new("term"),
            OsStr::new("-d"),
            OsStr::new(device_identity),
            OsStr::new("ls"),
            OsStr::new("--json"),
        ])?;
        parse_session_output(&output)
    }

    fn run<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let working_dir = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("official UU CLI path has no parent directory"))?;
        Command::new(&self.path)
            .args(args)
            .current_dir(working_dir)
            .output()
            .with_context(|| format!("failed to run official UU CLI at {}", self.path.display()))
    }
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    success: bool,
    data: Option<T>,
    error: Option<CliError>,
}

#[derive(Debug, Deserialize)]
struct CliError {
    code: Option<i64>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DevicePayload {
    Page(DevicePage),
    Items(Vec<DeviceItem>),
}

#[derive(Debug, Deserialize)]
struct DevicePage {
    #[serde(default, alias = "items")]
    devices: Vec<DeviceItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceItem {
    device_id: String,
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    is_online: bool,
    platform: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct SessionPage {
    #[serde(default)]
    items: Vec<SessionItem>,
    #[allow(dead_code)]
    total: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionItem {
    name: String,
    device_id: String,
    #[serde(default)]
    device_name: String,
    shell: Option<String>,
    state: String,
    created_at: Option<String>,
    attachable: Option<bool>,
}

fn parse_version_output(output: &Output) -> Result<CliVersionInfo> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = if stdout.trim().is_empty() {
        stderr.as_ref()
    } else {
        stdout.as_ref()
    };

    let mut cli_version = None;
    let mut app_version = None;
    let mut message = None;

    for line in combined.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "cliVersion" if !value.is_empty() && value != "null" => {
                cli_version = Some(value.to_owned())
            }
            "appVersion" if !value.is_empty() && value != "null" => {
                app_version = Some(value.to_owned())
            }
            "message" if !value.is_empty() && value != "null" => message = Some(value.to_owned()),
            _ => {}
        }
    }

    let cli_version = cli_version.ok_or_else(|| {
        anyhow!(
            "unexpected output from official UU CLI version command (exit {:?}): {}",
            output.status.code(),
            concise_error(&stdout, &stderr)
        )
    })?;

    Ok(CliVersionInfo {
        cli_version,
        app_version,
        message,
    })
}

fn parse_device_output(output: &Output) -> Result<Vec<DeviceInfo>> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let json = extract_json(&stdout)
        .or_else(|| extract_json(&stderr))
        .ok_or_else(|| {
            anyhow!(
                "official UU CLI returned no device JSON (exit {:?}): {}",
                output.status.code(),
                concise_error(&stdout, &stderr)
            )
        })?;

    let envelope: Envelope<DevicePayload> = serde_json::from_str(json)
        .with_context(|| format!("invalid device-list JSON from official UU CLI: {json}"))?;
    ensure_success(&envelope)?;

    let payload = envelope
        .data
        .ok_or_else(|| anyhow!("official UU CLI reported success without device-list data"))?;
    let items = match payload {
        DevicePayload::Page(page) => page.devices,
        DevicePayload::Items(items) => items,
    };

    Ok(items
        .into_iter()
        .map(|item| DeviceInfo {
            device_identity: item.device_id,
            device_name: item.device_name,
            is_online: item.is_online,
            platform: item.platform.and_then(platform_text),
        })
        .collect())
}

fn platform_text(value: Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn parse_session_output(output: &Output) -> Result<Vec<SessionInfo>> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let json = extract_json(&stdout)
        .or_else(|| extract_json(&stderr))
        .ok_or_else(|| {
            anyhow!(
                "official UU CLI returned no JSON (exit {:?}): {}",
                output.status.code(),
                concise_error(&stdout, &stderr)
            )
        })?;

    let envelope: Envelope<SessionPage> = serde_json::from_str(json)
        .with_context(|| format!("invalid terminal-list JSON from official UU CLI: {json}"))?;
    ensure_success(&envelope)?;

    let page = envelope
        .data
        .ok_or_else(|| anyhow!("official UU CLI reported success without terminal-list data"))?;

    Ok(page
        .items
        .into_iter()
        .map(|item| SessionInfo {
            device_identity: item.device_id,
            device_name: item.device_name,
            session_name: item.name,
            shell: item.shell,
            state: item.state,
            created_at: item.created_at,
            attachable: item.attachable,
        })
        .collect())
}

fn ensure_success<T>(envelope: &Envelope<T>) -> Result<()> {
    if envelope.success {
        return Ok(());
    }
    let (code, message) = envelope
        .error
        .as_ref()
        .map(|error| (error.code, error.message.as_deref()))
        .unwrap_or((None, None));
    let message = message.unwrap_or("unknown official UU CLI error");
    if let Some(code) = code {
        bail!("official UU CLI error {code}: {message}");
    }
    bail!("official UU CLI error: {message}")
}

fn extract_json(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (start < end).then_some(&trimmed[start..=end])
}

fn concise_error(stdout: &str, stderr: &str) -> String {
    let combined = format!("{} {}", stdout.trim(), stderr.trim());
    let one_line = combined.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.len() <= 300 {
        one_line
    } else {
        format!("{}…", &one_line[..300])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::ExitStatus;

    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;

    #[cfg(unix)]
    fn status(code: u32) -> ExitStatus {
        ExitStatus::from_raw((code as i32) << 8)
    }

    #[cfg(windows)]
    fn status(code: u32) -> ExitStatus {
        ExitStatus::from_raw(code)
    }

    #[test]
    fn parses_observed_version_output_when_main_app_is_stopped() {
        let output = Output {
            status: status(0),
            stdout: b"cliVersion: 4.43.0.3607\nbuildNumber: null\nappVersion: null\nmessage: Main application is not running\n".to_vec(),
            stderr: vec![],
        };
        let version = parse_version_output(&output).unwrap();
        assert_eq!(version.cli_version, "4.43.0.3607");
        assert_eq!(version.app_version, None);
        assert_eq!(
            version.message.as_deref(),
            Some("Main application is not running")
        );
    }

    #[test]
    fn parses_device_list_page() {
        let output = Output {
            status: status(0),
            stdout: br#"{"success":true,"data":{"devices":[{"deviceId":"dev-1","deviceName":"AIC-Car","isOnline":true,"platform":4}]},"error":null,"timestamp":"now"}"#.to_vec(),
            stderr: vec![],
        };
        let devices = parse_device_output(&output).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_identity, "dev-1");
        assert_eq!(devices[0].device_name, "AIC-Car");
        assert!(devices[0].is_online);
        assert_eq!(devices[0].platform.as_deref(), Some("4"));
    }

    #[test]
    fn parses_device_list_array_payload() {
        let output = Output {
            status: status(0),
            stdout: br#"{"success":true,"data":[{"deviceId":"dev-2","deviceName":"Car-B","isOnline":false,"platform":"linux"}],"error":null}"#.to_vec(),
            stderr: vec![],
        };
        let devices = parse_device_output(&output).unwrap();
        assert_eq!(devices[0].device_identity, "dev-2");
        assert!(!devices[0].is_online);
        assert_eq!(devices[0].platform.as_deref(), Some("linux"));
    }

    #[test]
    fn parses_observed_terminal_wrapper() {
        let output = Output {
            status: status(0),
            stdout: br#"{"success":true,"data":{"items":[{"name":"bash-3","deviceId":"dev-1","deviceName":"AIC-Car","shell":"/bin/bash","state":"running","createdAt":"2026-10-19T17:27:45Z","attachable":true}],"total":1},"error":null,"timestamp":"now"}"#.to_vec(),
            stderr: vec![],
        };
        let sessions = parse_session_output(&output).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].device_identity, "dev-1");
        assert_eq!(sessions[0].session_name, "bash-3");
        assert_eq!(sessions[0].attachable, Some(true));
        assert!(sessions[0].authorization_fingerprint().is_some());
    }

    #[test]
    fn preserves_official_error_code_and_message() {
        let output = Output {
            status: status(1),
            stdout: br#"{"success":false,"data":null,"error":{"code":105,"message":"Timeout waiting for terminal environment check"},"timestamp":"now"}"#.to_vec(),
            stderr: vec![],
        };
        let error = parse_session_output(&output).unwrap_err().to_string();
        assert!(error.contains("105"));
        assert!(error.contains("Timeout waiting for terminal environment check"));
    }

    #[test]
    fn tolerates_non_json_noise_around_wrapper() {
        let output = Output {
            status: status(0),
            stdout:
                b"notice\n{\"success\":true,\"data\":{\"items\":[],\"total\":0},\"error\":null}\n"
                    .to_vec(),
            stderr: vec![],
        };
        assert!(parse_session_output(&output).unwrap().is_empty());
    }
}
