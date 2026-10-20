use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
use std::process::Command;
use uu_cli::job::decode_pwd_output;
#[cfg(target_os = "linux")]
use uu_cli::job::{wrap_pwd_probe, CompletionDecoder};

fn envelope(pwd_output: &[u8]) -> Vec<u8> {
    let mut output = b"PWD1\r\n".to_vec();
    for (index, chunk) in pwd_output.chunks(32).enumerate() {
        let mut hex = String::with_capacity(chunk.len() * 2);
        for byte in chunk {
            use std::fmt::Write as _;
            write!(&mut hex, "{byte:02x}").unwrap();
        }
        output.extend_from_slice(format!("D{index:04}:{hex}\r\n").as_bytes());
    }
    let hash = Sha256::digest(pwd_output);
    let mut hash_hex = String::with_capacity(hash.len() * 2);
    for byte in hash {
        use std::fmt::Write as _;
        write!(&mut hash_hex, "{byte:02x}").unwrap();
    }
    output.extend_from_slice(format!("Z:{}:{hash_hex}\r\n", pwd_output.len()).as_bytes());
    output
}

fn pwd_output(path_bytes: &[u8]) -> Vec<u8> {
    let mut output = path_bytes.to_vec();
    output.push(b'\n');
    output
}

fn assert_rejected(output: &[u8]) {
    assert!(decode_pwd_output(output).is_err(), "accepted malformed PWD1 envelope");
}

#[test]
fn decodes_root_and_unicode_paths_preserving_spaces_and_trailing_lf() {
    assert_eq!(decode_pwd_output(&envelope(b"/\n")).unwrap(), "/");

    let path = "/tmp/空间 with spaces \n";
    let decoded = decode_pwd_output(&envelope(&pwd_output(path.as_bytes()))).unwrap();
    assert_eq!(decoded, path);
}

#[test]
fn decodes_long_path_across_many_bounded_hex_records() {
    let path = format!("/{}", "segment-".repeat(500));
    let decoded = decode_pwd_output(&envelope(&pwd_output(path.as_bytes()))).unwrap();
    assert_eq!(decoded, path);
}

#[test]
fn accepts_observed_cursor_vt_only_between_records() {
    let path = format!("/{}", "x".repeat(96));
    let mut output = envelope(&pwd_output(path.as_bytes()));
    let second_record = output
        .windows(b"D0001:".len())
        .position(|window| window == b"D0001:")
        .expect("long path produces a second data record");
    output.splice(
        second_record..second_record,
        b"\x1b[19;1H\x1b[1D".iter().copied(),
    );
    assert_eq!(decode_pwd_output(&output).unwrap(), path);
}

#[test]
fn rejects_missing_repeated_corrupt_records_and_bad_hash_or_length() {
    let path = format!("/{}", "x".repeat(96));
    let valid = String::from_utf8(envelope(&pwd_output(path.as_bytes()))).unwrap();
    let header_end = valid.find("\r\n").unwrap() + 2;
    let first_end = valid[header_end..].find("\r\n").unwrap() + header_end + 2;
    let second_end = valid[first_end..].find("\r\n").unwrap() + first_end + 2;

    let missing_first = format!("{}{}", &valid[..header_end], &valid[first_end..]);
    assert_rejected(missing_first.as_bytes());

    let first_record = &valid[header_end..first_end];
    let repeated = format!("{}{}{}", &valid[..first_end], first_record, &valid[first_end..]);
    assert_rejected(repeated.as_bytes());

    let reordered = format!(
        "{}{}{}{}",
        &valid[..header_end],
        &valid[first_end..second_end],
        &valid[header_end..first_end],
        &valid[second_end..]
    );
    assert_rejected(reordered.as_bytes());

    let mut corrupt_hex = valid.clone().into_bytes();
    corrupt_hex[header_end + 6] = b'g';
    assert_rejected(&corrupt_hex);

    let mut bad_hash = valid.clone().into_bytes();
    let hash_start = bad_hash.iter().rposition(|byte| *byte == b':').unwrap() + 1;
    bad_hash[hash_start] = if bad_hash[hash_start] == b'0' { b'1' } else { b'0' };
    assert_rejected(&bad_hash);

    let mut bad_length = valid.clone();
    let footer = bad_length.rfind("Z:").unwrap();
    let length_digit = footer + 2;
    bad_length.replace_range(length_digit..length_digit + 1, "0");
    assert_rejected(bad_length.as_bytes());

    let footer = valid.rfind("Z:").unwrap();
    assert_rejected(valid[..footer].as_bytes());

    let mut no_final_lf = valid.into_bytes();
    no_final_lf.truncate(no_final_lf.len() - 2);
    assert_rejected(&no_final_lf);
}

