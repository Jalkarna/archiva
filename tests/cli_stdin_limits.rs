use std::fs;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use archiva::core::json::DEFAULT_MAX_BYTES;

#[test]
fn write_decision_stdin_rejects_input_over_json_byte_limit() {
    let root = unique_temp_dir("archiva-cli-stdin-limit");
    let mut child = Command::new(env!("CARGO_BIN_EXE_archiva"))
        .arg("write-decision")
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let input = vec![b'{'; DEFAULT_MAX_BYTES + 1];
    let write_result = child.stdin.as_mut().unwrap().write_all(&input);
    if let Err(error) = write_result {
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "JSON input exceeds configured byte limit\n"
    );

    let _ = fs::remove_dir_all(root);
}

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!("{prefix}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}
