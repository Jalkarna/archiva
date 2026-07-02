use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::core::decision::write_decision_input_from_json;
use crate::core::json::{self, JsonObject, JsonValue};
use crate::core::lint::format_ghost_check_result;
use crate::core::project;
use crate::core::version::{APPLICATION_NAME, APPLICATION_VERSION, MCP_PROTOCOL_VERSION};
use crate::core::{
    error::{ArchivaError, Result},
    paths::RelativePath,
};

pub fn handle_protocol_line(line: &str) -> Option<JsonValue> {
    let request = match json::parse(line) {
        Ok(value) => value,
        Err(error) => {
            return Some(error_response(
                Some(JsonValue::Null),
                -32700,
                error.message(),
            ));
        }
    };
    handle_protocol_request(&request)
}

pub fn handle_protocol_line_with_tool_handler(
    line: &str,
    handler: &mut dyn ToolCallHandler,
) -> Option<JsonValue> {
    let request = match json::parse(line) {
        Ok(value) => value,
        Err(error) => {
            return Some(error_response(
                Some(JsonValue::Null),
                -32700,
                error.message(),
            ));
        }
    };
    handle_protocol_request_with_tool_handler(&request, handler)
}

pub fn handle_protocol_request(request: &JsonValue) -> Option<JsonValue> {
    handle_protocol_request_internal(request, None)
}

pub fn handle_protocol_request_with_tool_handler(
    request: &JsonValue,
    handler: &mut dyn ToolCallHandler,
) -> Option<JsonValue> {
    handle_protocol_request_internal(request, Some(handler))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    name: ToolName,
    arguments: JsonValue,
}

impl ToolCall {
    pub fn name(&self) -> &ToolName {
        &self.name
    }

