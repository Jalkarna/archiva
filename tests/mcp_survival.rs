//! MCP server survival CI gate (audit blocker B1b).
//!
//! The long-lived MCP server must survive any malformed request and any request
//! that would panic a tool: it returns a JSON-RPC/MCP error and keeps serving.
//! These tests drive the stdio protocol handler over adversarial input and
//! assert the session continues to a subsequent well-formed request.

use archiva::core::json::JsonValue;
use archiva::mcp::handle_protocol_input;

fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!("{prefix}-{}-{nanos}", std::process::id()));
    path
}

/// A batch of hostile request lines followed by a valid tools/list; the server
/// must answer the final request, proving it never aborted mid-session.
#[test]
fn mcp_survives_malformed_requests_and_continues_session() {
    let root = unique_temp_dir("archiva-mcp-survival");

    let hostile = [
        "not json at all",
        "{",
        "{\"jsonrpc\":\"2.0\"",
        "[]",
        "null",
        "123",
        "\"a string\"",
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":123}",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":\"not an object\"}",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"why\",\"arguments\":{\"file\":\"'\"}}}",
        "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"why\",\"arguments\":{\"file\":\"../escape.ts\"}}}",
        "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{}}",
    ];

    let mut input = String::new();
    for line in hostile {
        input.push_str(line);
        input.push('\n');
    }
    // Final well-formed request: if the server survived, this is answered.
    input.push_str("{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"tools/list\",\"params\":{}}\n");

    let output = handle_protocol_input(&root, &input).expect("server must not abort");
    let last = output
        .lines()
        .last()
        .expect("expected at least the final response");
    assert!(
        last.contains("\"id\":99") && last.contains("\"tools\""),
        "session did not survive to the final request; last line: {last}"
    );

    let _ = std::fs::remove_dir_all(root);
}

/// A ghost_check on a deeply nested source file previously overflowed the stack
/// and aborted the server. It must now return a result and the session must
/// continue.
#[test]
fn mcp_survives_deeply_nested_source_and_continues() {
    let root = unique_temp_dir("archiva-mcp-deep-source");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let n = 60_000;
    let mut deep = String::with_capacity(2 * n + 16);
    deep.push_str("fn f() {");
    for _ in 0..n {
        deep.push('{');
    }
    for _ in 0..n {
        deep.push('}');
    }
    deep.push('}');
    std::fs::write(root.join("src").join("deep.rs"), deep).unwrap();

    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"ghost_check\",\"arguments\":{\"file\":\"src/deep.rs\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\",\"params\":{}}\n",
    );

    let output = handle_protocol_input(&root, input).expect("server must not abort");
    let ids: Vec<Option<f64>> = output
        .lines()
        .filter_map(|line| match archiva::core::json::parse(line) {
            Ok(JsonValue::Object(object)) => Some(match object.get("id") {
                Some(JsonValue::Number(n)) => Some(*n),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    assert!(
        ids.contains(&Some(3.0)),
        "session did not reach the trailing tools/list; output: {output}"
    );

    let _ = std::fs::remove_dir_all(root);
}

/// A ghost_check on a `.ts` file with deeply nested template-literal
/// interpolation (`` `${`${…}`}` ``) previously overflowed the stack in the
/// JS/TS scanners and aborted the server. It must now return a result and the
/// session must continue.
#[test]
fn mcp_survives_deeply_nested_template_literal_and_continues() {
    let root = unique_temp_dir("archiva-mcp-deep-template");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let n = 60_000;
    let mut deep = String::with_capacity(6 * n + 16);
    deep.push_str("const x = ");
    for _ in 0..n {
        deep.push_str("`${");
    }
    deep.push('1');
    for _ in 0..n {
        deep.push_str("}`");
    }
    deep.push(';');
    std::fs::write(root.join("src").join("deep.ts"), deep).unwrap();

    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"ghost_check\",\"arguments\":{\"file\":\"src/deep.ts\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\",\"params\":{}}\n",
    );

    let output = handle_protocol_input(&root, input).expect("server must not abort");
    let ids: Vec<Option<f64>> = output
        .lines()
        .filter_map(|line| match archiva::core::json::parse(line) {
            Ok(JsonValue::Object(object)) => Some(match object.get("id") {
                Some(JsonValue::Number(n)) => Some(*n),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    assert!(
        ids.contains(&Some(3.0)),
        "session did not reach the trailing tools/list; output: {output}"
    );

    let _ = std::fs::remove_dir_all(root);
}

/// A ghost_check on a `.ts` file with a deeply nested destructuring binding
/// pattern (`const {a:{a:{…}}} = y;`) previously overflowed the stack in the
/// binding-pattern collectors and aborted the server. It must now return a
/// result and the session must continue.
#[test]
fn mcp_survives_deeply_nested_destructuring_and_continues() {
    let root = unique_temp_dir("archiva-mcp-deep-destructure");
    std::fs::create_dir_all(root.join("src")).unwrap();
    let n = 60_000;
    let mut deep = String::with_capacity(4 * n + 16);
    deep.push_str("const ");
    for _ in 0..n {
        deep.push_str("{a:");
    }
    deep.push('x');
    for _ in 0..n {
        deep.push('}');
    }
    deep.push_str(" = y;\n");
    std::fs::write(root.join("src").join("deep.ts"), deep).unwrap();

    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"ghost_check\",\"arguments\":{\"file\":\"src/deep.ts\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/list\",\"params\":{}}\n",
    );

    let output = handle_protocol_input(&root, input).expect("server must not abort");
    let ids: Vec<Option<f64>> = output
        .lines()
        .filter_map(|line| match archiva::core::json::parse(line) {
            Ok(JsonValue::Object(object)) => Some(match object.get("id") {
                Some(JsonValue::Number(n)) => Some(*n),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    assert!(
        ids.contains(&Some(3.0)),
        "session did not reach the trailing tools/list; output: {output}"
    );

    let _ = std::fs::remove_dir_all(root);
}
