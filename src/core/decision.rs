use crate::core::dlog::{
    DecisionHistoryEntry, DecisionRecord, DlogFile, LineRange, RejectedAlternative,
};
use crate::core::error::{ArchivaError, Result};
use crate::core::fingerprint::{fingerprint, get_lines};
use crate::core::json::{self, JsonObject, JsonValue};
use crate::core::paths::RelativePath;
use std::fmt::Write as _;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteDecisionInput {
    pub file: RelativePath,
    pub anchor: String,
    pub lines: LineRange,
    pub chose: String,
    pub because: String,
    pub rejected: Vec<RejectedAlternative>,
    pub expires_if: Option<String>,
    pub supersedes: Option<String>,
    pub session: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupersedePlan {
    pub anchor: String,
    pub history: Vec<crate::core::dlog::DecisionHistoryEntry>,
}

pub fn parse_write_decision_input_json(input: &str) -> Result<WriteDecisionInput> {
    let value = json::parse(input)?;
    write_decision_input_from_json(&value)
}

pub fn write_decision_input_from_json(value: &JsonValue) -> Result<WriteDecisionInput> {
    let object = expect_object(value, "")?;
    Ok(WriteDecisionInput {
        file: RelativePath::new(&expect_non_empty_string(
            required(object, "file", "file")?,
            "file",
        )?)?,
        anchor: expect_non_empty_string(required(object, "anchor", "anchor")?, "anchor")?,
        lines: parse_lines(required(object, "lines", "lines")?)?,
        chose: expect_non_empty_string(required(object, "chose", "chose")?, "chose")?,
        because: expect_non_empty_string(required(object, "because", "because")?, "because")?,
        rejected: parse_rejected(required(object, "rejected", "rejected")?)?,
        expires_if: optional_non_empty_string(object, "expires_if")?,
        supersedes: optional_non_empty_string(object, "supersedes")?,
        session: optional_non_empty_string(object, "session")?,
    })
}

pub fn next_decision_id(dlog: &DlogFile) -> Result<String> {
    let mut max_id = 0_u128;
    for (_, decision) in dlog.decisions.iter() {
        if let Some(value) = parse_decision_number(&decision.id) {
            max_id = max_id.max(value);
        }
    }
    let next = max_id
        .checked_add(1)
        .ok_or_else(|| ArchivaError::schema("id", "decision id counter overflow"))?;
    Ok(format!("dec_{next:03}"))
}

pub fn prepare_supersede(
    dlog: &DlogFile,
    supersedes: Option<&str>,
    because: &str,
) -> Result<Option<SupersedePlan>> {
    let Some(id) = supersedes else {
        return Ok(None);
    };
    let Some((anchor, record)) = find_decision_by_id(dlog, id) else {
        return Err(ArchivaError::cli(format!(
            "Cannot supersede unknown decision id \"{}\" in {}. Call why first and use the recorded decision id.",
            id,
            dlog.file.as_str()
        )));
    };

    let mut history = record.history.clone();
    history.push(crate::core::dlog::DecisionHistoryEntry {
        id: record.id.clone(),
        chose: record.chose.clone(),
        because: Some(record.because.clone()),
        timestamp: Some(record.timestamp.clone()),
        superseded_reason: Some(because.to_string()),
    });

    Ok(Some(SupersedePlan {
        anchor: anchor.to_string(),
        history,
    }))
}

pub fn build_decision_record(
    input: &WriteDecisionInput,
    id: impl Into<String>,
    source: &str,
    timestamp: impl Into<String>,
    env_session: Option<&str>,
    history: Vec<DecisionHistoryEntry>,
) -> DecisionRecord {
    // Snap the recorded range to the extractor's live span for the anchor, and
    // fingerprint that same span. `post_tool_use` re-anchors a resolving anchor
    // to exactly this extractor position (audit blocker B3), so deriving both
    // `lines_hint` and `fingerprint` from it at write time keeps the invariant
    // `fingerprint(get_lines(source, lines_hint)) == fingerprint` intact after a
    // re-anchor of unchanged code — otherwise a caller whose `input.lines`
    // differs from the AST node span (e.g. a body-only or approximate range from
    // an MCP `write_decision`) would be falsely marked STALE on the first
    // `post_tool_use`. Fall back to the caller's range only if the anchor does
    // not resolve (it was validated to exist, but the extractor may be
    // incomplete on pathological input).
    let anchor_range = crate::core::anchor::extract_anchors(&input.file, source)
        .anchors
        .get_str(&input.anchor)
        .map(|info| LineRange {
            start: info.start,
            end: info.end,
        })
        .unwrap_or_else(|| input.lines.clone());
    let selected_source = get_lines(
        source,
        anchor_range.start as usize,
        anchor_range.end as usize,
    );

    DecisionRecord {
        id: id.into(),
        lines_hint: anchor_range,
        fingerprint: fingerprint(&selected_source),
        chose: input.chose.clone(),
        because: input.because.clone(),
        rejected: input.rejected.clone(),
        expires_if: input.expires_if.clone(),
        session: input
            .session
            .clone()
            .or_else(|| env_session.map(str::to_string)),
        timestamp: timestamp.into(),
        history,
        status: None,
        stale_since: None,
        supersedes: input.supersedes.clone(),
    }
}

pub fn apply_decision_record(
    dlog: &mut DlogFile,
    anchor: String,
    decision: DecisionRecord,
    superseded_anchor: Option<&str>,
) {
    if superseded_anchor.is_some_and(|superseded| superseded != anchor) {
        if let Some(superseded) = superseded_anchor {
            dlog.decisions.remove_str(superseded);
        }
    }
    dlog.decisions.insert(anchor, decision);
}

pub fn why_from_dlog(dlog: Option<&DlogFile>, file: &RelativePath, anchor: Option<&str>) -> String {
    let Some(dlog) = dlog else {
        return format!("No decisions found for {}.", file.as_str());
    };

    let rendered = dlog
        .decisions
        .iter()
        .filter(|(key, _)| anchor.is_none_or(|anchor| key.as_str() == anchor))
        .map(|(key, decision)| format_decision(key, decision))
        .collect::<Vec<_>>();

    if rendered.is_empty() {
        return match anchor {
            Some(anchor) => format!("No decision found for {} at {}.", file.as_str(), anchor),
            None => format!("No decision found for {}.", file.as_str()),
        };
    }

    rendered.join("\n\n")
}

pub fn why_for_line_from_dlog(dlog: Option<&DlogFile>, file: &RelativePath, line: u32) -> String {
    let Some(dlog) = dlog else {
        return format!("No decisions found for {}.", file.as_str());
    };

    for (anchor, decision) in dlog.decisions.iter() {
        if line >= decision.lines_hint.start && line <= decision.lines_hint.end {
            return format_decision(anchor, decision);
        }
    }

    format!("No decision found for {} at line {}.", file.as_str(), line)
}

pub fn history_from_dlog(dlog: Option<&DlogFile>, file: &RelativePath, anchor: &str) -> String {
    let Some(decision) = dlog.and_then(|dlog| dlog.decisions.get_str(anchor)) else {
        return format!("No decision found for {} at {}.", file.as_str(), anchor);
    };

    let mut chain = Vec::new();
    for entry in &decision.history {
        chain.push(format_history_entry(
            &entry.id,
            &entry.chose,
            entry.because.as_deref(),
            entry.timestamp.as_deref(),
        ));
    }
    chain.push(format_history_entry(
        &decision.id,
        &decision.chose,
        Some(&decision.because),
        Some(&decision.timestamp),
    ));
    chain.join("\n\n")
}

pub fn session_start_from_dlogs(dlogs: &[DlogFile]) -> String {
    session_start_from_dlogs_with_total(dlogs.len(), dlogs)
}

pub fn session_start_from_dlogs_with_total(total_files: usize, dlogs: &[DlogFile]) -> String {
    let mut output = start_session_report(total_files);
    if total_files == 0 {
        return output;
    }

    for dlog in dlogs {
        append_session_report_dlog(&mut output, dlog);
    }

    finish_session_report(output)
}

pub fn start_session_report(total_files: usize) -> String {
    if total_files == 0 {
        return "[Archiva] No decision map found.".to_string();
    }

    format!(
        "[Archiva] Decision map loaded for {} files:\n\n",
        total_files
    )
}

pub fn append_session_report_dlog(output: &mut String, dlog: &DlogFile) {
    append_session_report_file(output, &dlog.file, dlog);
}

pub fn append_session_report_file(output: &mut String, file: &RelativePath, dlog: &DlogFile) {
    output.push_str(file.as_str());
    for (anchor, decision) in dlog.decisions.iter() {
        let status = decision
            .status
            .as_ref()
            .map(|status| format!(" {}", status.as_str()))
            .unwrap_or_default();
        let mut rejected = String::new();
        for (index, item) in decision.rejected.iter().take(2).enumerate() {
            if index > 0 {
                rejected.push_str(", ");
            }
            rejected.push_str(&item.approach);
            rejected.push('(');
            rejected.push_str(&compact(&item.reason));
            rejected.push(')');
        }
        write!(
            output,
            "\n  {}-{} {}{} -> {}{}",
            decision.lines_hint.start,
            decision.lines_hint.end,
            anchor,
            status,
            compact(&decision.chose),
            if rejected.is_empty() {
                String::new()
            } else {
                format!(" | x {rejected}")
            }
        )
        .expect("writing to a String cannot fail");
    }
    output.push_str("\n\n");
}

pub fn finish_session_report(output: String) -> String {
    trim_end_newlines(output)
}

/// Append a "skipped corrupt file" section to a session report so a malformed
/// `.dlog` is named rather than silently ignored (audit blocker B5). Each entry
/// is `(file, message)`. No-op when there are no corrupt files.
pub fn append_session_report_corrupt(output: &mut String, corrupt: &[(RelativePath, String)]) {
    if corrupt.is_empty() {
        return;
    }
    writeln!(
        output,
        "[Archiva] {} decision log{} could not be parsed and {} skipped:",
        corrupt.len(),
        if corrupt.len() == 1 { "" } else { "s" },
        if corrupt.len() == 1 { "was" } else { "were" },
    )
    .expect("writing to a String cannot fail");
    for (file, message) in corrupt {
        writeln!(output, "  {}: {}", file.as_str(), message)
            .expect("writing to a String cannot fail");
    }
    output.push('\n');
}

pub fn format_decision(anchor: &str, decision: &DecisionRecord) -> String {
    let status = decision
        .status
        .as_ref()
        .map(|status| format!(" [{}]", status.as_str()))
        .unwrap_or_default();
    let rejected = if decision.rejected.is_empty() {
        String::new()
    } else {
        format!(
            "\nRejected:\n{}",
            decision
                .rejected
                .iter()
                .map(|item| format!("  - {} -> {}", item.approach, item.reason))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let expires = decision
        .expires_if
        .as_ref()
        .map(|expires_if| format!("\nExpires if: {expires_if}"))
        .unwrap_or_default();
    let session = decision
        .session
        .as_ref()
        .map(|session| format!("  Session: {session}"))
        .unwrap_or_default();

    format!(
        "{} {} (lines {}-{}){}\nChose: {}\nBecause: {}{}\nRecorded: {}{}{}",
        anchor,
        decision.id,
        decision.lines_hint.start,
        decision.lines_hint.end,
        status,
        decision.chose,
        decision.because,
        rejected,
        decision.timestamp,
        session,
        expires
    )
}

fn parse_lines(value: &JsonValue) -> Result<LineRange> {
    let JsonValue::Array(values) = value else {
        return Err(ArchivaError::schema(
            "lines",
            "expected two positive integers",
        ));
    };
    if values.len() != 2 {
        return Err(ArchivaError::schema(
            "lines",
            "expected two positive integers",
        ));
    }
    let start = expect_positive_u32(&values[0], "lines.0")?;
    let end = expect_positive_u32(&values[1], "lines.1")?;
    if end < start {
        return Err(ArchivaError::schema("lines", "lines end must be >= start"));
    }
    Ok(LineRange { start, end })
}

fn parse_rejected(value: &JsonValue) -> Result<Vec<RejectedAlternative>> {
    let JsonValue::Array(values) = value else {
        return Err(ArchivaError::schema("rejected", "expected array"));
    };
    let mut rejected = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let field = format!("rejected.{index}");
        let object = expect_object(value, &field)?;
        rejected.push(RejectedAlternative {
            approach: expect_non_empty_string(
                required(object, "approach", &format!("{field}.approach"))?,
                &format!("{field}.approach"),
            )?,
            reason: expect_non_empty_string(
                required(object, "reason", &format!("{field}.reason"))?,
                &format!("{field}.reason"),
            )?,
        });
    }
    Ok(rejected)
}

fn optional_non_empty_string(object: &JsonObject, key: &str) -> Result<Option<String>> {
    object
        .get(key)
        .map(|value| expect_non_empty_string(value, key))
        .transpose()
}

fn required<'a>(object: &'a JsonObject, key: &str, field: &str) -> Result<&'a JsonValue> {
    object
        .get(key)
        .ok_or_else(|| ArchivaError::schema(field, "missing required field"))
}