#[test]
fn rejects_unsupported_vt_inside_or_between_records() {
    let path = format!("/{}", "x".repeat(96));
    let mut inside_record = envelope(&pwd_output(path.as_bytes()));
    let first_record = inside_record
        .windows(b"D0000:".len())
        .position(|window| window == b"D0000:")
        .unwrap();
    let insert_at = first_record + b"D0000:ab".len();
    inside_record.splice(insert_at..insert_at, b"\x1b[1D".iter().copied());
    assert_rejected(&inside_record);

    let mut unsupported_boundary = envelope(&pwd_output(path.as_bytes()));
    let second_record = unsupported_boundary
        .windows(b"D0001:".len())
        .position(|window| window == b"D0001:")
        .unwrap();
    unsupported_boundary.splice(
        second_record..second_record,
        b"\x1b[31m".iter().copied(),
    );
    assert_rejected(&unsupported_boundary);
}

#[test]
fn rejects_nonabsolute_non_utf8_or_unterminated_pwd_output() {
    assert_rejected(&envelope(b"relative\n"));
    assert_rejected(&envelope(b"/bad-\xff\n"));
    assert_rejected(&envelope(b"/missing-command-lf"));
    assert_rejected(&envelope(b"/path\0bad\n"));
}

#[cfg(target_os = "linux")]
#[test]
fn bash_wrapper_round_trips_long_unicode_path_ending_in_lf() {
    let temp = tempfile::tempdir().unwrap();
    let mut path = temp.path().to_path_buf();
    for segment in [
        "a".repeat(90),
        "b".repeat(90),
        "目录 with spaces ".to_owned(),
        format!("tail-{}\n", "c".repeat(40)),
    ] {
        path.push(segment);
        std::fs::create_dir(&path).unwrap();
    }

    let expected = std::fs::canonicalize(&path)
        .unwrap()
        .into_os_string()
        .into_string()
        .expect("test path is UTF-8");
    assert!(expected.len() > 120);
    assert!(expected.ends_with('\n'));

    let script = wrap_pwd_probe("pwdprobe1").unwrap();
    let result = Command::new("/bin/bash")
        .args(["--noprofile", "--norc", "-c", &script])
        .current_dir(&path)
        .output()
        .unwrap();
    assert!(result.status.success(), "bash wrapper failed: {:?}", result.stderr);

    let mut decoder = CompletionDecoder::new("pwdprobe1").unwrap();
    let payload = decoder.push(&result.stdout).unwrap();
    assert_eq!(decoder.completed(), Some(0));
    assert_eq!(decode_pwd_output(&payload).unwrap(), expected);

    for record in payload.split(|byte| *byte == b'\n') {
        let record = record.strip_suffix(b"\r").unwrap_or(record);
        if record.starts_with(b"D") {
            assert!(record.len() <= 70, "encoded line exceeds the console width budget");
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn missing_external_encoder_tool_fails_without_a_valid_envelope() {
    let temp = tempfile::tempdir().unwrap();
    let script = wrap_pwd_probe("pwdprobe2").unwrap();
    let result = Command::new("bash")
        .env("PATH", "")
        .args(["--noprofile", "--norc", "-c", &script])
        .current_dir(temp.path())
        .output()
        .unwrap();

    let mut decoder = CompletionDecoder::new("pwdprobe2").unwrap();
    let payload = decoder.push(&result.stdout).unwrap();
    assert_eq!(decoder.completed(), Some(127));
    assert_rejected(&payload);
}