    pub fn arguments(&self) -> &JsonValue {
        &self.arguments
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolName {
    string: Option<String>,
    display: String,
}

impl ToolName {
    pub fn as_str(&self) -> Option<&str> {
        self.string.as_deref()
    }

    pub fn display(&self) -> &str {
        &self.display
    }

    pub fn matches(&self, value: &str) -> bool {
        self.as_str() == Some(value)
    }
}

pub trait ToolCallHandler {
    fn call_tool(&mut self, call: ToolCall) -> std::result::Result<JsonValue, String>;
}

pub struct ProjectToolHandler {
    project_root: PathBuf,
}

impl ProjectToolHandler {
    pub fn new(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
        }
    }
}

impl ToolCallHandler for ProjectToolHandler {
    fn call_tool(&mut self, call: ToolCall) -> std::result::Result<JsonValue, String> {
        match call.name().as_str() {
            Some("write_decision") => {
                let input =
                    write_decision_input_from_json(call.arguments()).map_err(user_message)?;
                let decision =
                    project::write_decision(&self.project_root, &input).map_err(user_message)?;
                Ok(text_result(&format!("Recorded {}.", decision.id)))
            }
            Some("why") => {
                let input = why_tool_arguments_from_json(call.arguments()).map_err(user_message)?;
                // A `line` query resolves to the decision covering that line,
                // matching the CLI's `why <file> <line>` semantics. Previously
                // `line` was silently dropped and a whole-file (or first-anchor)
                // result was returned — confidently wrong (audit blocker B12).
                let output = match input.line {
                    Some(line) => project::why_for_line(&self.project_root, &input.file, line)
                        .map_err(user_message)?,
                    None => project::why(&self.project_root, &input.file, input.anchor.as_deref())
                        .map_err(user_message)?,
                };
                Ok(text_result(&output))
            }
            Some("ghost_check") => {
                let input =
                    ghost_check_tool_arguments_from_json(call.arguments()).map_err(user_message)?;
                let issues = project::lint_file(&self.project_root, &input.file, false)
                    .map_err(user_message)?;
                Ok(text_result(&format_ghost_check_result(
                    &input.file,
                    &issues,
                )))
            }
            _ => Err(format!("Unknown tool: {}", call.name().display())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhyToolArguments {
    pub file: RelativePath,
    pub anchor: Option<String>,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GhostCheckToolArguments {
    pub file: RelativePath,
}

pub fn parse_tool_call_params(params: Option<&JsonValue>) -> std::result::Result<ToolCall, String> {
    let Some(params) = params else {
        return Err("Cannot read properties of undefined (reading 'name')".to_string());
    };
    if matches!(params, JsonValue::Null) {
        return Err("Cannot read properties of null (reading 'name')".to_string());
    }

    let (raw_name, raw_arguments) = match params {
        JsonValue::Object(object) => (object.get("name"), object.get("arguments")),
        _ => (None, None),
    };

    Ok(ToolCall {
        name: parse_tool_name(raw_name),
        arguments: match raw_arguments {
            Some(JsonValue::Null) | None => object(Vec::new()),
            Some(value) => value.clone(),
        },
    })
}

pub fn why_tool_arguments_from_json(value: &JsonValue) -> Result<WhyToolArguments> {
    let object = expect_tool_object(value)?;
    Ok(WhyToolArguments {
        file: RelativePath::new(&expect_tool_non_empty_string(
            required_tool_field(object, "file")?,
            "file",
        )?)?,
        anchor: optional_tool_non_empty_string(object, "anchor")?,
        line: optional_tool_positive_line(object, "line")?,
    })
}

/// Parse an optional positive line number from a tool argument. A JSON number
/// is accepted only when it is a positive integer (matching the CLI's
/// `why <file> <line>` contract); anything else is a schema error rather than a
/// silently-dropped field (audit blocker B12).
fn optional_tool_positive_line(object: &JsonObject, key: &str) -> Result<Option<u32>> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    match value {
        JsonValue::Null => Ok(None),
        JsonValue::Number(number) => {
            let rounded = *number as u64;
            if *number >= 1.0 && (rounded as f64 - number).abs() < f64::EPSILON {
                u32::try_from(rounded)
                    .map(Some)
                    .map_err(|_| ArchivaError::schema(key, "line is out of range"))
            } else {
                Err(ArchivaError::schema(key, "expected a positive integer"))
            }
        }
        _ => Err(ArchivaError::schema(key, "expected a positive integer")),
    }
}

pub fn ghost_check_tool_arguments_from_json(value: &JsonValue) -> Result<GhostCheckToolArguments> {
    let object = expect_tool_object(value)?;
    Ok(GhostCheckToolArguments {
        file: RelativePath::new(&expect_tool_non_empty_string(
            required_tool_field(object, "file")?,
            "file",
        )?)?,
    })
}

pub fn serve_stdio(project_root: &Path) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve_reader_writer(project_root, stdin.lock(), stdout.lock())
}

pub fn handle_protocol_input(project_root: &Path, input: &str) -> Result<String> {
    let mut output = Vec::new();
    serve_reader_writer(project_root, io::Cursor::new(input), &mut output)?;
    String::from_utf8(output)
        .map_err(|source| ArchivaError::cli(format!("MCP output was not UTF-8: {source}")))
}

pub fn serve_reader_writer<R, W>(project_root: &Path, reader: R, mut writer: W) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let mut reader = reader;
    let mut handler = ProjectToolHandler::new(project_root);
    loop {
        let raw_line = match read_protocol_line_with_limit(&mut reader, json::DEFAULT_MAX_BYTES)
            .map_err(|source| ArchivaError::io(None, "read MCP request", source))?
        {
            ProtocolLineRead::Eof => break,
            ProtocolLineRead::Line(line) => line,
            ProtocolLineRead::Oversize => {
                writeln!(
                    writer,
                    "{}",
                    json::stringify_compact(&error_response(
                        Some(JsonValue::Null),
                        -32700,
                        "JSON input exceeds configured byte limit"
                    ))
                )
                .map_err(|source| ArchivaError::io(None, "write MCP response", source))?;
                writer
                    .flush()
                    .map_err(|source| ArchivaError::io(None, "flush MCP response", source))?;
                continue;
            }
        };
        let line = match std::str::from_utf8(&raw_line) {
            Ok(value) => value.trim_end_matches(['\r', '\n']),
            Err(error) => {
                writeln!(
                    writer,
                    "{}",
                    json::stringify_compact(&error_response(
                        Some(JsonValue::Null),
                        -32700,
                        &format!("MCP request was not UTF-8: {error}")
                    ))
                )
                .map_err(|source| ArchivaError::io(None, "write MCP response", source))?;
                writer
                    .flush()
                    .map_err(|source| ArchivaError::io(None, "flush MCP response", source))?;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // Outer panic backstop: even if request dispatch panics outside a tool
        // call (e.g. in parsing or method routing), the server must survive and
        // keep serving. Catch any unwind, log nothing to stdout except a
        // well-formed JSON-RPC error echoing the request id when recoverable.
        let dispatched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_protocol_line_with_tool_handler(line, &mut handler)
        }));
        let response = match dispatched {
            Ok(response) => response,
            Err(panic) => Some(error_response(
                recover_request_id(line),
                -32603,
                &panic_message(panic),
            )),
        };
        if let Some(response) = response {
            writeln!(writer, "{}", json::stringify_compact(&response))
                .map_err(|source| ArchivaError::io(None, "write MCP response", source))?;
            writer
                .flush()
                .map_err(|source| ArchivaError::io(None, "flush MCP response", source))?;
        }
    }
    Ok(())
}

enum ProtocolLineRead {
    Eof,
    Line(Vec<u8>),
    Oversize,
}

fn read_protocol_line_with_limit<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> io::Result<ProtocolLineRead> {
    let mut line = Vec::new();
    let mut oversize = false;
    let sentinel_limit = max_bytes.saturating_add(1);

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if oversize {
                return Ok(ProtocolLineRead::Oversize);
            }
            return if line.is_empty() {
                Ok(ProtocolLineRead::Eof)
            } else {
                Ok(ProtocolLineRead::Line(line))
            };
        }

        let chunk_len = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        let line_ended = chunk_len < available.len() || available[chunk_len - 1] == b'\n';

        if !oversize {
            let remaining = sentinel_limit.saturating_sub(line.len());
            let copy_len = remaining.min(chunk_len);
            line.extend_from_slice(&available[..copy_len]);
            if line.len() > max_bytes || copy_len < chunk_len {
                oversize = true;
            }
        }

        reader.consume(chunk_len);
        if line_ended {
            return if oversize {
                Ok(ProtocolLineRead::Oversize)
            } else {
                Ok(ProtocolLineRead::Line(line))
            };
        }
    }
}

fn handle_protocol_request_internal(
    request: &JsonValue,
    handler: Option<&mut dyn ToolCallHandler>,
) -> Option<JsonValue> {
    let JsonValue::Object(object) = request else {
        return Some(error_response(
            Some(JsonValue::Null),
            -32600,
            "Invalid request",
        ));
    };
    let id = object.get("id");
    let method = match object.get("method") {
        Some(JsonValue::String(method)) if !method.is_empty() => method.as_str(),
        _ => {
            return match id {
                Some(JsonValue::Null) | None => None,
                Some(id) => Some(error_response(Some(id.clone()), -32600, "Missing method")),
            };
        }
    };

    if method.starts_with("notifications/") {
        return None;
    }

    match method {
        "initialize" => Some(success_response(id.cloned(), initialize_result())),
        "tools/list" => Some(success_response(id.cloned(), tools_list_result())),
        "tools/call" => match handler {
            Some(handler) => Some(handle_tool_call(id.cloned(), object.get("params"), handler)),
            None => Some(error_response(
                id.cloned(),
                -32000,
                "Unsupported MCP method: tools/call",
            )),
        },
        _ => Some(error_response(
            id.cloned(),
            -32000,
            &format!("Unsupported MCP method: {method}"),
        )),
    }
}

