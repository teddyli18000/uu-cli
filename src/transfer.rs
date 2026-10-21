//! Binary-safe file transfer over the command executor used by the worker.
//!
//! The remote endpoint needs a POSIX shell plus base64, dd, wc, cut, sha256sum, and mv.
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
const DOWNLOAD_CHUNK_OUTPUT_LIMIT: usize = 2048;
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
/// The remote file is first described by size and SHA-256. Its contents are then
/// downloaded in bounded raw-byte chunks, each with an explicit frame and size
/// acknowledgment, and decoded into a unique temporary file beside the destination.
/// The local destination is atomically replaced only after the total size and digest
/// verify.
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
    let mut hasher = Sha256::new();
    let mut actual_size = 0u64;
    let mut sequence = 0u64;
    while actual_size < expected_size {
        let expected_chunk_size = (expected_size - actual_size).min(UPLOAD_CHUNK_SIZE as u64);
        let command = download_chunk_command(remote_path, &nonce, sequence, expected_chunk_size);
        let mut output = BoundedWriter::new(DOWNLOAD_CHUNK_OUTPUT_LIMIT);
        run_remote(executor, &command, &mut output)?;
        if output.exceeded {
            return Err(completed("remote chunk output exceeded its limit"));
        }

        let encoded = parse_chunk_frame(&output.bytes, sequence, expected_chunk_size)?;
        let chunk_size = {
            let mut decoder =
                Base64Decoder::new(&mut temporary.file, &mut hasher, expected_chunk_size);
            decoder
                .write_all(&encoded)
                .map_err(|error| preparation(format!("cannot decode remote chunk: {error}")))?;
            decoder.finish().map_err(|error| match error {
                DecodeError::Invalid(message) => completed(message),
                DecodeError::Local(error) => {
                    preparation(format!("cannot write local temporary file: {error}"))
                }
            })?
        };
        if chunk_size != expected_chunk_size {
            return Err(completed(format!(
                "remote chunk {sequence} had {chunk_size} decoded bytes, expected {expected_chunk_size}"
            )));
        }
        actual_size = actual_size
            .checked_add(chunk_size)
            .ok_or_else(|| completed("download exceeded the maximum supported size"))?;
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| completed("download has too many chunks"))?;
    }
    let actual_hash = hex_digest(hasher.finalize().as_slice());
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

fn download_chunk_command(
    remote_path: &str,
    nonce: &str,
    sequence: u64,
    expected_size: u64,
) -> String {
    let body = format!(
        r#"printf 'UUCLI_CHUNK_BEGIN:{sequence}:{expected_size}\n'; dd if="$uu_cli_dest" bs={UPLOAD_CHUNK_SIZE} skip={sequence} count=1 2>/dev/null | base64 || exit $?; printf '\nUUCLI_CHUNK_END:{sequence}:{expected_size}\n'"#
    );
    remote_command(remote_path, nonce, &body)
}

fn remote_command(remote_path: &str, nonce: &str, body: &str) -> String {
    let quoted_path = shell_quote(remote_path);
    format!(
        r#"set -eu; uu_cli_remote_input={quoted_path}; case "$uu_cli_remote_input" in '~') [ -n "$HOME" ] || {{ echo "uu-cli: remote HOME is unset" >&2; exit 1; }}; uu_cli_dest="$HOME";; '~/'*) [ -n "$HOME" ] || {{ echo "uu-cli: remote HOME is unset" >&2; exit 1; }}; uu_cli_rest=$(printf '%s' "$uu_cli_remote_input" | cut -c 3-); uu_cli_dest="$HOME/$uu_cli_rest";; /*) uu_cli_dest="$uu_cli_remote_input";; *) uu_cli_dest="$PWD/$uu_cli_remote_input";; esac; uu_cli_tmp="$uu_cli_dest.uu-cli-{nonce}.part"; {body}"#
    )
}

