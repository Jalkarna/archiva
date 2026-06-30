use crate::core::dmap::DecisionStatus;
use crate::core::error::{ArchivaError, Result};
use crate::core::ordered_map::OrderedMap;
use crate::core::paths::RelativePath;
use crate::core::version::DLOG_SCHEMA_VERSION;
use crate::core::yaml::{parse_yaml, render_yaml, YamlObject, YamlValue};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedAlternative {
    pub approach: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionHistoryEntry {
    pub id: String,
    pub chose: String,
    pub because: Option<String>,
    pub timestamp: Option<String>,
    pub superseded_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionRecord {
    pub id: String,
    pub lines_hint: LineRange,
    pub fingerprint: String,
    pub chose: String,
    pub because: String,
    pub rejected: Vec<RejectedAlternative>,
    pub expires_if: Option<String>,
    pub session: Option<String>,
    pub timestamp: String,
    pub history: Vec<DecisionHistoryEntry>,
    pub status: Option<DecisionStatus>,
    pub stale_since: Option<String>,
    pub supersedes: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DlogFile {
    pub file: RelativePath,
    pub schema: u32,
    pub decisions: OrderedMap<String, DecisionRecord>,
}

pub fn parse_dlog_yaml(input: &str) -> Result<DlogFile> {
    let value = parse_yaml(input)?;
    parse_dlog_value(&value)
}

pub fn render_dlog_yaml(dlog: &DlogFile) -> Result<String> {
    Ok(render_yaml(&dlog_to_yaml(dlog)))
}

fn parse_dlog_value(value: &YamlValue) -> Result<DlogFile> {
    let object = expect_object(value, "")?;
    let file = expect_string(required(object, "file", "file")?, "file")?;
    let schema = expect_u32(required(object, "schema", "schema")?, "schema")?;
    if schema != DLOG_SCHEMA_VERSION {
        return Err(ArchivaError::schema(
            "schema",
            format!("expected schema version {DLOG_SCHEMA_VERSION}"),
        ));
    }
    let decisions_value = required(object, "decisions", "decisions")?;
    let decisions_object = expect_object(decisions_value, "decisions")?;
    let mut decisions = OrderedMap::new();
    for (anchor, decision_value) in decisions_object.entries() {
        decisions.insert(
            anchor.clone(),
            parse_decision_record(anchor, decision_value)?,
        );
    }
    Ok(DlogFile {
        file: RelativePath::new(&file)?,
        schema,
        decisions,
    })
}

fn parse_decision_record(anchor: &str, value: &YamlValue) -> Result<DecisionRecord> {
    let field = |name: &str| format!("decisions.{anchor}.{name}");
    let object = expect_object(value, &format!("decisions.{anchor}"))?;
    Ok(DecisionRecord {
        id: expect_non_empty_string(required(object, "id", &field("id"))?, &field("id"))?,
        lines_hint: parse_line_range(
            required(object, "lines_hint", &field("lines_hint"))?,
            &field("lines_hint"),
        )?,
        fingerprint: expect_non_empty_string(
            required(object, "fingerprint", &field("fingerprint"))?,
            &field("fingerprint"),
        )?,
        chose: expect_non_empty_string(
            required(object, "chose", &field("chose"))?,
            &field("chose"),
        )?,
        because: expect_non_empty_string(
            required(object, "because", &field("because"))?,
            &field("because"),
        )?,
        rejected: parse_rejected(
            required(object, "rejected", &field("rejected"))?,
            &field("rejected"),
        )?,
        expires_if: optional_string(object, "expires_if", &field("expires_if"))?,
        session: optional_string(object, "session", &field("session"))?,
        timestamp: expect_non_empty_string(
            required(object, "timestamp", &field("timestamp"))?,
            &field("timestamp"),
        )?,
        history: match object.get("history") {
            Some(value) => parse_history(value, &field("history"))?,
            None => Vec::new(),
        },
        status: optional_status(object, "status", &field("status"))?,
        stale_since: optional_string(object, "stale_since", &field("stale_since"))?,
        supersedes: optional_string(object, "supersedes", &field("supersedes"))?,
    })
}

fn parse_line_range(value: &YamlValue, field: &str) -> Result<LineRange> {
    let YamlValue::Array(values) = value else {
        return Err(ArchivaError::schema(
            field,
            "expected two positive integers",
        ));
    };
    if values.len() != 2 {
        return Err(ArchivaError::schema(
            field,
            "expected two positive integers",
        ));
    }
    let start = expect_positive_u32(&values[0], field)?;
    let end = expect_positive_u32(&values[1], field)?;
    Ok(LineRange { start, end })
}

fn parse_rejected(value: &YamlValue, field: &str) -> Result<Vec<RejectedAlternative>> {
    let YamlValue::Array(values) = value else {
        return Err(ArchivaError::schema(field, "expected an array"));
    };
    let mut rejected = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let item_field = format!("{field}.{index}");
        let object = expect_object(value, &item_field)?;
        rejected.push(RejectedAlternative {
            approach: expect_non_empty_string(
                required(object, "approach", &format!("{item_field}.approach"))?,
                &format!("{item_field}.approach"),
            )?,
            reason: expect_non_empty_string(
                required(object, "reason", &format!("{item_field}.reason"))?,
                &format!("{item_field}.reason"),
            )?,
        });
    }
    Ok(rejected)
}

fn parse_history(value: &YamlValue, field: &str) -> Result<Vec<DecisionHistoryEntry>> {
    let YamlValue::Array(values) = value else {
        return Err(ArchivaError::schema(field, "expected an array"));
    };
    let mut history = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let item_field = format!("{field}.{index}");
        let object = expect_object(value, &item_field)?;
        history.push(DecisionHistoryEntry {
            id: expect_non_empty_string(
                required(object, "id", &format!("{item_field}.id"))?,
                &format!("{item_field}.id"),
            )?,
            chose: expect_non_empty_string(
                required(object, "chose", &format!("{item_field}.chose"))?,
                &format!("{item_field}.chose"),
            )?,
            because: optional_string(object, "because", &format!("{item_field}.because"))?,
            timestamp: optional_string(object, "timestamp", &format!("{item_field}.timestamp"))?,
            superseded_reason: optional_string(
                object,
                "superseded_reason",
                &format!("{item_field}.superseded_reason"),
            )?,
        });
    }
    Ok(history)
}

fn dlog_to_yaml(dlog: &DlogFile) -> YamlValue {
    let mut root = YamlObject::new();
    root.insert(
        "file".to_string(),
        YamlValue::String(dlog.file.as_str().to_string()),
    );
    root.insert("schema".to_string(), YamlValue::Number(dlog.schema as i64));

    let mut decisions = YamlObject::new();
    for (anchor, decision) in dlog.decisions.iter() {
        decisions.insert(anchor.clone(), decision_to_yaml(decision));
    }
    root.insert("decisions".to_string(), YamlValue::Object(decisions));
    YamlValue::Object(root)
}

fn decision_to_yaml(decision: &DecisionRecord) -> YamlValue {
    let mut object = YamlObject::new();
    object.insert("id".to_string(), YamlValue::String(decision.id.clone()));
    object.insert(
        "lines_hint".to_string(),
        YamlValue::Array(vec![
            YamlValue::Number(decision.lines_hint.start as i64),
            YamlValue::Number(decision.lines_hint.end as i64),
        ]),
    );
    object.insert(
        "fingerprint".to_string(),
        YamlValue::String(decision.fingerprint.clone()),
    );
    object.insert(
        "chose".to_string(),
        YamlValue::String(decision.chose.clone()),
    );
    object.insert(
        "because".to_string(),
        YamlValue::String(decision.because.clone()),
    );
    object.insert("rejected".to_string(), rejected_to_yaml(&decision.rejected));
    insert_optional_string(&mut object, "expires_if", &decision.expires_if);
    insert_optional_string(&mut object, "session", &decision.session);
    object.insert(
        "timestamp".to_string(),
        YamlValue::String(decision.timestamp.clone()),
    );
    object.insert("history".to_string(), history_to_yaml(&decision.history));
    if let Some(status) = &decision.status {
        object.insert(
            "status".to_string(),
            YamlValue::String(status.as_str().to_string()),
        );
    }
    insert_optional_string(&mut object, "stale_since", &decision.stale_since);
    insert_optional_string(&mut object, "supersedes", &decision.supersedes);
    YamlValue::Object(object)
}

fn rejected_to_yaml(rejected: &[RejectedAlternative]) -> YamlValue {
    YamlValue::Array(
        rejected
            .iter()
            .map(|entry| {
                YamlValue::Object(YamlObject::from_entries(vec![
                    (
                        "approach".to_string(),
                        YamlValue::String(entry.approach.clone()),
                    ),
                    (
                        "reason".to_string(),
                        YamlValue::String(entry.reason.clone()),
                    ),
                ]))
            })
            .collect(),
    )
}

fn history_to_yaml(history: &[DecisionHistoryEntry]) -> YamlValue {
    YamlValue::Array(
        history
            .iter()
            .map(|entry| {
                let mut object = YamlObject::new();
                object.insert("id".to_string(), YamlValue::String(entry.id.clone()));
                object.insert("chose".to_string(), YamlValue::String(entry.chose.clone()));
                insert_optional_string(&mut object, "because", &entry.because);
                insert_optional_string(&mut object, "timestamp", &entry.timestamp);
                insert_optional_string(&mut object, "superseded_reason", &entry.superseded_reason);
                YamlValue::Object(object)
            })
            .collect(),
    )
}

fn insert_optional_string(object: &mut YamlObject, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        object.insert(key.to_string(), YamlValue::String(value.clone()));
    }
}