pub fn initialize_result() -> JsonValue {
    object(vec![
        (
            "protocolVersion",
            JsonValue::String(MCP_PROTOCOL_VERSION.to_string()),
        ),
        ("capabilities", object(vec![("tools", object(Vec::new()))])),
        (
            "serverInfo",
            object(vec![
                ("name", JsonValue::String(APPLICATION_NAME.to_string())),
                (
                    "version",
                    JsonValue::String(APPLICATION_VERSION.to_string()),
                ),
            ]),
        ),
    ])
}

pub fn tools_list_result() -> JsonValue {
    object(vec![("tools", JsonValue::Array(tool_definitions()))])
}

pub fn text_result(text: &str) -> JsonValue {
    object(vec![(
        "content",
        JsonValue::Array(vec![object(vec![
            ("type", JsonValue::String("text".to_string())),
            ("text", JsonValue::String(text.to_string())),
        ])]),
    )])
}

/// An MCP tool result flagged as an error (`isError: true`). Per the MCP spec,
/// a tool that fails should return its error as a result with this flag rather
/// than a JSON-RPC protocol error, so the client attributes it to the tool call
/// and the session continues. Used when a tool invocation panics (B1b): the
/// long-lived server must never abort on one bad request.
pub fn error_text_result(text: &str) -> JsonValue {
    object(vec![
        (
            "content",
            JsonValue::Array(vec![object(vec![
                ("type", JsonValue::String("text".to_string())),
                ("text", JsonValue::String(text.to_string())),
            ])]),
        ),
        ("isError", JsonValue::Bool(true)),
    ])
}

/// Best-effort human-readable message from a caught panic payload.
fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        format!("internal error: {text}")
    } else if let Some(text) = panic.downcast_ref::<String>() {
        format!("internal error: {text}")
    } else {
        "internal error: request handling panicked".to_string()
    }
}

/// Recover the JSON-RPC `id` from a raw request line on a best-effort basis, so
/// the panic-backstop error response can echo the correct id. Returns `None`
/// (rendered as `id: null`) when the line cannot be parsed or carries no id.
fn recover_request_id(line: &str) -> Option<JsonValue> {
    match json::parse(line) {
        Ok(JsonValue::Object(object)) => object.get("id").cloned(),
        _ => None,
    }
}

pub fn tool_definitions() -> Vec<JsonValue> {
    vec![write_decision_tool(), why_tool(), ghost_check_tool()]
}

fn handle_tool_call(
    id: Option<JsonValue>,
    params: Option<&JsonValue>,
    handler: &mut dyn ToolCallHandler,
) -> JsonValue {
    let call = match parse_tool_call_params(params) {
        Ok(call) => call,
        Err(message) => return error_response(id, -32000, &message),
    };
    // Per-request panic boundary: a tool implementation that panics (e.g. an
    // unforeseen parser edge case on committed input) must not abort the
    // long-lived server. Catch it and report it as an MCP tool error
    // (isError: true) so the client attributes it to this call and the session
    // continues serving subsequent requests.
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler.call_tool(call)));
    match outcome {
        Ok(Ok(result)) => success_response(id, result),
        Ok(Err(message)) => error_response(id, -32000, &message),
        Err(panic) => success_response(id, error_text_result(&panic_message(panic))),
    }
}

fn parse_tool_name(value: Option<&JsonValue>) -> ToolName {
    match value {
        Some(JsonValue::String(value)) => ToolName {
            string: Some(value.clone()),
            display: value.clone(),
        },
        Some(JsonValue::Null) | None => ToolName {
            string: Some(String::new()),
            display: String::new(),
        },
        Some(value) => ToolName {
            string: None,
            display: js_template_string(value),
        },
    }
}

fn js_template_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(_) => json::stringify_compact(value),
        JsonValue::String(value) => value.clone(),
        JsonValue::Array(values) => values
            .iter()
            .map(|value| match value {
                JsonValue::Null => String::new(),
                _ => js_template_string(value),
            })
            .collect::<Vec<_>>()
            .join(","),
        JsonValue::Object(_) => "[object Object]".to_string(),
    }
}

fn expect_tool_object(value: &JsonValue) -> Result<&JsonObject> {
    match value {
        JsonValue::Object(object) => Ok(object),
        _ => Err(ArchivaError::schema("", "expected object")),
    }
}

fn required_tool_field<'a>(object: &'a JsonObject, key: &str) -> Result<&'a JsonValue> {
    object
        .get(key)
        .ok_or_else(|| ArchivaError::schema(key, "missing required field"))
}

fn optional_tool_non_empty_string(object: &JsonObject, key: &str) -> Result<Option<String>> {
    object
        .get(key)
        .map(|value| expect_tool_non_empty_string(value, key))
        .transpose()
}

fn expect_tool_non_empty_string(value: &JsonValue, field: &str) -> Result<String> {
    match value {
        JsonValue::String(value) if !value.is_empty() => Ok(value.clone()),
        JsonValue::String(_) => Err(ArchivaError::schema(field, "expected non-empty string")),
        _ => Err(ArchivaError::schema(field, "expected string")),
    }
}

