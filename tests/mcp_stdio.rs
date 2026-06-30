use std::io::Write;
use std::process::{Command, Stdio};

use archiva::core::json::DEFAULT_MAX_BYTES;

#[test]
fn binary_mcp_stdio_handles_initialize_and_tools_list() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_archiva"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{}}}}"
        )
        .unwrap();
        writeln!(
            stdin,
            "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{{}}}}"
        )
        .unwrap();
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let responses = stdout.lines().collect::<Vec<_>>();
    assert_eq!(responses.len(), 2, "stdout={stdout}");
    assert!(responses[0].contains("\"id\":1"), "stdout={stdout}");
    assert!(
        responses[0].contains("\"protocolVersion\""),
        "stdout={stdout}"
    );
    assert!(responses[1].contains("\"id\":2"), "stdout={stdout}");
    assert!(responses[1].contains("\"tools\""), "stdout={stdout}");
    assert!(
        responses[1].contains("\"write_decision\""),
        "stdout={stdout}"
    );
    assert!(responses[1].contains("\"why\""), "stdout={stdout}");
    assert!(responses[1].contains("\"ghost_check\""), "stdout={stdout}");
}

#[test]
fn binary_mcp_stdio_reports_invalid_utf8_and_continues_session() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_archiva"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(b"\xff\n").unwrap();
        writeln!(
            stdin,
            "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{{}}}}"
        )
        .unwrap();
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let responses = stdout.lines().collect::<Vec<_>>();
    assert_eq!(responses.len(), 2, "stdout={stdout}");
    assert!(responses[0].contains("\"code\":-32700"), "stdout={stdout}");
    assert!(responses[1].contains("\"id\":2"), "stdout={stdout}");
    assert!(responses[1].contains("\"tools\""), "stdout={stdout}");
    assert!(
        responses[1].contains("\"write_decision\""),
        "stdout={stdout}"
    );
}

#[test]
fn binary_mcp_stdio_rejects_oversized_request_and_continues_session() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_archiva"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(&vec![b'{'; DEFAULT_MAX_BYTES + 1]).unwrap();
        stdin.write_all(b"\n").unwrap();
        writeln!(
            stdin,
            "{{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{{}}}}"
        )
        .unwrap();
    }
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let responses = stdout.lines().collect::<Vec<_>>();
    assert_eq!(responses.len(), 2, "stdout={stdout}");
    assert_eq!(
        responses[0],
        "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700,\"message\":\"JSON input exceeds configured byte limit\"}}"
    );
    assert!(responses[1].contains("\"id\":2"), "stdout={stdout}");
    assert!(responses[1].contains("\"tools\""), "stdout={stdout}");
}
