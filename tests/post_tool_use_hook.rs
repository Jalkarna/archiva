//! PostToolUse hook integration gate (audit blocker B2).
//!
//! Under Claude Code the `post-tool-use` hook receives its payload as JSON on
//! stdin (the installed command takes no arguments). The auto-re-anchor never
//! ran before because the CLI ignored stdin. These tests drive the real binary
//! with the actual Claude Code payload shape and assert the re-anchor fires.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_archiva")
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!("{prefix}-{}-{nanos}", std::process::id()));
    path
}

struct Output {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run(root: &Path, args: &[&str], stdin: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(bin())
        .args(args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn archiva");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    Output {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn write_decision(root: &Path) {
    let json = r#"{"file":"src/hook.ts","anchor":"fn:hookTarget","lines":[1,3],"chose":"plain","because":"fixture","rejected":[]}"#;
    let out = run(root, &["write-decision", "--json", json], "");
    assert_eq!(out.status, 0, "write-decision failed: {}", out.stderr);
}

fn setup(prefix: &str) -> PathBuf {
    let root = unique_temp_dir(prefix);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src").join("hook.ts"),
        "function hookTarget() {\n  return 1;\n}\n",
    )
    .unwrap();
    let init = run(&root, &["init"], "");
    assert_eq!(init.status, 0, "init failed: {}", init.stderr);
    write_decision(&root);
    root
}

/// The headline case: a real Edit payload with an absolute `tool_input.file_path`
/// on stdin drives the re-anchor, and the decision is marked STALE after the
/// tracked function body changes.
#[test]
fn post_tool_use_reanchors_from_claude_code_stdin_payload() {
    let root = setup("archiva-hook-stdin");
    let abs = root.join("src").join("hook.ts");
    std::fs::write(&abs, "function hookTarget() {\n  return 2;\n}\n").unwrap();

    let payload = format!(
        r#"{{"session_id":"s1","tool_name":"Edit","tool_input":{{"file_path":{:?},"old_string":"1","new_string":"2"}},"cwd":{:?}}}"#,
        abs.to_string_lossy(),
        root.to_string_lossy()
    );
    let out = run(&root, &["hooks", "post-tool-use"], &payload);
    assert_eq!(out.status, 0, "hook failed: {}", out.stderr);
    assert!(
        out.stdout.contains("Re-anchored src/hook.ts"),
        "stdout: {}",
        out.stdout
    );
    assert!(out.stdout.contains("1 stale"), "stdout: {}", out.stdout);
}

/// A Write payload (also carries tool_input.file_path) is handled the same way.
#[test]
fn post_tool_use_handles_write_tool_payload() {
    let root = setup("archiva-hook-write");
    let abs = root.join("src").join("hook.ts");
    std::fs::write(&abs, "function hookTarget() {\n  return 3;\n}\n").unwrap();

    let payload = format!(
        r#"{{"tool_name":"Write","tool_input":{{"file_path":{:?},"content":"..."}},"cwd":{:?}}}"#,
        abs.to_string_lossy(),
        root.to_string_lossy()
    );
    let out = run(&root, &["hooks", "post-tool-use"], &payload);
    assert_eq!(out.status, 0, "hook failed: {}", out.stderr);
    assert!(
        out.stdout.contains("Re-anchored src/hook.ts"),
        "stdout: {}",
        out.stdout
    );
}

/// The hook fires after every tool. A payload for a non-file tool (Bash) must
/// be a clean no-op — no error, no re-anchor — so it never disrupts the agent.
#[test]
fn post_tool_use_is_noop_for_non_file_tool_payload() {
    let root = setup("archiva-hook-noop");
    let payload = r#"{"tool_name":"Bash","tool_input":{"command":"ls -la"}}"#;
    let out = run(&root, &["hooks", "post-tool-use"], payload);
    assert_eq!(out.status, 0, "hook errored: {}", out.stderr);
    assert!(
        !out.stdout.contains("Re-anchored"),
        "stdout: {}",
        out.stdout
    );
}

/// A malformed payload (not even JSON) must not error or abort — the hook has
/// to survive whatever it is handed.
#[test]
fn post_tool_use_is_noop_for_malformed_payload() {
    let root = setup("archiva-hook-malformed");
    let out = run(&root, &["hooks", "post-tool-use"], "not json at all {{{");
    assert_eq!(out.status, 0, "hook errored: {}", out.stderr);
    assert!(
        !out.stdout.contains("Re-anchored"),
        "stdout: {}",
        out.stdout
    );
}

/// A payload whose file lies outside the project is a no-op (nothing to
/// re-anchor here), not an error.
#[test]
fn post_tool_use_is_noop_for_file_outside_project() {
    let root = setup("archiva-hook-outside");
    let payload =
        r#"{"tool_name":"Edit","tool_input":{"file_path":"/etc/hosts"},"cwd":"/somewhere/else"}"#;
    let out = run(&root, &["hooks", "post-tool-use"], payload);
    assert_eq!(out.status, 0, "hook errored: {}", out.stderr);
    assert!(
        !out.stdout.contains("Re-anchored"),
        "stdout: {}",
        out.stdout
    );
}