fn write_decision_tool() -> JsonValue {
    object(vec![
        ("name", JsonValue::String("write_decision".to_string())),
        (
            "description",
            JsonValue::String(
                "Log a decision you just made: what you chose, why, and what you rejected."
                    .to_string(),
            ),
        ),
        (
            "inputSchema",
            object(vec![
                ("type", JsonValue::String("object".to_string())),
                (
                    "required",
                    string_array(&["file", "anchor", "lines", "chose", "because", "rejected"]),
                ),
                (
                    "properties",
                    object(vec![
                        ("file", string_type()),
                        ("anchor", string_type()),
                        (
                            "lines",
                            object(vec![
                                ("type", JsonValue::String("array".to_string())),
                                ("items", number_type()),
                                ("minItems", JsonValue::Number(2.0)),
                                ("maxItems", JsonValue::Number(2.0)),
                            ]),
                        ),
                        ("chose", string_type()),
                        ("because", string_type()),
                        (
                            "rejected",
                            object(vec![
                                ("type", JsonValue::String("array".to_string())),
                                (
                                    "items",
                                    object(vec![
                                        ("type", JsonValue::String("object".to_string())),
                                        ("required", string_array(&["approach", "reason"])),
                                        (
                                            "properties",
                                            object(vec![
                                                ("approach", string_type()),
                                                ("reason", string_type()),
                                            ]),
                                        ),
                                    ]),
                                ),
                            ]),
                        ),
                        ("expires_if", string_type()),
                        ("supersedes", string_type()),
                    ]),
                ),
            ]),
        ),
    ])
}

fn why_tool() -> JsonValue {
    object(vec![
        ("name", JsonValue::String("why".to_string())),
        (
            "description",
            JsonValue::String(
                "Look up the decision log for a file, by anchor or line, before modifying it."
                    .to_string(),
            ),
        ),
        (
            "inputSchema",
            object(vec![
                ("type", JsonValue::String("object".to_string())),
                ("required", string_array(&["file"])),
                (
                    "properties",
                    object(vec![
                        ("file", string_type()),
                        ("anchor", string_type()),
                        ("line", number_type()),
                    ]),
                ),
            ]),
        ),
    ])
}

fn ghost_check_tool() -> JsonValue {
    object(vec![
        ("name", JsonValue::String("ghost_check".to_string())),
        (
            "description",
            JsonValue::String("Check for stale or orphaned decisions in a file.".to_string()),
        ),
        (
            "inputSchema",
            object(vec![
                ("type", JsonValue::String("object".to_string())),
                ("required", string_array(&["file"])),
                ("properties", object(vec![("file", string_type())])),
            ]),
        ),
    ])
}

fn string_type() -> JsonValue {
    object(vec![("type", JsonValue::String("string".to_string()))])
}

fn number_type() -> JsonValue {
    object(vec![("type", JsonValue::String("number".to_string()))])
}

fn string_array(values: &[&str]) -> JsonValue {
    JsonValue::Array(
        values
            .iter()
            .map(|value| JsonValue::String((*value).to_string()))
            .collect(),
    )
}

fn object(entries: Vec<(&str, JsonValue)>) -> JsonValue {
    JsonValue::Object(JsonObject::from_entries(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    ))
}

fn success_response(id: Option<JsonValue>, result: JsonValue) -> JsonValue {
    let mut entries = vec![("jsonrpc", JsonValue::String("2.0".to_string()))];
    if let Some(id) = id {
        entries.push(("id", id));
    }
    entries.push(("result", result));
    object(entries)
}

fn error_response(id: Option<JsonValue>, code: i32, message: &str) -> JsonValue {
    let mut entries = vec![("jsonrpc", JsonValue::String("2.0".to_string()))];
    if let Some(id) = id {
        entries.push(("id", id));
    }
    entries.push((
        "error",
        object(vec![
            ("code", JsonValue::Number(code as f64)),
            ("message", JsonValue::String(message.to_string())),
        ]),
    ));
    object(entries)
}

fn user_message(error: ArchivaError) -> String {
    error.user_message()
}

#[cfg(test)]
mod tests {
    use super::{
        ghost_check_tool_arguments_from_json, handle_protocol_input, handle_protocol_line,
        handle_protocol_line_with_tool_handler, initialize_result, parse_tool_call_params,
        text_result, tools_list_result, why_tool_arguments_from_json, ToolCall, ToolCallHandler,
    };
    use crate::core::json::{stringify_compact, JsonValue};
    use crate::core::paths::{decision_lock_path, dlog_path, dmap_path, RelativePath};
    use crate::core::storage::load_dlog;
    use crate::core::version::APPLICATION_VERSION;
    use std::fs;
    use std::path::PathBuf;

    struct FakeToolHandler {
        calls: Vec<ToolCall>,
    }

    impl FakeToolHandler {
        fn new() -> Self {
            Self { calls: Vec::new() }
        }
    }

    impl ToolCallHandler for FakeToolHandler {
        fn call_tool(&mut self, call: ToolCall) -> std::result::Result<JsonValue, String> {
            let result = if call.name().matches("why") {
                Ok(text_result("why result"))
            } else {
                Err(format!("Unknown tool: {}", call.name().display()))
            };
            self.calls.push(call);
            result
        }
    }

    struct PanicOnceToolHandler {
        panicked: bool,
    }

    impl ToolCallHandler for PanicOnceToolHandler {
        fn call_tool(&mut self, call: ToolCall) -> std::result::Result<JsonValue, String> {
            if call.name().matches("boom") {
                self.panicked = true;
                panic!("simulated tool panic on committed input");
            }
            Ok(text_result("survived"))
        }
    }