fn optional_string(object: &YamlObject, key: &str, field: &str) -> Result<Option<String>> {
    object
        .get(key)
        .map(|value| expect_string(value, field))
        .transpose()
}

fn optional_status(object: &YamlObject, key: &str, field: &str) -> Result<Option<DecisionStatus>> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let value = expect_string(value, field)?;
    DecisionStatus::parse(&value)
        .ok_or_else(|| ArchivaError::schema(field, "expected UNDECIDED, STALE, or ORPHAN"))
        .map(Some)
}

fn required<'a>(object: &'a YamlObject, key: &str, field: &str) -> Result<&'a YamlValue> {
    object
        .get(key)
        .ok_or_else(|| ArchivaError::schema(field, "missing required field"))
}

fn expect_object<'a>(value: &'a YamlValue, field: &str) -> Result<&'a YamlObject> {
    match value {
        YamlValue::Object(object) => Ok(object),
        _ => Err(ArchivaError::schema(field, "expected object")),
    }
}

fn expect_string(value: &YamlValue, field: &str) -> Result<String> {
    match value {
        YamlValue::String(value) => Ok(value.clone()),
        _ => Err(ArchivaError::schema(field, "expected string")),
    }
}

fn expect_non_empty_string(value: &YamlValue, field: &str) -> Result<String> {
    let value = expect_string(value, field)?;
    if value.is_empty() {
        return Err(ArchivaError::schema(field, "expected non-empty string"));
    }
    Ok(value)
}

