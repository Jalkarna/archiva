use crate::core::error::{ArchivaError, Result};
use crate::core::json::{self, JsonObject, JsonValue};

const SESSION_START_COMMAND: &str = "archiva hooks session-start";
const POST_TOOL_USE_COMMAND: &str = "archiva hooks post-tool-use";
const POST_TOOL_USE_MATCHER: &str = "Write|Edit|MultiEdit";

pub fn merge_claude_settings_json(existing: Option<&str>) -> Result<String> {
    let value = match existing {
        Some(input) if !input.trim().is_empty() => json::parse(input)?,
        _ => JsonValue::Object(JsonObject::new()),
    };
    let merged = merge_claude_settings_value(value)?;
    Ok(format!("{}\n", json::stringify_pretty(&merged, 2)))
}

pub fn merge_claude_settings_value(existing: JsonValue) -> Result<JsonValue> {
    let JsonValue::Object(mut settings) = existing else {
        return Err(ArchivaError::schema(
            ".claude/settings.json",
            "expected object",
        ));
    };

    let existing_hooks = match settings.get("hooks") {
        Some(JsonValue::Object(object)) => object.clone(),
        _ => JsonObject::new(),
    };
    let mut hooks = existing_hooks;
    let session_start = merge_hook_groups(
        hooks.get("SessionStart"),
        IncomingHookGroup {
            matcher: None,
            command: SESSION_START_COMMAND,
        },
    );
    hooks.insert("SessionStart".to_string(), JsonValue::Array(session_start));
    let post_tool_use = merge_hook_groups(
        hooks.get("PostToolUse"),
        IncomingHookGroup {
            matcher: Some(POST_TOOL_USE_MATCHER),
            command: POST_TOOL_USE_COMMAND,
        },
    );
    hooks.insert("PostToolUse".to_string(), JsonValue::Array(post_tool_use));
    settings.insert("hooks".to_string(), JsonValue::Object(hooks));

    let mut mcp_servers = match settings.get("mcpServers") {
        Some(JsonValue::Object(object)) => object.clone(),
        _ => JsonObject::new(),
    };
    mcp_servers.insert("archiva".to_string(), archiva_mcp_server());
    settings.insert("mcpServers".to_string(), JsonValue::Object(mcp_servers));

    Ok(JsonValue::Object(settings))
}

#[derive(Clone, Copy)]
struct IncomingHookGroup {
    matcher: Option<&'static str>,
    command: &'static str,
}

fn merge_hook_groups(existing: Option<&JsonValue>, incoming: IncomingHookGroup) -> Vec<JsonValue> {
    let mut current = existing_hook_groups(existing);

    if let Some(matcher) = incoming.matcher {
        if let Some(group) = current
            .iter_mut()
            .find(|group| group_matcher(group).as_deref() == Some(matcher))
        {
            if !group_has_command(group, incoming.command) {
                push_hook_command(group, incoming.command);
            }
            return current;
        }
        current.push(hook_group(Some(matcher), incoming.command));
        return current;
    }

    if !current
        .iter()
        .any(|group| group_has_command(group, incoming.command))
    {
        current.push(hook_group(None, incoming.command));
    }
    current
}

fn existing_hook_groups(existing: Option<&JsonValue>) -> Vec<JsonValue> {
    let Some(JsonValue::Array(groups)) = existing else {
        return Vec::new();
    };
    groups.iter().map(normalize_hook_group).collect()
}

fn normalize_hook_group(value: &JsonValue) -> JsonValue {
    let mut group = match value {
        JsonValue::Object(object) => object.clone(),
        _ => JsonObject::new(),
    };
    let hooks = match group.get("hooks") {
        Some(JsonValue::Array(hooks)) => JsonValue::Array(hooks.clone()),
        _ => JsonValue::Array(Vec::new()),
    };
    group.insert("hooks".to_string(), hooks);
    JsonValue::Object(group)
}