fn expect_object<'a>(value: &'a JsonValue, field: &str) -> Result<&'a JsonObject> {
    match value {
        JsonValue::Object(object) => Ok(object),
        _ => Err(ArchivaError::schema(field, "expected object")),
    }
}

fn expect_non_empty_string(value: &JsonValue, field: &str) -> Result<String> {
    match value {
        JsonValue::String(value) if !value.is_empty() => Ok(value.clone()),
        JsonValue::String(_) => Err(ArchivaError::schema(field, "expected non-empty string")),
        _ => Err(ArchivaError::schema(field, "expected string")),
    }
}

fn expect_positive_u32(value: &JsonValue, field: &str) -> Result<u32> {
    match value {
        JsonValue::Number(value)
            if value.is_finite()
                && *value > 0.0
                && value.fract() == 0.0
                && *value <= u32::MAX as f64 =>
        {
            Ok(*value as u32)
        }
        _ => Err(ArchivaError::schema(field, "expected positive integer")),
    }
}

fn find_decision_by_id<'a>(dlog: &'a DlogFile, id: &str) -> Option<(&'a str, &'a DecisionRecord)> {
    dlog.decisions
        .iter()
        .find(|(_, record)| record.id == id)
        .map(|(anchor, record)| (anchor.as_str(), record))
}

