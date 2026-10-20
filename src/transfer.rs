//! Binary-safe file transfer over the command executor used by the worker.
//!
//! The remote endpoint needs a POSIX shell plus base64, wc, cut, sha256sum, and mv.
//! No shell state is retained between jobs: every command resolves its path from
//! the current job's remote_root and uses a unique adjacent temporary file.

use crate::job::shell_quote;
use crate::runner::{JobResult, JobRunError};
use sha2::{Digest, Sha256};
use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const UPLOAD_CHUNK_SIZE: usize = 768;
const IO_BUFFER_SIZE: usize = 64 * 1024;
static NEXT_NONCE: AtomicU64 = AtomicU64::new(0);

/// A successful transfer, including the bytes and digest verified on both ends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferReceipt {
    pub size: u64,
    pub sha256: String,
}

/// Runs one isolated remote job and writes its merged output to the supplied writer.
pub type JobExecutor<'a> = dyn FnMut(&str, &mut dyn Write) -> Result<JobResult, JobRunError> + 'a;

/// Uploads a local file to the remote workspace.
///
/// Relative remote paths are rooted at the job's remote_root. Absolute paths remain
/// absolute, and "~" / "~/..." are resolved using the remote HOME. Data is uploaded
/// in bounded base64 chunks; the destination is replaced only after size and SHA-256
/// verification, using a same-directory rename.
pub fn put(
    local_path: &Path,
    remote_path: &str,
    executor: &mut JobExecutor<'_>,
) -> Result<TransferReceipt, JobRunError> {
    if remote_path.is_empty() {
        return Err(preparation("remote destination path is empty"));
    }
    let mut input = File::open(local_path)
        .map_err(|error| preparation(format!("cannot open local source: {error}")))?;
    let nonce = nonce();
    let create_command = remote_command(
        remote_path,
        &nonce,
        r#"test ! -d "$uu_cli_dest" || { echo "uu-cli: destination is a directory" >&2; exit 1; }; (umask 077; set -C; : > "$uu_cli_tmp")"#,
    );
    run_remote(executor, &create_command, &mut io::sink())?;

    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut pending = Vec::with_capacity(UPLOAD_CHUNK_SIZE * 2);
    let mut read_buffer = [0u8; UPLOAD_CHUNK_SIZE];

    loop {
        let count = input
            .read(&mut read_buffer)
            .map_err(|error| preparation(format!("cannot read local source: {error}")))?;
        if count == 0 {
            break;
        }
        pending.extend_from_slice(&read_buffer[..count]);
        while pending.len() >= UPLOAD_CHUNK_SIZE {
            upload_chunk(
                &pending[..UPLOAD_CHUNK_SIZE],
                remote_path,
                &nonce,
                executor,
                &mut hasher,
                &mut size,
            )?;
            pending.drain(..UPLOAD_CHUNK_SIZE);
        }
    }
    if !pending.is_empty() {
        upload_chunk(
            &pending,
            remote_path,
            &nonce,
            executor,
            &mut hasher,
            &mut size,
        )?;
    }

    let digest = hex_digest(hasher.finalize().as_slice());
    let verify_and_publish = remote_command(
        remote_path,
        &nonce,
        &format!(
            r#"actual_size=$(wc -c < "$uu_cli_tmp" | tr -d '[:space:]'); [ "$actual_size" = '{}' ] || {{ echo "uu-cli: remote byte count mismatch" >&2; exit 1; }}; actual_hash=$(sha256sum "$uu_cli_tmp" | cut -d ' ' -f 1); [ "$actual_hash" = '{}' ] || {{ echo "uu-cli: remote SHA-256 mismatch" >&2; exit 1; }}; test ! -d "$uu_cli_dest" || {{ echo "uu-cli: destination became a directory" >&2; exit 1; }}; mv -f "$uu_cli_tmp" "$uu_cli_dest""#,
            size, digest
        ),
    );
    run_remote(executor, &verify_and_publish, &mut io::sink())?;

    Ok(TransferReceipt {
        size,
        sha256: digest,
    })
}