fn parse_metadata(output: &[u8]) -> Result<(u64, String), JobRunError> {
    let clean = strip_transfer_controls(output)?;
    let text = std::str::from_utf8(&clean)
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

fn parse_chunk_frame(
    output: &[u8],
    sequence: u64,
    expected_size: u64,
) -> Result<Vec<u8>, JobRunError> {
    let clean = strip_transfer_controls(output)?;
    let (begin, remainder) = take_line(&clean)
        .ok_or_else(|| completed("remote chunk frame had no begin acknowledgment"))?;
    let expected_begin = format!("UUCLI_CHUNK_BEGIN:{sequence}:{expected_size}");
    if begin != expected_begin.as_bytes() {
        return Err(completed("remote chunk begin acknowledgment was invalid"));
    }

    let end_prefix = format!("UUCLI_CHUNK_END:{sequence}:");
    let end_start = find_subslice(remainder, end_prefix.as_bytes())
        .ok_or_else(|| completed("remote chunk frame had no end acknowledgment"))?;
    if end_start == 0 || !matches!(remainder[end_start - 1], b'\n' | b'\r') {
        return Err(completed(
            "remote chunk end acknowledgment was not line-framed",
        ));
    }
    let encoded = &remainder[..end_start];
    let after_prefix = &remainder[end_start + end_prefix.len()..];
    let (actual_line, trailing) = take_line(after_prefix).unwrap_or((after_prefix, &[]));
    let actual_size = std::str::from_utf8(actual_line)
        .ok()
        .and_then(|text| text.parse::<u64>().ok())
        .ok_or_else(|| completed("remote chunk size acknowledgment was invalid"))?;
    if actual_size != expected_size {
        return Err(completed(format!(
            "remote chunk {sequence} acknowledged {actual_size} bytes, expected {expected_size}"
        )));
    }
    if !trailing
        .iter()
        .all(|byte| matches!(byte, b'\r' | b'\n' | b'\t'))
    {
        return Err(completed("remote chunk frame had trailing data"));
    }
    Ok(encoded.to_vec())
}

fn take_line(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = bytes.iter().position(|byte| *byte == b'\n')?;
    let line = bytes[..end].strip_suffix(b"\r").unwrap_or(&bytes[..end]);
    Some((line, &bytes[end + 1..]))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Removes only ConPTY cursor CSI sequences from file-transfer protocol output.
/// The complete bounded job capture is passed here, so a sequence split across
/// executor writes is recognized without retaining unbounded stream state.
fn strip_transfer_controls(input: &[u8]) -> Result<Vec<u8>, JobRunError> {
    const MAX_CSI_BYTES: usize = 32;
    let mut clean = Vec::with_capacity(input.len());
    let mut offset = 0;
    while offset < input.len() {
        let byte = input[offset];
        if byte == 0x1b {
            if input.get(offset + 1) != Some(&b'[') {
                return Err(completed(
                    "file-transfer output contained an unsupported escape",
                ));
            }
            let sequence_start = offset;
            let mut end = offset + 2;
            while end < input.len() && !(0x40..=0x7e).contains(&input[end]) {
                end += 1;
                if end - sequence_start >= MAX_CSI_BYTES {
                    return Err(completed(
                        "file-transfer cursor sequence exceeded its limit",
                    ));
                }
            }
            let final_byte = *input
                .get(end)
                .ok_or_else(|| completed("file-transfer cursor sequence was incomplete"))?;
            let parameters = &input[offset + 2..end];
            let cursor_visibility = parameters == b"?25" && matches!(final_byte, b'h' | b'l');
            let cursor_position = parameters
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'9' | b';'))
                && matches!(final_byte, b'A'..=b'H' | b'd' | b'f');
            if !cursor_visibility && !cursor_position {
                return Err(completed(
                    "file-transfer output contained an unsupported CSI sequence",
                ));
            }
            if end - sequence_start + 1 > MAX_CSI_BYTES {
                return Err(completed(
                    "file-transfer cursor sequence exceeded its limit",
                ));
            }
            offset = end + 1;
            continue;
        }
        if byte == 0x7f || (0x80..=0x9f).contains(&byte) {
            return Err(completed(
                "file-transfer output contained an unsupported control byte",
            ));
        }
        if byte.is_ascii_control() && !matches!(byte, b'\r' | b'\n' | b'\t') {
            return Err(completed(
                "file-transfer output contained an unsupported control byte",
            ));
        }
        clean.push(byte);
        offset += 1;
    }
    Ok(clean)
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
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
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
    hasher: &'a mut Sha256,
    expected_size: u64,
    quartet: [u8; 4],
    quartet_len: usize,
    ended: bool,
    size: u64,
    error: Option<DecodeError>,
}

impl<'a> Base64Decoder<'a> {
    fn new(file: &'a mut File, hasher: &'a mut Sha256, expected_size: u64) -> Self {
        Self {
            file,
            hasher,
            expected_size,
            quartet: [0; 4],
            quartet_len: 0,
            ended: false,
            size: 0,
            error: None,
        }
    }