fn parse_decision_number(id: &str) -> Option<u128> {
    let digits = id.strip_prefix("dec_")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u128>().ok()
}

fn format_history_entry(
    id: &str,
    chose: &str,
    because: Option<&str>,
    timestamp: Option<&str>,
) -> String {
    let because = because
        .filter(|because| !because.is_empty())
        .map(|because| format!("\n  Because: {because}"))
        .unwrap_or_default();
    format!(
        "{} {}\n  Chose: {}{}",
        id,
        timestamp.unwrap_or(""),
        chose,
        because
    )
}

fn compact(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}

fn trim_end_newlines(mut value: String) -> String {
    while value.ends_with('\n') {
        value.pop();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{
        apply_decision_record, build_decision_record, history_from_dlog, next_decision_id,
        parse_write_decision_input_json, prepare_supersede, session_start_from_dlogs,
        session_start_from_dlogs_with_total, why_for_line_from_dlog, why_from_dlog,
        write_decision_input_from_json, WriteDecisionInput,
    };
    use crate::core::dlog::{
        DecisionHistoryEntry, DecisionRecord, DlogFile, LineRange, RejectedAlternative,
    };
    use crate::core::dmap::DecisionStatus;
    use crate::core::fingerprint::{fingerprint, get_lines};
    use crate::core::json::{JsonObject, JsonValue};
    use crate::core::ordered_map::OrderedMap;
    use crate::core::paths::RelativePath;

    #[test]
    fn parses_write_decision_json_and_ignores_unknown_fields_like_zod() {
        let input = parse_write_decision_input_json(
            r#"{
              "file": "src/app.ts",
              "anchor": "fn:run",
              "lines": [2, 8],
              "chose": "use a std-only validator",
              "because": "shared validation avoids CLI and MCP drift",
              "rejected": [{"approach": "zod in Rust", "reason": "would add a dependency", "extra": true}],
              "expires_if": "schema changes",
              "supersedes": "dec_001",
              "session": "sess_123",
              "ignored": "field"
            }"#,
        )
        .unwrap();

        assert_eq!(input.file.as_str(), "src/app.ts");
        assert_eq!(input.anchor, "fn:run");
        assert_eq!(input.lines, LineRange { start: 2, end: 8 });
        assert_eq!(input.rejected[0].approach, "zod in Rust");
        assert_eq!(input.expires_if.as_deref(), Some("schema changes"));
        assert_eq!(input.supersedes.as_deref(), Some("dec_001"));
        assert_eq!(input.session.as_deref(), Some("sess_123"));
    }

    #[test]
    fn validates_required_non_empty_fields_and_line_refinement() {
        let error = parse_write_decision_input_json(
            r#"{"file":"src/app.ts","anchor":"fn:run","lines":[3,2],"chose":"x","because":"y","rejected":[]}"#,
        )
        .unwrap_err();
        assert_eq!(error.user_message(), "lines: lines end must be >= start");

        let error = parse_write_decision_input_json(
            r#"{"file":"src/app.ts","anchor":"fn:run","lines":[1,1],"chose":"","because":"y","rejected":[]}"#,
        )
        .unwrap_err();
        assert_eq!(error.user_message(), "chose: expected non-empty string");

        let error = parse_write_decision_input_json(
            r#"{"file":"src/app.ts","anchor":"fn:run","lines":[1,1],"chose":"x","because":"y","rejected":[{"approach":"x"}]}"#,
        )
        .unwrap_err();
        assert_eq!(
            error.user_message(),
            "rejected.0.reason: missing required field"
        );
    }

    #[test]
    fn rejects_non_object_inputs_bad_paths_and_bad_numeric_lines() {
        assert_eq!(
            parse_write_decision_input_json("[]")
                .unwrap_err()
                .user_message(),
            "expected object"
        );
        assert!(parse_write_decision_input_json(
            r#"{"file":"../x.ts","anchor":"fn:x","lines":[1,1],"chose":"x","because":"y","rejected":[]}"#
        )
        .unwrap_err()
        .user_message()
        .contains("parent path segments are not allowed"));
        assert_eq!(
            parse_write_decision_input_json(
                r#"{"file":"src/x.ts","anchor":"fn:x","lines":[1.5,2],"chose":"x","because":"y","rejected":[]}"#
            )
            .unwrap_err()
            .user_message(),
            "lines.0: expected positive integer"
        );
    }

    #[test]
    fn accepts_json_value_inputs_for_future_mcp_call_paths() {
        let value = JsonValue::Object(JsonObject::from_entries(vec![
            (
                "file".to_string(),
                JsonValue::String("src/mcp.ts".to_string()),
            ),
            (
                "anchor".to_string(),
                JsonValue::String("fn:tool".to_string()),
            ),
            (
                "lines".to_string(),
                JsonValue::Array(vec![JsonValue::Number(1.0), JsonValue::Number(3.0)]),
            ),
            (
                "chose".to_string(),
                JsonValue::String("validate once".to_string()),
            ),
            (
                "because".to_string(),
                JsonValue::String("MCP shares the contract".to_string()),
            ),
            ("rejected".to_string(), JsonValue::Array(Vec::new())),
        ]));

        let input = write_decision_input_from_json(&value).unwrap();
        assert_eq!(input.file.as_str(), "src/mcp.ts");
        assert_eq!(input.lines, LineRange { start: 1, end: 3 });
    }

    #[test]
    fn formats_why_why_for_line_and_history_like_typescript_contract() {
        let dlog = explain_fixture();
        let file = RelativePath::new("src/explain.ts").unwrap();

        assert_eq!(
            why_from_dlog(Some(&dlog), &file, Some("fn:first")),
            "fn:first dec_001 (lines 1-3) [STALE]\nChose: first approach with extra whitespace\nBecause: first reason\nRejected:\n  - class wrapper -> adds no behavior\n  - global helper -> hides coupling\n  - third hidden -> not shown in session map\nRecorded: 2026-06-26T20:31:18.340Z  Session: sess_a\nExpires if: api changes"
        );
        assert_eq!(
            why_for_line_from_dlog(Some(&dlog), &file, 6),
            "fn:second dec_002 (lines 5-8)\nChose: second approach\nwith newlines and      spaces\nBecause: second reason\nRecorded: 2026-06-26T20:32:18.340Z"
        );
        assert_eq!(
            history_from_dlog(Some(&dlog), &file, "fn:first"),
            "dec_000 2026-06-25T10:00:00.000Z\n  Chose: older approach\n  Because: older reason\n\ndec_001 2026-06-26T20:31:18.340Z\n  Chose: first approach with extra whitespace\n  Because: first reason"
        );
        assert_eq!(
            why_from_dlog(Some(&dlog), &file, None),
            "fn:first dec_001 (lines 1-3) [STALE]\nChose: first approach with extra whitespace\nBecause: first reason\nRejected:\n  - class wrapper -> adds no behavior\n  - global helper -> hides coupling\n  - third hidden -> not shown in session map\nRecorded: 2026-06-26T20:31:18.340Z  Session: sess_a\nExpires if: api changes\n\nfn:second dec_002 (lines 5-8)\nChose: second approach\nwith newlines and      spaces\nBecause: second reason\nRecorded: 2026-06-26T20:32:18.340Z"
        );
    }

    #[test]
    fn formats_missing_decision_messages_like_typescript_contract() {
        let dlog = explain_fixture();
        let file = RelativePath::new("src/explain.ts").unwrap();

        assert_eq!(
            why_from_dlog(None, &file, None),
            "No decisions found for src/explain.ts."
        );
        assert_eq!(
            why_from_dlog(Some(&dlog), &file, Some("fn:missing")),
            "No decision found for src/explain.ts at fn:missing."
        );
        assert_eq!(
            why_for_line_from_dlog(Some(&dlog), &file, 99),
            "No decision found for src/explain.ts at line 99."
        );
        assert_eq!(
            history_from_dlog(None, &file, "fn:first"),
            "No decision found for src/explain.ts at fn:first."
        );
    }

    #[test]
    fn formats_session_start_loaded_map_like_typescript_contract() {
        let dlog = explain_fixture();
        assert_eq!(
            session_start_from_dlogs(&[dlog]),
            "[Archiva] Decision map loaded for 1 files:\n\nsrc/explain.ts\n  1-3 fn:first STALE -> first approach with extra whitespace | x class wrapper(adds no behavior), global helper(hides coupling)\n  5-8 fn:second -> second approach with newlines and spaces"
        );
        assert_eq!(
            session_start_from_dlogs(&[]),
            "[Archiva] No decision map found."
        );
    }

    #[test]
    fn formats_session_start_discovered_count_and_empty_logs() {
        let empty = DlogFile {
            file: RelativePath::new("src/empty.ts").unwrap(),
            schema: 1,
            decisions: OrderedMap::new(),
        };

        assert_eq!(
            session_start_from_dlogs_with_total(2, &[empty]),
            "[Archiva] Decision map loaded for 2 files:\n\nsrc/empty.ts"
        );
    }

    #[test]
    fn generates_next_decision_id_from_max_matching_id() {
        let mut dlog = explain_fixture();
        dlog.decisions.insert(
            "fn:odd".to_string(),
            DecisionRecord {
                id: "not_dec_999".to_string(),
                ..fixture_record("not_dec_999")
            },
        );
        dlog.decisions.insert(
            "fn:wide".to_string(),
            DecisionRecord {
                id: "dec_010".to_string(),
                ..fixture_record("dec_010")
            },
        );

        assert_eq!(next_decision_id(&dlog).unwrap(), "dec_011");
        assert_eq!(
            next_decision_id(&DlogFile {
                file: RelativePath::new("src/empty.ts").unwrap(),
                schema: 1,
                decisions: OrderedMap::new(),
            })
            .unwrap(),
            "dec_001"
        );
    }

    #[test]
    fn rejects_decision_id_overflow() {
        let mut dlog = DlogFile {
            file: RelativePath::new("src/overflow.ts").unwrap(),
            schema: 1,
            decisions: OrderedMap::new(),
        };
        dlog.decisions.insert(
            "fn:max".to_string(),
            DecisionRecord {
                id: format!("dec_{}", u128::MAX),
                ..fixture_record("dec_999")
            },
        );

        assert_eq!(
            next_decision_id(&dlog).unwrap_err().user_message(),
            "id: decision id counter overflow"
        );
    }

    #[test]
    fn prepares_supersede_history_and_rejects_unknown_ids() {
        let dlog = explain_fixture();
        let plan = prepare_supersede(&dlog, Some("dec_001"), "superseding reason")
            .unwrap()
            .unwrap();

        assert_eq!(plan.anchor, "fn:first");
        assert_eq!(plan.history.len(), 2);
        assert_eq!(plan.history[0].id, "dec_000");
        assert_eq!(plan.history[1].id, "dec_001");
        assert_eq!(
            plan.history[1].chose,
            "first approach with extra whitespace"
        );
        assert_eq!(plan.history[1].because.as_deref(), Some("first reason"));
        assert_eq!(
            plan.history[1].timestamp.as_deref(),
            Some("2026-06-26T20:31:18.340Z")
        );
        assert_eq!(
            plan.history[1].superseded_reason.as_deref(),
            Some("superseding reason")
        );
        assert_eq!(prepare_supersede(&dlog, None, "reason").unwrap(), None);
        assert_eq!(
            prepare_supersede(&dlog, Some("missing"), "reason")
                .unwrap_err()
                .user_message(),
            "Cannot supersede unknown decision id \"missing\" in src/explain.ts. Call why first and use the recorded decision id."
        );
    }

    #[test]
    fn builds_decision_record_with_fingerprint_and_env_session_fallback() {
        let source = "export function fromEnv() {\n  return 1;\n}\n";
        let history = vec![DecisionHistoryEntry {
            id: "dec_000".to_string(),
            chose: "previous".to_string(),
            because: Some("older reason".to_string()),
            timestamp: Some("2026-06-25T10:00:00.000Z".to_string()),
            superseded_reason: Some("replacement".to_string()),
        }];
        let input = WriteDecisionInput {
            file: RelativePath::new("src/env.ts").unwrap(),
            anchor: "fn:fromEnv".to_string(),
            lines: LineRange { start: 1, end: 3 },
            chose: "use env session".to_string(),
            because: "fixture".to_string(),
            rejected: vec![RejectedAlternative {
                approach: "global default".to_string(),
                reason: "hides caller intent".to_string(),
            }],
            expires_if: Some("session policy changes".to_string()),
            supersedes: None,
            session: None,
        };

        let record = build_decision_record(
            &input,
            "dec_001",
            source,
            "2026-06-26T20:31:18.340Z",
            Some("env_session_contract"),
            history.clone(),
        );

        assert_eq!(record.id, "dec_001");
        assert_eq!(record.lines_hint, LineRange { start: 1, end: 3 });
        assert_eq!(record.fingerprint, fingerprint(&get_lines(source, 1, 3)));
        assert_eq!(record.chose, input.chose);
        assert_eq!(record.because, input.because);
        assert_eq!(record.rejected, input.rejected);
        assert_eq!(record.expires_if.as_deref(), Some("session policy changes"));
        assert_eq!(record.session.as_deref(), Some("env_session_contract"));
        assert_eq!(record.timestamp, "2026-06-26T20:31:18.340Z");
        assert_eq!(record.history, history);
        assert_eq!(record.status, None);
        assert_eq!(record.stale_since, None);
        assert_eq!(record.supersedes, None);
    }

    #[test]
    fn build_decision_record_prefers_input_session_over_env_fallback() {
        let input = WriteDecisionInput {
            file: RelativePath::new("src/session.ts").unwrap(),
            anchor: "fn:session".to_string(),
            lines: LineRange { start: 1, end: 1 },
            chose: "use explicit session".to_string(),
            because: "caller supplied it".to_string(),
            rejected: Vec::new(),
            expires_if: None,
            supersedes: None,
            session: Some("input_session".to_string()),
        };

        let record = build_decision_record(
            &input,
            "dec_001",
            "const value = 1;\n",
            "2026-06-26T20:31:18.340Z",
            Some("env_session"),
            Vec::new(),
        );

        assert_eq!(record.session.as_deref(), Some("input_session"));
    }

    #[test]
    fn builds_superseding_decision_record_with_prepared_history() {
        let dlog = explain_fixture();
        let plan = prepare_supersede(&dlog, Some("dec_001"), "superseding reason")
            .unwrap()
            .unwrap();
        let source =
            "export function first() {\n  return 1;\n}\nexport function second() {\n  return 2;\n}\n";
        let input = WriteDecisionInput {
            file: RelativePath::new("src/supersede.ts").unwrap(),
            anchor: "fn:second".to_string(),
            lines: LineRange { start: 4, end: 6 },
            chose: "second anchor".to_string(),
            because: "superseding reason".to_string(),
            rejected: vec![RejectedAlternative {
                approach: "keep first".to_string(),
                reason: "moved responsibility".to_string(),
            }],
            expires_if: None,
            supersedes: Some("dec_001".to_string()),
            session: None,
        };

        let record = build_decision_record(
            &input,
            "dec_002",
            source,
            "2026-06-26T20:32:18.340Z",
            None,
            plan.history.clone(),
        );

        assert_eq!(record.id, "dec_002");
        assert_eq!(record.lines_hint, LineRange { start: 4, end: 6 });
        assert_eq!(record.fingerprint, fingerprint(&get_lines(source, 4, 6)));
        assert_eq!(record.supersedes.as_deref(), Some("dec_001"));
        assert_eq!(record.history, plan.history);
        assert_eq!(record.history[1].id, "dec_001");
        assert_eq!(
            record.history[1].superseded_reason.as_deref(),
            Some("superseding reason")
        );
    }

    #[test]
    fn applies_superseding_decision_by_deleting_old_anchor_when_needed() {
        let mut dlog = explain_fixture();
        let mut replacement = fixture_record("dec_003");
        replacement.history = prepare_supersede(&dlog, Some("dec_001"), "new reason")
            .unwrap()
            .unwrap()
            .history;

        apply_decision_record(
            &mut dlog,
            "fn:third".to_string(),
            replacement,
            Some("fn:first"),
        );

        assert_eq!(
            dlog.decisions
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:second", "fn:third"]
        );
        assert_eq!(
            dlog.decisions.get_str("fn:third").unwrap().history[1].id,
            "dec_001"
        );
    }

    fn explain_fixture() -> DlogFile {
        DlogFile {
            file: RelativePath::new("src/explain.ts").unwrap(),
            schema: 1,
            decisions: OrderedMap::from_entries(vec![
                (
                    "fn:first".to_string(),
                    DecisionRecord {
                        id: "dec_001".to_string(),
                        lines_hint: LineRange { start: 1, end: 3 },
                        fingerprint: "11111111".to_string(),
                        chose: "first approach with extra whitespace".to_string(),
                        because: "first reason".to_string(),
                        rejected: vec![
                            RejectedAlternative {
                                approach: "class wrapper".to_string(),
                                reason: "adds no behavior".to_string(),
                            },
                            RejectedAlternative {
                                approach: "global helper".to_string(),
                                reason: "hides coupling".to_string(),
                            },
                            RejectedAlternative {
                                approach: "third hidden".to_string(),
                                reason: "not shown in session map".to_string(),
                            },
                        ],
                        expires_if: Some("api changes".to_string()),
                        session: Some("sess_a".to_string()),
                        timestamp: "2026-06-26T20:31:18.340Z".to_string(),
                        history: vec![DecisionHistoryEntry {
                            id: "dec_000".to_string(),
                            chose: "older approach".to_string(),
                            because: Some("older reason".to_string()),
                            timestamp: Some("2026-06-25T10:00:00.000Z".to_string()),
                            superseded_reason: Some("first reason".to_string()),
                        }],
                        status: Some(DecisionStatus::Stale),
                        stale_since: Some("2026-06-26T21:00:00.000Z".to_string()),
                        supersedes: None,
                    },
                ),
                (
                    "fn:second".to_string(),
                    DecisionRecord {
                        id: "dec_002".to_string(),
                        lines_hint: LineRange { start: 5, end: 8 },
                        fingerprint: "22222222".to_string(),
                        chose: "second approach\nwith newlines and      spaces".to_string(),
                        because: "second reason".to_string(),
                        rejected: Vec::new(),
                        expires_if: None,
                        session: None,
                        timestamp: "2026-06-26T20:32:18.340Z".to_string(),
                        history: Vec::new(),
                        status: None,
                        stale_since: None,
                        supersedes: None,
                    },
                ),
            ]),
        }
    }

    fn fixture_record(id: &str) -> DecisionRecord {
        DecisionRecord {
            id: id.to_string(),
            lines_hint: LineRange { start: 1, end: 3 },
            fingerprint: "deadbeef".to_string(),
            chose: "choice".to_string(),
            because: "reason".to_string(),
            rejected: Vec::new(),
            expires_if: None,
            session: None,
            timestamp: "2026-06-26T20:31:18.340Z".to_string(),
            history: Vec::new(),
            status: None,
            stale_since: None,
            supersedes: None,
        }
    }
}