fn group_matcher(group: &JsonValue) -> Option<String> {
    match group {
        JsonValue::Object(object) => match object.get("matcher") {
            Some(JsonValue::String(matcher)) => Some(matcher.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn group_has_command(group: &JsonValue, command: &str) -> bool {
    let JsonValue::Object(object) = group else {
        return false;
    };
    let Some(JsonValue::Array(hooks)) = object.get("hooks") else {
        return false;
    };
    hooks
        .iter()
        .any(|hook| hook_command_value(hook) == Some(command))
}

fn push_hook_command(group: &mut JsonValue, command: &str) {
    let JsonValue::Object(object) = group else {
        return;
    };
    match object.get_mut("hooks") {
        Some(JsonValue::Array(hooks)) => hooks.push(hook_command(command)),
        _ => object.insert(
            "hooks".to_string(),
            JsonValue::Array(vec![hook_command(command)]),
        ),
    }
}

fn hook_command_value(hook: &JsonValue) -> Option<&str> {
    match hook {
        JsonValue::Object(object) => match object.get("command") {
            Some(JsonValue::String(command)) => Some(command.as_str()),
            _ => None,
        },
        _ => None,
    }
}

fn hook_group(matcher: Option<&str>, command: &str) -> JsonValue {
    let mut group = JsonObject::new();
    if let Some(matcher) = matcher {
        group.insert(
            "matcher".to_string(),
            JsonValue::String(matcher.to_string()),
        );
    }
    group.insert(
        "hooks".to_string(),
        JsonValue::Array(vec![hook_command(command)]),
    );
    JsonValue::Object(group)
}

fn hook_command(command: &str) -> JsonValue {
    JsonValue::Object(JsonObject::from_entries(vec![
        ("type".to_string(), JsonValue::String("command".to_string())),
        (
            "command".to_string(),
            JsonValue::String(command.to_string()),
        ),
    ]))
}

fn archiva_mcp_server() -> JsonValue {
    JsonValue::Object(JsonObject::from_entries(vec![
        (
            "command".to_string(),
            JsonValue::String("archiva".to_string()),
        ),
        (
            "args".to_string(),
            JsonValue::Array(vec![JsonValue::String("mcp".to_string())]),
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::merge_claude_settings_json;
    use crate::core::json::{parse, JsonValue};

    #[test]
    fn renders_fresh_claude_settings_with_archiva_hooks_and_mcp_server() {
        assert_eq!(
            merge_claude_settings_json(None).unwrap(),
            "{\n  \"hooks\": {\n    \"SessionStart\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"archiva hooks session-start\"\n          }\n        ]\n      }\n    ],\n    \"PostToolUse\": [\n      {\n        \"matcher\": \"Write|Edit|MultiEdit\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"archiva hooks post-tool-use\"\n          }\n        ]\n      }\n    ]\n  },\n  \"mcpServers\": {\n    \"archiva\": {\n      \"command\": \"archiva\",\n      \"args\": [\n        \"mcp\"\n      ]\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn merges_existing_settings_preserving_order_and_overwriting_archiva_server() {
        let output = merge_claude_settings_json(Some(
            "{\n  \"mcpServers\": {\n    \"other\": { \"command\": \"other-tool\", \"args\": [\"serve\"] },\n    \"archiva\": { \"command\": \"old-archiva\", \"args\": [\"old\"] }\n  },\n  \"hooks\": {\n    \"SessionStart\": [\n      { \"hooks\": [{ \"type\": \"command\", \"command\": \"echo existing\" }] }\n    ]\n  }\n}\n",
        ))
        .unwrap();

        assert_eq!(
            output,
            "{\n  \"mcpServers\": {\n    \"other\": {\n      \"command\": \"other-tool\",\n      \"args\": [\n        \"serve\"\n      ]\n    },\n    \"archiva\": {\n      \"command\": \"archiva\",\n      \"args\": [\n        \"mcp\"\n      ]\n    }\n  },\n  \"hooks\": {\n    \"SessionStart\": [\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"echo existing\"\n          }\n        ]\n      },\n      {\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"archiva hooks session-start\"\n          }\n        ]\n      }\n    ],\n    \"PostToolUse\": [\n      {\n        \"matcher\": \"Write|Edit|MultiEdit\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"archiva hooks post-tool-use\"\n          }\n        ]\n      }\n    ]\n  }\n}\n"
        );
    }

    #[test]
    fn deduplicates_archiva_hooks_without_removing_custom_hooks() {
        let output = merge_claude_settings_json(Some(
            "{\n  \"hooks\": {\n    \"SessionStart\": [\n      { \"hooks\": [{ \"type\": \"command\", \"command\": \"archiva hooks session-start\" }] }\n    ],\n    \"PostToolUse\": [\n      { \"matcher\": \"Write|Edit|MultiEdit\", \"hooks\": [\n        { \"type\": \"command\", \"command\": \"echo post\" },\n        { \"type\": \"command\", \"command\": \"archiva hooks post-tool-use\" }\n      ] }\n    ]\n  }\n}\n",
        ))
        .unwrap();
        let parsed = parse(&output).unwrap();
        let JsonValue::Object(root) = parsed else {
            panic!("expected object");
        };
        let hooks = root.get("hooks").unwrap();
        let text = crate::core::json::stringify_compact(hooks);

        assert_eq!(text.matches("archiva hooks session-start").count(), 1);
        assert_eq!(text.matches("archiva hooks post-tool-use").count(), 1);
        assert!(text.contains("echo post"));
    }

    #[test]
    fn rejects_non_object_settings_json() {
        assert_eq!(
            merge_claude_settings_json(Some("[]"))
                .unwrap_err()
                .user_message(),
            ".claude/settings.json: expected object"
        );
    }
}