/// Downloads a remote file into a local path.
///
/// The remote file is first described by size and SHA-256. Its base64 stream is
/// decoded incrementally into a unique temporary file beside the destination. The
/// local destination is atomically replaced only after the stream and digest verify.
pub fn get(
    remote_path: &str,
    local_path: &Path,
    executor: &mut JobExecutor<'_>,
) -> Result<TransferReceipt, JobRunError> {
    if remote_path.is_empty() {
        return Err(preparation("remote source path is empty"));
    }
    if local_path.is_dir() {
        return Err(preparation("local destination is a directory"));
    }

    let nonce = nonce();
    let metadata_command = remote_command(
        remote_path,
        &nonce,
        r#"test -f "$uu_cli_dest" || { echo "uu-cli: remote source is not a regular file" >&2; exit 1; }; remote_size=$(wc -c < "$uu_cli_dest" | tr -d '[:space:]'); remote_hash=$(sha256sum "$uu_cli_dest" | cut -d ' ' -f 1); printf '%s %s\n' "$remote_size" "$remote_hash""#,
    );
    let mut metadata = BoundedWriter::new(256);
    run_remote(executor, &metadata_command, &mut metadata)?;
    if metadata.exceeded {
        return Err(completed("remote metadata output exceeded its limit"));
    }
    let (expected_size, expected_hash) = parse_metadata(&metadata.bytes)?;

    let mut temporary = LocalTemp::create(local_path)?;
    let stream_command = remote_command(remote_path, &nonce, r#"base64 "$uu_cli_dest""#);
    let (actual_size, actual_hash) = {
        let mut decoder = Base64Decoder::new(&mut temporary.file);
        let result = executor(&stream_command, &mut decoder)?;
        if result.exit_code != 0 {
            return Err(completed(format!(
                "remote download command exited with status {}",
                result.exit_code
            )));
        }
        decoder.finish().map_err(|error| match error {
            DecodeError::Invalid(message) => completed(message),
            DecodeError::Local(error) => {
                preparation(format!("cannot write local temporary file: {error}"))
            }
        })?
    };
    if actual_size != expected_size || actual_hash != expected_hash {
        return Err(completed(format!(
            "download integrity check failed (expected {expected_size} bytes / {expected_hash}, received {actual_size} bytes / {actual_hash})"
        )));
    }

    temporary
        .file
        .flush()
        .and_then(|_| temporary.file.sync_all())
        .map_err(|error| preparation(format!("cannot flush local temporary file: {error}")))?;
    fs::rename(&temporary.path, local_path).map_err(|error| {
        preparation(format!(
            "cannot atomically replace local destination: {error}"
        ))
    })?;
    temporary.keep = true;

    Ok(TransferReceipt {
        size: actual_size,
        sha256: actual_hash,
    })
}

fn upload_chunk(
    bytes: &[u8],
    remote_path: &str,
    nonce: &str,
    executor: &mut JobExecutor<'_>,
    hasher: &mut Sha256,
    size: &mut u64,
) -> Result<(), JobRunError> {
    let encoded = base64_encode(bytes);
    let command = remote_command(
        remote_path,
        nonce,
        &format!(
            r#"printf '%s' {} | base64 -d >> "$uu_cli_tmp""#,
            shell_quote(&encoded)
        ),
    );
    run_remote(executor, &command, &mut io::sink())?;
    hasher.update(bytes);
    *size = size
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| preparation("local source exceeds the maximum supported size"))?;
    Ok(())
}

fn run_remote(
    executor: &mut JobExecutor<'_>,
    command: &str,
    output: &mut dyn Write,
) -> Result<(), JobRunError> {
    let result = executor(command, output)?;
    if result.exit_code != 0 {
        return Err(completed(format!(
            "remote transfer command exited with status {}",
            result.exit_code
        )));
    }
    Ok(())
}

fn remote_command(remote_path: &str, nonce: &str, body: &str) -> String {
    let quoted_path = shell_quote(remote_path);
    format!(
        r#"set -eu; uu_cli_remote_input={quoted_path}; case "$uu_cli_remote_input" in '~') [ -n "$HOME" ] || {{ echo "uu-cli: remote HOME is unset" >&2; exit 1; }}; uu_cli_dest="$HOME";; '~/'*) [ -n "$HOME" ] || {{ echo "uu-cli: remote HOME is unset" >&2; exit 1; }}; uu_cli_rest=$(printf '%s' "$uu_cli_remote_input" | cut -c 3-); uu_cli_dest="$HOME/$uu_cli_rest";; /*) uu_cli_dest="$uu_cli_remote_input";; *) uu_cli_dest="$PWD/$uu_cli_remote_input";; esac; uu_cli_tmp="$uu_cli_dest.uu-cli-{nonce}.part"; {body}"#
    )
}

fn parse_metadata(output: &[u8]) -> Result<(u64, String), JobRunError> {
    let text = std::str::from_utf8(output)
        .map_err(|_| completed("remote metadata was not valid UTF-8"))?;
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.len() != 2 {
        return Err(completed("remote metadata had an unexpected format"));
    }
    let size = fields[0]
        .parse::<u64>()
        .map_err(|_| completed("remote file size was invalid"))?;
    let hash = fields[1];
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(completed("remote SHA-256 digest was invalid"));
    }
    Ok((size, hash.to_ascii_lowercase()))
}