    #[test]
    fn tool_panic_is_contained_as_iserror_and_session_continues() {
        // A panicking tool call must not abort the server. It should come back
        // as a tool result with isError: true (echoing the request id), and a
        // subsequent request on the same handler must still be served.
        let mut handler = PanicOnceToolHandler { panicked: false };

        let prior_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let boom = handle_protocol_line_with_tool_handler(
            "{\"jsonrpc\":\"2.0\",\"id\":42,\"method\":\"tools/call\",\"params\":{\"name\":\"boom\"}}",
            &mut handler,
        )
        .unwrap();
        std::panic::set_hook(prior_hook);

        assert!(handler.panicked);
        let rendered = stringify_compact(&boom);
        assert!(rendered.contains("\"id\":42"), "{rendered}");
        assert!(rendered.contains("\"isError\":true"), "{rendered}");
        assert!(rendered.contains("internal error"), "{rendered}");
        // It is a result envelope, not a protocol error.
        assert!(rendered.contains("\"result\""), "{rendered}");
        assert!(!rendered.contains("\"error\":{"), "{rendered}");

        let after = handle_protocol_line_with_tool_handler(
            "{\"jsonrpc\":\"2.0\",\"id\":43,\"method\":\"tools/call\",\"params\":{\"name\":\"ok\"}}",
            &mut handler,
        )
        .unwrap();
        let after_rendered = stringify_compact(&after);
        assert!(after_rendered.contains("survived"), "{after_rendered}");
        assert!(after_rendered.contains("\"id\":43"), "{after_rendered}");
    }

    #[test]
    fn builds_initialize_result_matching_typescript_contract() {
        assert_eq!(
            stringify_compact(&initialize_result()),
            format!(
                "{{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{{\"tools\":{{}}}},\"serverInfo\":{{\"name\":\"archiva\",\"version\":\"{}\"}}}}",
                APPLICATION_VERSION
            )
        );
    }

    #[test]
    fn builds_tools_list_result_with_current_schema_and_session_omission() {
        let output = stringify_compact(&tools_list_result());
        assert_eq!(
            output,
            "{\"tools\":[{\"name\":\"write_decision\",\"description\":\"Log a decision you just made: what you chose, why, and what you rejected.\",\"inputSchema\":{\"type\":\"object\",\"required\":[\"file\",\"anchor\",\"lines\",\"chose\",\"because\",\"rejected\"],\"properties\":{\"file\":{\"type\":\"string\"},\"anchor\":{\"type\":\"string\"},\"lines\":{\"type\":\"array\",\"items\":{\"type\":\"number\"},\"minItems\":2,\"maxItems\":2},\"chose\":{\"type\":\"string\"},\"because\":{\"type\":\"string\"},\"rejected\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"required\":[\"approach\",\"reason\"],\"properties\":{\"approach\":{\"type\":\"string\"},\"reason\":{\"type\":\"string\"}}}},\"expires_if\":{\"type\":\"string\"},\"supersedes\":{\"type\":\"string\"}}}},{\"name\":\"why\",\"description\":\"Look up the decision log for a file, by anchor or line, before modifying it.\",\"inputSchema\":{\"type\":\"object\",\"required\":[\"file\"],\"properties\":{\"file\":{\"type\":\"string\"},\"anchor\":{\"type\":\"string\"},\"line\":{\"type\":\"number\"}}}},{\"name\":\"ghost_check\",\"description\":\"Check for stale or orphaned decisions in a file.\",\"inputSchema\":{\"type\":\"object\",\"required\":[\"file\"],\"properties\":{\"file\":{\"type\":\"string\"}}}}]}"
        );
        assert!(!output.contains("\"session\""));
    }

    #[test]
    fn builds_text_result_payload_for_tool_calls() {
        assert_eq!(
            stringify_compact(&text_result("Recorded dec_001.")),
            "{\"content\":[{\"type\":\"text\",\"text\":\"Recorded dec_001.\"}]}"
        );
    }

    #[test]
    fn handles_stdio_edge_case_protocol_lines_like_typescript_contract() {
        let missing = handle_protocol_line("{\"jsonrpc\":\"2.0\",\"id\":1}").unwrap();
        assert_eq!(
            stringify_compact(&missing),
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32600,\"message\":\"Missing method\"}}"
        );

        assert!(handle_protocol_line("{\"jsonrpc\":\"2.0\",\"id\":null}").is_none());
        assert!(handle_protocol_line("{\"jsonrpc\":\"2.0\"}").is_none());
        assert!(handle_protocol_line("{\"jsonrpc\":\"2.0\",\"method\":\"\"}").is_none());

        let empty_method =
            handle_protocol_line("{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"\"}").unwrap();
        assert_eq!(
            stringify_compact(&empty_method),
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"error\":{\"code\":-32600,\"message\":\"Missing method\"}}"
        );

        let initialize =
            handle_protocol_line("{\"jsonrpc\":\"2.0\",\"method\":\"initialize\"}").unwrap();
        let initialize_output = stringify_compact(&initialize);
        assert!(initialize_output.starts_with("{\"jsonrpc\":\"2.0\",\"result\":"));
        assert!(!initialize_output.contains("\"id\""));
        assert!(initialize_output.contains("\"protocolVersion\":\"2024-11-05\""));

        assert!(handle_protocol_line(
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"id\":99}"
        )
        .is_none());

        let unsupported =
            handle_protocol_line("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"does/not/exist\"}")
                .unwrap();
        assert_eq!(
            stringify_compact(&unsupported),
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32000,\"message\":\"Unsupported MCP method: does/not/exist\"}}"
        );

        let invalid = handle_protocol_line("{bad").unwrap();
        let invalid_output = stringify_compact(&invalid);
        assert!(invalid_output
            .starts_with("{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700"));
    }