    fn finish(self) -> Result<u64, DecodeError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.quartet_len != 0 {
            return Err(DecodeError::Invalid(
                "remote base64 stream ended mid-quantum".to_owned(),
            ));
        }
        Ok(self.size)
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
        if self.size.saturating_add(length as u64) > self.expected_size {
            self.error = Some(DecodeError::Invalid(
                "remote chunk exceeded its declared byte count".to_owned(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "uu-cli-transfer-chunks-{}-{stamp:x}",
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

    #[test]
    fn download_uses_acknowledged_chunks_and_strips_split_cursor_csi() {
        let temp = TempDir::new();
        let destination = temp.0.join("download.bin");
        let bytes: Vec<u8> = (0usize..16_385)
            .map(|index| (index.wrapping_mul(37).wrapping_add(11) & 0xff) as u8)
            .collect();
        let expected_hash = hex_digest(Sha256::digest(&bytes).as_slice());
        let expected_chunks = bytes.len().div_ceil(UPLOAD_CHUNK_SIZE);
        let mut commands = Vec::new();
        let mut executor = |command: &str, output: &mut dyn Write| {
            commands.push(command.to_owned());
            if command.contains("remote_size=$(wc -c") {
                output
                    .write_all(b"\x1b[?2")
                    .and_then(|_| output.write_all(b"5l"))
                    .and_then(|_| write!(output, "{} {}\r\n", bytes.len(), expected_hash))
                    .map_err(|error| {
                        preparation(format!("fixture metadata output failed: {error}"))
                    })?;
            } else {
                assert_eq!(command.matches("bs=768 ").count(), 1);
                assert_eq!(command.matches("count=1").count(), 1);
                assert!(!command.contains("wc -c"));
                let expected_sequence = commands.len() - 2;
                let skip_values: Vec<usize> = command
                    .split(" skip=")
                    .skip(1)
                    .map(|part| {
                        part.split_whitespace()
                            .next()
                            .and_then(|text| text.parse::<usize>().ok())
                            .expect("download command should include its chunk sequence")
                    })
                    .collect();
                assert_eq!(skip_values, vec![expected_sequence]);
                let sequence = skip_values[0];
                let start = sequence * UPLOAD_CHUNK_SIZE;
                let end = (start + UPLOAD_CHUNK_SIZE).min(bytes.len());
                let chunk = &bytes[start..end];
                let encoded = base64_encode(chunk);
                let begin = format!("UUCLI_CHUNK_BEGIN:{sequence}:{}\r\n", chunk.len());
                let acknowledgment = format!("\r\nUUCLI_CHUNK_END:{sequence}:{}\r\n", chunk.len());
                let split = encoded.len() / 2;
                output
                    .write_all(begin.as_bytes())
                    .and_then(|_| output.write_all(&encoded.as_bytes()[..split]))
                    // Deliberately split this CSI between executor writes, as a
                    // ConPTY read can split presentation controls arbitrarily.
                    .and_then(|_| output.write_all(b"\x1b[17;"))
                    .and_then(|_| output.write_all(b"1H"))
                    .and_then(|_| output.write_all(&encoded.as_bytes()[split..]))
                    .and_then(|_| output.write_all(acknowledgment.as_bytes()))
                    .map_err(|error| {
                        preparation(format!("fixture chunk output failed: {error}"))
                    })?;
            }
            Ok(JobResult { exit_code: 0 })
        };

        let receipt = get("remote.bin", &destination, &mut executor).unwrap();
        drop(executor);

        assert_eq!(receipt.size, bytes.len() as u64);
        assert_eq!(receipt.sha256, expected_hash);
        assert_eq!(fs::read(destination).unwrap(), bytes);
        assert_eq!(commands.len(), 1 + expected_chunks);
        for (sequence, command) in commands.iter().skip(1).enumerate() {
            assert!(
                command.contains(&format!("bs=768 skip={sequence} count=1")),
                "chunk {sequence} should use a bounded dd read"
            );
            assert!(
                command.contains(&format!(
                    "UUCLI_CHUNK_BEGIN:{sequence}:{}",
                    (bytes.len() - sequence * UPLOAD_CHUNK_SIZE).min(UPLOAD_CHUNK_SIZE)
                )),
                "chunk {sequence} should frame its expected raw byte count"
            );
        }
    }

    #[test]
    fn transfer_output_rejects_unsupported_controls() {
        assert!(strip_transfer_controls(b"data\x1b[31m").is_err());
        assert!(strip_transfer_controls(b"data\x07").is_err());
    }
}