fn nonce() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        "{timestamp:x}-{:x}-{:x}",
        std::process::id(),
        NEXT_NONCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn preparation(message: impl Display) -> JobRunError {
    JobRunError::Preparation(anyhow::anyhow!("{message}"))
}

fn completed(message: impl Display) -> JobRunError {
    JobRunError::Completed(anyhow::anyhow!("{message}"))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(((bytes.len() + 2) / 3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit),
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        let accepted = remaining.min(input.len());
        self.bytes.extend_from_slice(&input[..accepted]);
        self.exceeded |= accepted != input.len();
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct LocalTemp {
    path: PathBuf,
    file: File,
    keep: bool,
}

impl LocalTemp {
    fn create(destination: &Path) -> Result<Self, JobRunError> {
        let file_name = destination
            .file_name()
            .ok_or_else(|| preparation("local destination must name a file"))?;
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut last_error = None;
        for attempt in 0..16 {
            let mut name = file_name.to_os_string();
            name.push(format!(".uu-cli-{}-{attempt}.part", nonce()));
            let path = parent.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        keep: false,
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    last_error = Some(error);
                }
                Err(error) => {
                    return Err(preparation(format!(
                        "cannot create local temporary file: {error}"
                    )))
                }
            }
        }
        Err(preparation(format!(
            "cannot allocate a unique local temporary file: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "name collision".to_owned())
        )))
    }
}

impl Drop for LocalTemp {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug)]
enum DecodeError {
    Invalid(String),
    Local(io::Error),
}

struct Base64Decoder<'a> {
    file: &'a mut File,
    quartet: [u8; 4],
    quartet_len: usize,
    ended: bool,
    size: u64,
    hasher: Sha256,
    error: Option<DecodeError>,
}

impl<'a> Base64Decoder<'a> {
    fn new(file: &'a mut File) -> Self {
        Self {
            file,
            quartet: [0; 4],
            quartet_len: 0,
            ended: false,
            size: 0,
            hasher: Sha256::new(),
            error: None,
        }
    }

    fn finish(self) -> Result<(u64, String), DecodeError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.quartet_len != 0 {
            return Err(DecodeError::Invalid(
                "remote base64 stream ended mid-quantum".to_owned(),
            ));
        }
        Ok((self.size, hex_digest(self.hasher.finalize().as_slice())))
    }

    fn accept(&mut self, byte: u8) {
        if self.error.is_some() {
            return;
        }
        if matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
            return;
        }
        if self.ended {
            self.error = Some(DecodeError::Invalid(
                "remote base64 stream had data after padding".to_owned(),
            ));
            return;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 64,
            _ => {
                self.error = Some(DecodeError::Invalid(
                    "remote base64 stream contained an invalid character".to_owned(),
                ));
                return;
            }
        };
        self.quartet[self.quartet_len] = value;
        self.quartet_len += 1;
        if self.quartet_len == 4 {
            self.decode_quartet();
        }
    }

    fn decode_quartet(&mut self) {
        let [a, b, c, d] = self.quartet;
        if a == 64 || b == 64 || (c == 64 && d != 64) {
            self.error = Some(DecodeError::Invalid(
                "remote base64 stream had misplaced padding".to_owned(),
            ));
            return;
        }
        let mut decoded = [0u8; 3];
        decoded[0] = (a << 2) | (b >> 4);
        let mut length = 1;
        if c != 64 {
            decoded[1] = (b << 4) | (c >> 2);
            length = 2;
            if d != 64 {
                decoded[2] = (c << 6) | d;
                length = 3;
            }
        }
        if c == 64 && (b & 0x0f) != 0 || d == 64 && c != 64 && (c & 0x03) != 0 {
            self.error = Some(DecodeError::Invalid(
                "remote base64 stream had non-canonical padding".to_owned(),
            ));
            return;
        }
        if let Err(error) = self.file.write_all(&decoded[..length]) {
            self.error = Some(DecodeError::Local(error));
            return;
        }
        self.hasher.update(&decoded[..length]);
        match self.size.checked_add(length as u64) {
            Some(size) => self.size = size,
            None => {
                self.error = Some(DecodeError::Invalid(
                    "download exceeded the maximum supported size".to_owned(),
                ));
                return;
            }
        }
        self.ended = c == 64 || d == 64;
        self.quartet_len = 0;
    }
}

impl Write for Base64Decoder<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        for byte in input {
            self.accept(*byte);
        }
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