fn expect_u32(value: &YamlValue, field: &str) -> Result<u32> {
    match value {
        YamlValue::Number(value) if *value >= 0 && *value <= u32::MAX as i64 => Ok(*value as u32),
        _ => Err(ArchivaError::schema(field, "expected integer")),
    }
}

fn expect_positive_u32(value: &YamlValue, field: &str) -> Result<u32> {
    match value {
        YamlValue::Number(value) if *value > 0 && *value <= u32::MAX as i64 => Ok(*value as u32),
        _ => Err(ArchivaError::schema(field, "expected positive integer")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_dlog_yaml, render_dlog_yaml, DecisionRecord, DlogFile, LineRange, RejectedAlternative,
    };
    use crate::core::ordered_map::OrderedMap;
    use crate::core::paths::RelativePath;
    use crate::core::yaml::DEFAULT_MAX_DEPTH;

    #[test]
    fn parses_js_yaml_dlog_reader_contract() {
        let dlog = parse_dlog_yaml(
            "# top-level comments are ignored\nfile: src/yaml.ts\nschema: 1\ndecisions:\n  fn:yaml:\n    id: \"dec_011\"\n    lines_hint: [1, 3]\n    fingerprint: abc123ef\n    chose: \"double quoted choice\"\n    because: >-\n      folded line one\n      folded line two\n    rejected:\n      - approach: 'single quoted: option # literal'\n        reason: |-\n          literal first\n          literal second\n    timestamp: '2026-06-26T20:31:18.340Z'\n",
        )
        .unwrap();

        assert_eq!(dlog.file.as_str(), "src/yaml.ts");
        assert_eq!(
            dlog.decisions
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:yaml"]
        );
        let decision = dlog.decisions.get_str("fn:yaml").unwrap();
        assert_eq!(decision.id, "dec_011");
        assert_eq!(decision.lines_hint, LineRange { start: 1, end: 3 });
        assert_eq!(decision.because, "folded line one folded line two");
        assert_eq!(
            decision.rejected[0].approach,
            "single quoted: option # literal"
        );
        assert_eq!(decision.rejected[0].reason, "literal first\nliteral second");
        assert!(decision.history.is_empty());
    }

    #[test]
    fn parses_js_yaml_flow_dlog_reader_contract() {
        let dlog = parse_dlog_yaml(
            "file: src/flow.ts\nschema: 1\ndecisions: {fn:flow: {id: dec_012, lines_hint: [1, 3], fingerprint: abc123ef, chose: \"flow choice\", because: \"line\\nreason\", rejected: [{approach: \"flow object\", reason: 'flow: # literal'}], timestamp: '2026-06-26T20:31:18.340Z', history: []}}\n",
        )
        .unwrap();

        assert_eq!(dlog.file.as_str(), "src/flow.ts");
        let decision = dlog.decisions.get_str("fn:flow").unwrap();
        assert_eq!(decision.id, "dec_012");
        assert_eq!(decision.lines_hint, LineRange { start: 1, end: 3 });
        assert_eq!(decision.chose, "flow choice");
        assert_eq!(decision.because, "line\nreason");
        assert_eq!(decision.rejected[0].approach, "flow object");
        assert_eq!(decision.rejected[0].reason, "flow: # literal");
        assert!(decision.history.is_empty());
    }

    #[test]
    fn renders_js_yaml_writer_contract_shape() {
        let dlog = DlogFile {
            file: RelativePath::new("src/wrap.ts").unwrap(),
            schema: 1,
            decisions: OrderedMap::from_entries(vec![(
                "fn:wrap".to_string(),
                DecisionRecord {
                    id: "dec_010".to_string(),
                    lines_hint: LineRange { start: 2, end: 8 },
                    fingerprint: "abc123ef".to_string(),
                    chose: "plain string with colon: and hash # kept as scalar".to_string(),
                    because:
                        "This reason intentionally crosses the one hundred character js-yaml wrapping boundary so Rust can verify plain scalar wrapping parity later."
                            .to_string(),
                    rejected: vec![RejectedAlternative {
                        approach: "flow array [x, y]".to_string(),
                        reason:
                            "A long rejected reason also crosses the wrapping boundary so formatting remains visible in golden tests."
                                .to_string(),
                    }],
                    expires_if: Some("2026-01-02T03:04:05.000Z".to_string()),
                    session: Some("sess_contract".to_string()),
                    timestamp: "2026-06-26T20:31:18.340Z".to_string(),
                    history: Vec::new(),
                    status: None,
                    stale_since: None,
                    supersedes: None,
                },
            )]),
        };

        assert_eq!(
            render_dlog_yaml(&dlog).unwrap(),
            "file: src/wrap.ts\nschema: 1\ndecisions:\n  fn:wrap:\n    id: dec_010\n    lines_hint:\n      - 2\n      - 8\n    fingerprint: abc123ef\n    chose: 'plain string with colon: and hash # kept as scalar'\n    because: >-\n      This reason intentionally crosses the one hundred character js-yaml wrapping boundary so Rust\n      can verify plain scalar wrapping parity later.\n    rejected:\n      - approach: flow array [x, y]\n        reason: >-\n          A long rejected reason also crosses the wrapping boundary so formatting remains visible in\n          golden tests.\n    expires_if: '2026-01-02T03:04:05.000Z'\n    session: sess_contract\n    timestamp: '2026-06-26T20:31:18.340Z'\n    history: []\n"
        );
    }

    #[test]
    fn rejects_invalid_dlog_schema_shapes() {
        assert!(
            parse_dlog_yaml("file: src/a.ts\nschema: 2\ndecisions:\n  fn:a:\n    id: dec\n")
                .unwrap_err()
                .user_message()
                .contains("schema")
        );
        assert!(
            parse_dlog_yaml("file: src/a.ts\nschema: 1\ndecisions:\n  fn:a:\n    id: dec\n")
                .unwrap_err()
                .user_message()
                .contains("lines_hint")
        );
    }

    #[test]
    fn parse_dlog_yaml_rejects_deeply_nested_ignored_block_field() {
        let error = parse_dlog_yaml(&valid_dlog_with_extra(&deep_block_unknown_field(
            DEFAULT_MAX_DEPTH + 2,
        )))
        .unwrap_err()
        .user_message();

        assert!(error.contains("YAML nesting exceeds configured depth limit"));
    }

    #[test]
    fn parse_dlog_yaml_rejects_deeply_nested_ignored_flow_field() {
        let error = parse_dlog_yaml(&valid_dlog_with_extra(&deep_flow_unknown_field(
            DEFAULT_MAX_DEPTH + 2,
        )))
        .unwrap_err()
        .user_message();

        assert!(error.contains("YAML nesting exceeds configured depth limit"));
    }

    fn valid_dlog_with_extra(extra: &str) -> String {
        format!(
            "file: src/deep.ts\nschema: 1\ndecisions:\n  fn:deep:\n    id: dec_001\n    lines_hint: [1, 3]\n    fingerprint: abc123ef\n    chose: bounded yaml\n    because: fixture\n    rejected: []\n    timestamp: '2026-06-26T20:31:18.340Z'\n    history: []\n{extra}"
        )
    }

    fn deep_block_unknown_field(depth: usize) -> String {
        let mut field = String::from("ignored:\n");
        for level in 0..depth {
            field.push_str(&"  ".repeat(level + 1));
            field.push_str(&format!("level_{level}:\n"));
        }
        field.push_str(&"  ".repeat(depth + 1));
        field.push_str("leaf: value\n");
        field
    }

    fn deep_flow_unknown_field(depth: usize) -> String {
        format!("ignored: {}0{}\n", "[".repeat(depth), "]".repeat(depth))
    }
}