    #[test]
    fn dispatches_tools_call_through_injected_handler() {
        let mut handler = FakeToolHandler::new();
        let response = handle_protocol_line_with_tool_handler(
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"why\",\"arguments\":{\"file\":\"src/main.ts\"}}}",
            &mut handler,
        )
        .unwrap();

        assert_eq!(
            stringify_compact(&response),
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"why result\"}]}}"
        );
        assert_eq!(handler.calls.len(), 1);
        assert_eq!(handler.calls[0].name().as_str(), Some("why"));
        assert_eq!(
            stringify_compact(handler.calls[0].arguments()),
            "{\"file\":\"src/main.ts\"}"
        );
    }

    #[test]
    fn tools_call_defaults_arguments_and_envelopes_handler_errors_like_typescript() {
        let mut handler = FakeToolHandler::new();
        let response = handle_protocol_line_with_tool_handler(
            "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"tools/call\",\"params\":{\"name\":\"nope\"}}",
            &mut handler,
        )
        .unwrap();

        assert_eq!(
            stringify_compact(&response),
            "{\"jsonrpc\":\"2.0\",\"id\":8,\"error\":{\"code\":-32000,\"message\":\"Unknown tool: nope\"}}"
        );
        assert_eq!(stringify_compact(handler.calls[0].arguments()), "{}");
    }

    #[test]
    fn tools_call_preserves_current_params_edge_cases() {
        let mut handler = FakeToolHandler::new();
        let missing = handle_protocol_line_with_tool_handler(
            "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\"}",
            &mut handler,
        )
        .unwrap();
        assert_eq!(
            stringify_compact(&missing),
            "{\"jsonrpc\":\"2.0\",\"id\":9,\"error\":{\"code\":-32000,\"message\":\"Cannot read properties of undefined (reading 'name')\"}}"
        );

        let null = handle_protocol_line_with_tool_handler(
            "{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":\"tools/call\",\"params\":null}",
            &mut handler,
        )
        .unwrap();
        assert_eq!(
            stringify_compact(&null),
            "{\"jsonrpc\":\"2.0\",\"id\":10,\"error\":{\"code\":-32000,\"message\":\"Cannot read properties of null (reading 'name')\"}}"
        );

        let array_name = parse_tool_call_params(Some(
            &crate::core::json::parse(
                "{\"name\":[\"why\"],\"arguments\":{\"file\":\"src/main.ts\"}}",
            )
            .unwrap(),
        ))
        .unwrap();
        assert_eq!(array_name.name().display(), "why");
        assert_eq!(array_name.name().as_str(), None);
        assert!(!array_name.name().matches("why"));

        let object_name = parse_tool_call_params(Some(
            &crate::core::json::parse("{\"name\":{\"tool\":\"why\"}}").unwrap(),
        ))
        .unwrap();
        assert_eq!(object_name.name().display(), "[object Object]");
        assert_eq!(stringify_compact(object_name.arguments()), "{}");
    }

    #[test]
    fn default_protocol_handler_keeps_tools_call_gated_until_project_handler_exists() {
        let response =
            handle_protocol_line("{\"jsonrpc\":\"2.0\",\"id\":11,\"method\":\"tools/call\"}")
                .unwrap();

        assert_eq!(
            stringify_compact(&response),
            "{\"jsonrpc\":\"2.0\",\"id\":11,\"error\":{\"code\":-32000,\"message\":\"Unsupported MCP method: tools/call\"}}"
        );
    }

    #[test]
    fn parses_why_and_ghost_check_arguments_for_future_tool_handlers() {
        let why = why_tool_arguments_from_json(
            &crate::core::json::parse(
                "{\"file\":\"src/main.ts\",\"anchor\":\"fn:main\",\"ignored\":true}",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(why.file.as_str(), "src/main.ts");
        assert_eq!(why.anchor.as_deref(), Some("fn:main"));
        assert_eq!(why.line, None);

        let why_without_anchor = why_tool_arguments_from_json(
            &crate::core::json::parse("{\"file\":\"src/main.ts\"}").unwrap(),
        )
        .unwrap();
        assert_eq!(why_without_anchor.anchor, None);
        assert_eq!(why_without_anchor.line, None);

        // B12: the `line` field is parsed (not silently dropped) and drives a
        // line-based lookup.
        let why_by_line = why_tool_arguments_from_json(
            &crate::core::json::parse("{\"file\":\"src/main.ts\",\"line\":42}").unwrap(),
        )
        .unwrap();
        assert_eq!(why_by_line.line, Some(42));

        let ghost = ghost_check_tool_arguments_from_json(
            &crate::core::json::parse("{\"file\":\"src/drift.ts\",\"anchor\":\"ignored\"}")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(ghost.file.as_str(), "src/drift.ts");
    }

    #[test]
    fn rejects_invalid_why_and_ghost_check_arguments() {
        let missing =
            why_tool_arguments_from_json(&crate::core::json::parse("{}").unwrap()).unwrap_err();
        assert_eq!(missing.user_message(), "file: missing required field");

        let empty_anchor = why_tool_arguments_from_json(
            &crate::core::json::parse("{\"file\":\"src/main.ts\",\"anchor\":\"\"}").unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            empty_anchor.user_message(),
            "anchor: expected non-empty string"
        );

        let non_object =
            ghost_check_tool_arguments_from_json(&crate::core::json::parse("[]").unwrap())
                .unwrap_err();
        assert_eq!(non_object.user_message(), "expected object");

        let hardened_path = ghost_check_tool_arguments_from_json(
            &crate::core::json::parse("{\"file\":\"../outside.ts\"}").unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            hardened_path.user_message(),
            "Invalid project-relative path \"../outside.ts\": parent path segments are not allowed"
        );

        // B12: a non-positive / non-integer `line` is a schema error, not a
        // silently-dropped field.
        for bad in ["0", "-3", "2.5", "\"7\"", "true"] {
            let error = why_tool_arguments_from_json(
                &crate::core::json::parse(&format!("{{\"file\":\"src/main.ts\",\"line\":{bad}}}"))
                    .unwrap(),
            )
            .unwrap_err();
            assert_eq!(
                error.user_message(),
                "line: expected a positive integer",
                "input line={bad}"
            );
        }
    }

    #[test]
    fn why_line_query_returns_decision_covering_the_line() {
        // B12: an MCP `why` call with a `line` resolves to the decision covering
        // that line — the same result as CLI `why <file> <line>` — instead of
        // dropping the field and returning a whole-file (wrong) answer.
        let root = unique_temp_dir("archiva-mcp-why-line");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src").join("multi.ts"),
            "function first() {\n  return 1;\n}\n\nfunction second() {\n  return 2;\n}\n",
        )
        .unwrap();

        let setup = handle_protocol_input(
            &root,
            concat!(
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"write_decision\",\"arguments\":{\"file\":\"src/multi.ts\",\"anchor\":\"fn:first\",\"lines\":[1,3],\"chose\":\"first choice\",\"because\":\"a\",\"rejected\":[]}}}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"write_decision\",\"arguments\":{\"file\":\"src/multi.ts\",\"anchor\":\"fn:second\",\"lines\":[5,7],\"chose\":\"second choice\",\"because\":\"b\",\"rejected\":[]}}}\n",
            ),
        )
        .unwrap();
        assert!(setup.contains("Recorded dec_001."));
        assert!(setup.contains("Recorded dec_002."));

        // Line 6 is inside fn:second (lines 5-7): must return the second decision.
        let by_line = handle_protocol_input(
            &root,
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"why\",\"arguments\":{\"file\":\"src/multi.ts\",\"line\":6}}}\n",
        )
        .unwrap();
        assert!(by_line.contains("second choice"), "{by_line}");
        assert!(!by_line.contains("first choice"), "{by_line}");

        // A line outside any decision returns a precise not-found, not a wrong
        // decision.
        let miss = handle_protocol_input(
            &root,
            "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"why\",\"arguments\":{\"file\":\"src/multi.ts\",\"line\":99}}}\n",
        )
        .unwrap();
        assert!(miss.contains("No decision found"), "{miss}");
        assert!(miss.contains("line 99"), "{miss}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn serves_initialize_and_tools_list_over_stdio() {
        let root = unique_temp_dir("archiva-mcp-stdio-list");
        let output = handle_protocol_input(
            &root,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n",
        )
        .unwrap();

        assert!(output.contains("\"protocolVersion\":\"2024-11-05\""));
        assert!(output.contains("\"name\":\"write_decision\""));
        assert!(output.contains("\"name\":\"ghost_check\""));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mcp_stdio_rejects_oversized_request_and_continues_session() {
        let root = unique_temp_dir("archiva-mcp-stdio-limit");
        let mut input = "{".repeat(crate::core::json::DEFAULT_MAX_BYTES + 1);
        input.push('\n');
        input.push_str("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n");

        let output = handle_protocol_input(&root, &input).unwrap();
        let responses = output.lines().collect::<Vec<_>>();
        assert_eq!(responses.len(), 2, "output={output}");
        assert_eq!(
            responses[0],
            "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32700,\"message\":\"JSON input exceeds configured byte limit\"}}"
        );
        assert!(responses[1].contains("\"id\":2"), "output={output}");
        assert!(responses[1].contains("\"tools\""), "output={output}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_handler_writes_reads_and_ghost_checks_decisions() {
        let root = unique_temp_dir("archiva-mcp-project-tools");
        let source_path = root.join("src").join("drift.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "export function compute() {\n  return 1;\n}\n",
        )
        .unwrap();

        let write = handle_protocol_input(
            &root,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"write_decision\",\"arguments\":{\"file\":\"src/drift.ts\",\"anchor\":\"fn:compute\",\"lines\":[1,3],\"chose\":\"return one\",\"because\":\"fixture\",\"rejected\":[]}}}\n",
        )
        .unwrap();
        assert_eq!(
            write,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"Recorded dec_001.\"}]}}\n"
        );

        let why = handle_protocol_input(
            &root,
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"why\",\"arguments\":{\"file\":\"src/drift.ts\",\"anchor\":\"fn:compute\"}}}\n",
        )
        .unwrap();
        assert!(why.contains("return one"));

        fs::write(
            &source_path,
            "export function compute() {\n  return 2;\n}\n",
        )
        .unwrap();
        let ghost = handle_protocol_input(
            &root,
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"ghost_check\",\"arguments\":{\"file\":\"src/drift.ts\"}}}\n",
        )
        .unwrap();
        assert_eq!(
            ghost,
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"arc/stale fn:compute: fn:compute code fingerprint differs from recorded decision\"}]}}\n"
        );

        let file = RelativePath::new("src/drift.ts").unwrap();
        assert!(fs::read_to_string(dlog_path(&root, &file))
            .unwrap()
            .contains("status: STALE"));
        assert_eq!(
            load_dlog(&root, &file)
                .unwrap()
                .unwrap()
                .decisions
                .get_str("fn:compute")
                .unwrap()
                .status
                .as_ref()
                .map(|status| status.as_str()),
            Some("STALE")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_handler_normalizes_tool_supplied_paths() {
        let root = unique_temp_dir("archiva-mcp-normalized-paths");
        let source_path = root.join("src").join("tool.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "export function toolTarget() {\n  return 1;\n}\n",
        )
        .unwrap();

        let write = handle_protocol_input(
            &root,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"write_decision\",\"arguments\":{\"file\":\".//src/tool.ts\",\"anchor\":\"fn:toolTarget\",\"lines\":[1,3],\"chose\":\"normalized through mcp\",\"because\":\"fixture\",\"rejected\":[]}}}\n",
        )
        .unwrap();
        assert_eq!(
            write,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"Recorded dec_001.\"}]}}\n"
        );

        let file = RelativePath::new("src/tool.ts").unwrap();
        assert!(dlog_path(&root, &file).exists());

        let why = handle_protocol_input(
            &root,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"why","arguments":{"file":"src\\tool.ts","anchor":"fn:toolTarget"}}}
"#,
        )
        .unwrap();
        assert!(why.contains("normalized through mcp"));

        fs::write(
            &source_path,
            "export function toolTarget() {\n  return 2;\n}\n",
        )
        .unwrap();
        let ghost = handle_protocol_input(
            &root,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ghost_check","arguments":{"file":".\\src\\tool.ts"}}}
"#,
        )
        .unwrap();
        assert_eq!(
            ghost,
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"arc/stale fn:toolTarget: fn:toolTarget code fingerprint differs from recorded decision\"}]}}\n"
        );
        assert_eq!(
            load_dlog(&root, &file)
                .unwrap()
                .unwrap()
                .decisions
                .get_str("fn:toolTarget")
                .unwrap()
                .status
                .as_ref()
                .map(|status| status.as_str()),
            Some("STALE")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ghost_check_is_file_scoped_and_does_not_mutate_unrelated_dlogs() {
        let root = unique_temp_dir("archiva-mcp-ghost-file-scoped");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src").join("target.ts"),
            "function target() {\n  return 1;\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src").join("unrelated.ts"),
            "function unrelated() {\n  return 1;\n}\n",
        )
        .unwrap();

        let output = handle_protocol_input(
            &root,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"write_decision\",\"arguments\":{\"file\":\"src/target.ts\",\"anchor\":\"fn:target\",\"lines\":[1,3],\"chose\":\"target\",\"because\":\"fixture\",\"rejected\":[]}}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"write_decision\",\"arguments\":{\"file\":\"src/unrelated.ts\",\"anchor\":\"fn:unrelated\",\"lines\":[1,3],\"chose\":\"unrelated\",\"because\":\"fixture\",\"rejected\":[]}}}\n",
        )
        .unwrap();
        assert!(output.contains("Recorded dec_001."));

        fs::write(
            root.join("src").join("unrelated.ts"),
            "function unrelated() {\n  return 2;\n}\n",
        )
        .unwrap();
        let ghost = handle_protocol_input(
            &root,
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"ghost_check\",\"arguments\":{\"file\":\"src/target.ts\"}}}\n",
        )
        .unwrap();
        assert_eq!(
            ghost,
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"No issues found for src/target.ts.\"}]}}\n"
        );

        let unrelated = RelativePath::new("src/unrelated.ts").unwrap();
        assert_eq!(
            load_dlog(&root, &unrelated)
                .unwrap()
                .unwrap()
                .decisions
                .get_str("fn:unrelated")
                .unwrap()
                .status,
            None
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ghost_check_repairs_missing_and_corrupt_dmap_for_requested_file() {
        for mode in ["missing", "corrupt"] {
            let root = unique_temp_dir(&format!("archiva-mcp-ghost-dmap-repair-{mode}"));
            fs::create_dir_all(root.join("src")).unwrap();
            fs::write(
                root.join("src").join("clean.ts"),
                "function clean() {\n  return 1;\n}\n",
            )
            .unwrap();

            let write = handle_protocol_input(
                &root,
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"write_decision\",\"arguments\":{\"file\":\"src/clean.ts\",\"anchor\":\"fn:clean\",\"lines\":[1,3],\"chose\":\"clean\",\"because\":\"fixture\",\"rejected\":[]}}}\n",
            )
            .unwrap();
            assert!(write.contains("Recorded dec_001."));

            let file = RelativePath::new("src/clean.ts").unwrap();
            match mode {
                "missing" => fs::remove_file(dmap_path(&root, &file)).unwrap(),
                "corrupt" => {
                    fs::write(dmap_path(&root, &file), "not:dmap:valid:enough?\n").unwrap()
                }
                _ => unreachable!(),
            }

            let ghost = handle_protocol_input(
                &root,
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"ghost_check\",\"arguments\":{\"file\":\"src/clean.ts\"}}}\n",
            )
            .unwrap();
            assert_eq!(
                ghost,
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"No issues found for src/clean.ts.\"}]}}\n"
            );
            assert_eq!(
                fs::read_to_string(dmap_path(&root, &file)).unwrap(),
                "1-3:fn:clean\n"
            );
            assert!(!decision_lock_path(&root, &file).exists());
            assert_no_temp_siblings(&root.join(".decisions").join("src"));

            let _ = fs::remove_dir_all(root);
        }
    }

    fn assert_no_temp_siblings(dir: &std::path::Path) {
        let temp_siblings = fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.contains(".archiva-tmp-"))
            .collect::<Vec<_>>();
        assert!(temp_siblings.is_empty(), "{temp_siblings:?}");
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
}
