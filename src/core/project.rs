use std::collections::{HashMap, HashSet};
use std::env;
use std::path::{Component, Path, PathBuf};

use crate::core::anchor::{assert_anchor_exists, extract_anchors, AnchorExtraction, AnchorKind};
use crate::core::decision::{
    append_session_report_file, finish_session_report, history_from_dlog, start_session_report,
    why_for_line_from_dlog, why_from_dlog, WriteDecisionInput,
};
use crate::core::decision_status::{
    clear_recovered_status, is_fingerprint_stale, mark_orphan, mark_stale_now,
};
use crate::core::diff::{apply_line_changes_to_range, diff_lines};
use crate::core::dlog::DlogFile;
use crate::core::error::{ArchivaError, Result};
use crate::core::fingerprint::{fingerprint, get_lines};
use crate::core::fs::{
    list_files, list_storage_files, path_exists, read_text_file_with_limit, read_text_if_exists,
    read_text_if_exists_with_limit, SOURCE_FILE_MAX_BYTES,
};
use crate::core::git::{git_renamed_from, read_git_head_file};
use crate::core::gitignore::GitignoreMatcher;
use crate::core::lint::{LintIssue, LintRule, LintSeverity};
use crate::core::paths::{
    canonical_source_path_if_exists, dlog_path, source_path_from_decision_file, RelativePath,
};
use crate::core::status::{format_status_report, status_summary_from_dlog, StatusFileSummary};
use crate::core::storage::{
    ensure_dmap_current, ensure_dmap_current_locked, load_dlog, move_dlog_and_dmap_locked,
    with_decision_file_lock, write_decision_record_locked, write_dlog, write_dmap,
};
use crate::core::time::now_utc_millis;

const SOURCE_EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "rs"];

pub fn why(project_root: &Path, file: &RelativePath, anchor: Option<&str>) -> Result<String> {
    let dlog = load_dlog(project_root, file)?;
    Ok(why_from_dlog(dlog.as_ref(), file, anchor))
}

pub fn why_for_line(project_root: &Path, file: &RelativePath, line: u32) -> Result<String> {
    let dlog = load_dlog(project_root, file)?;
    Ok(why_for_line_from_dlog(dlog.as_ref(), file, line))
}

pub fn history(project_root: &Path, file: &RelativePath, anchor: &str) -> Result<String> {
    let dlog = load_dlog(project_root, file)?;
    Ok(history_from_dlog(dlog.as_ref(), file, anchor))
}

pub fn list_dlog_files(project_root: &Path) -> Result<Vec<PathBuf>> {
    list_storage_files(&project_root.join(".decisions"), |path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".dlog"))
    })
}

pub fn decision_file_to_source(
    project_root: &Path,
    decision_file_path: &Path,
) -> Result<RelativePath> {
    source_path_from_decision_file(project_root, decision_file_path)
}

pub fn session_start(project_root: &Path) -> Result<String> {
    let dlog_files = list_dlog_files(project_root)?;
    if dlog_files.is_empty() {
        return Ok(start_session_report(0));
    }

    let mut output = start_session_report(dlog_files.len());
    for dlog_file in &dlog_files {
        let file = decision_file_to_source(project_root, dlog_file)?;
        if let Some(dlog) = load_dlog(project_root, &file)? {
            ensure_dmap_current(project_root, &dlog, "session-start")?;
            append_session_report_file(&mut output, &file, &dlog);
        }
    }

    Ok(finish_session_report(output))
}

pub fn status(project_root: &Path) -> Result<String> {
    let summaries = load_project_status_summaries(project_root)?;
    let issue_count = lint_project_issue_count(project_root, false)?;
    Ok(format_status_report(&summaries, issue_count))
}

pub fn lint_project(project_root: &Path, fix: bool) -> Result<Vec<LintIssue>> {
    Ok(lint_project_inner(project_root, fix, true)?.issues)
}

pub fn lint_file(project_root: &Path, file: &RelativePath, fix: bool) -> Result<Vec<LintIssue>> {
    let mut issues = Vec::new();
    let mut decided = None::<HashSet<String>>;
    if let Some((dlog, dlog_issues)) = lint_dlog_locked(project_root, file, fix)? {
        decided = Some(
            dlog.decisions
                .iter()
                .map(|(anchor, _)| anchor.clone())
                .collect(),
        );
        issues.extend(dlog_issues);
    }

    lint_complex_undecided_file(project_root, file, decided.as_ref(), true, &mut issues)?;
    Ok(issues)
}

fn lint_project_issue_count(project_root: &Path, fix: bool) -> Result<usize> {
    Ok(lint_project_inner(project_root, fix, false)?.issue_count)
}

struct LintProjectOutput {
    issues: Vec<LintIssue>,
    issue_count: usize,
}

fn lint_project_inner(
    project_root: &Path,
    fix: bool,
    collect_issues: bool,
) -> Result<LintProjectOutput> {
    let dlog_files = list_dlog_files(project_root)?;
    let mut issues = Vec::new();
    let mut issue_count = 0_usize;
    let mut decisions_by_file = HashMap::<String, HashSet<String>>::new();

    for dlog_file in dlog_files {
        let file = decision_file_to_source(project_root, &dlog_file)?;
        let Some((dlog, dlog_issues)) = lint_dlog_locked(project_root, &file, fix)? else {
            continue;
        };
        issue_count += dlog_issues.len();
        if collect_issues {
            issues.extend(dlog_issues);
        }
        decisions_by_file.insert(
            dlog.file.as_str().to_string(),
            dlog.decisions
                .iter()
                .map(|(anchor, _)| anchor.clone())
                .collect(),
        );
    }

    lint_complex_undecided(
        project_root,
        &decisions_by_file,
        collect_issues,
        &mut issues,
        &mut issue_count,
    )?;
    Ok(LintProjectOutput {
        issues,
        issue_count,
    })
}

pub fn list_lint_source_files(project_root: &Path) -> Result<Vec<PathBuf>> {
    let matcher = load_gitignore_matcher(project_root)?;
    let root = project_root.canonicalize().map_err(|source| {
        ArchivaError::io(
            Some(project_root.to_path_buf()),
            "resolve project root",
            source,
        )
    })?;

    list_files(&root, |path| {
        if !is_source_file_path(path) {
            return false;
        }
        let Ok(relative) = path.strip_prefix(&root) else {
            return false;
        };
        let Some(relative) = path_to_forward_slashes(relative) else {
            return false;
        };
        !matcher.is_ignored(&relative)
    })
}

pub fn write_decision(
    project_root: &Path,
    input: &WriteDecisionInput,
) -> Result<crate::core::dlog::DecisionRecord> {
    let decision_timestamp = now_utc_millis()
        .map_err(|source| ArchivaError::cli(format!("Failed to read system time: {source}")))?;
    let env_session = env::var("ARCHIVA_SESSION").ok();
    write_decision_with_context(
        project_root,
        input,
        &decision_timestamp,
        env_session.as_deref(),
        &decision_timestamp,
    )
}

pub fn write_decision_with_context(
    project_root: &Path,
    input: &WriteDecisionInput,
    decision_timestamp: &str,
    env_session: Option<&str>,
    lock_timestamp: &str,
) -> Result<crate::core::dlog::DecisionRecord> {
    let source = read_source_text(project_root, &input.file)?;
    assert_anchor_exists(&input.file, &source, &input.anchor)?;
    write_decision_record_locked(
        project_root,
        input,
        &source,
        decision_timestamp,
        env_session,
        lock_timestamp,
    )
}

pub fn post_tool_use(project_root: &Path, file: &RelativePath) -> Result<String> {
    let git_renamed_from = git_renamed_from(project_root, file).unwrap_or(None);
    let has_current_dlog = path_exists(&dlog_path(project_root, file))?;
    let mut new_content = None::<String>;
    let mut extraction = None::<AnchorExtraction>;
    let moved_from = if has_current_dlog {
        None
    } else {
        let lock_timestamp = now_utc_millis()
            .map_err(|source| ArchivaError::cli(format!("Failed to read system time: {source}")))?;
        let git_source = if let Some(old_file) = git_renamed_from.as_ref() {
            path_exists(&dlog_path(project_root, old_file))?.then_some(old_file)
        } else {
            None
        };
        if let Some(old_file) = git_source {
            move_dlog_and_dmap_locked(
                project_root,
                old_file,
                file,
                "post-tool-use",
                &lock_timestamp,
            )?;
            Some(old_file.clone())
        } else {
            let Some(content) = read_source_text_if_exists(project_root, file)? else {
                return Ok(format!(
                    "No decisions for {}; nothing to re-anchor.",
                    file.as_str()
                ));
            };
            let anchors = extract_anchors(file, &content);
            let candidate = moved_dlog_candidate(project_root, file, &content, &anchors)?;
            new_content = Some(content);
            extraction = Some(anchors);
            if let Some(old_file) = candidate {
                move_dlog_and_dmap_locked(
                    project_root,
                    &old_file,
                    file,
                    "post-tool-use",
                    &lock_timestamp,
                )?;
                Some(old_file)
            } else {
                None
            }
        }
    };

    if !path_exists(&dlog_path(project_root, file))? {
        return Ok(format!(
            "No decisions for {}; nothing to re-anchor.",
            file.as_str()
        ));
    };

    let new_content = match new_content {
        Some(content) => content,
        None => read_source_text(project_root, file)?,
    };
    let extraction = extraction.unwrap_or_else(|| extract_anchors(file, &new_content));
    let old_git_file = git_renamed_from
        .as_ref()
        .or(moved_from.as_ref())
        .unwrap_or(file);
    let old_content =
        read_git_head_file(project_root, old_git_file).unwrap_or_else(|_| new_content.clone());
    let line_changes = diff_lines(&old_content, &new_content);

    let lock_timestamp = now_utc_millis()
        .map_err(|source| ArchivaError::cli(format!("Failed to read system time: {source}")))?;
    let Some((stale, orphan)) =
        with_decision_file_lock(project_root, file, "post-tool-use", &lock_timestamp, || {
            let Some(mut dlog) = load_dlog(project_root, file)? else {
                return Ok(None);
            };

            let mut stale = 0_usize;
            let mut orphan = 0_usize;
            for (anchor, decision) in dlog.decisions.iter_mut() {
                decision.lines_hint =
                    apply_line_changes_to_range(&line_changes, decision.lines_hint.clone());

                if extraction.anchors.get_str(anchor).is_none() {
                    if extraction.complete {
                        mark_orphan(decision);
                        orphan += 1;
                    }
                    continue;
                }

                if !extraction.complete {
                    continue;
                }

                if is_fingerprint_stale(&new_content, decision) {
                    mark_stale_now(decision).map_err(|source| {
                        ArchivaError::cli(format!("Failed to read system time: {source}"))
                    })?;
                    stale += 1;
                } else {
                    clear_recovered_status(decision);
                }
            }

            write_dlog(project_root, &dlog)?;
            write_dmap(project_root, &dlog)?;
            Ok(Some((stale, orphan)))
        })?
    else {
        return Ok(format!(
            "No decisions for {}; nothing to re-anchor.",
            file.as_str()
        ));
    };
    Ok(format!(
        "Re-anchored {}: {} stale, {} orphan.",
        file.as_str(),
        stale,
        orphan
    ))
}

fn moved_dlog_candidate(
    project_root: &Path,
    new_file: &RelativePath,
    new_content: &str,
    extraction: &AnchorExtraction,
) -> Result<Option<RelativePath>> {
    if !extraction.complete {
        return Ok(None);
    }

    let mut candidates = Vec::new();
    for dlog_file in list_dlog_files(project_root)? {
        let old_file = decision_file_to_source(project_root, &dlog_file)?;
        if old_file == *new_file || read_source_text_if_exists(project_root, &old_file)?.is_some() {
            continue;
        }
        let Some(dlog) = load_dlog(project_root, &old_file)? else {
            continue;
        };
        if moved_dlog_matches_new_source(&dlog, new_content, extraction) {
            candidates.push(old_file);
        }
    }

    if candidates.len() == 1 {
        Ok(candidates.pop())
    } else {
        Ok(None)
    }
}

fn moved_dlog_matches_new_source(
    dlog: &DlogFile,
    new_content: &str,
    extraction: &AnchorExtraction,
) -> bool {
    if dlog.decisions.is_empty() {
        return false;
    }

    let mut fingerprint_match = false;
    for (anchor, decision) in dlog.decisions.iter() {
        let Some(info) = extraction.anchors.get_str(anchor) else {
            return false;
        };
        let anchor_source = get_lines(new_content, info.start as usize, info.end as usize);
        if fingerprint(&anchor_source) == decision.fingerprint {
            fingerprint_match = true;
        }
    }
    fingerprint_match
}

fn load_project_status_summaries(project_root: &Path) -> Result<Vec<StatusFileSummary>> {
    let dlog_files = list_dlog_files(project_root)?;
    let mut summaries = Vec::new();
    for dlog_file in dlog_files {
        let file = decision_file_to_source(project_root, &dlog_file)?;
        if let Some(dlog) = load_dlog(project_root, &file)? {
            ensure_dmap_current(project_root, &dlog, "status")?;
            summaries.push(status_summary_from_dlog(&dlog));
        }
    }
    Ok(summaries)
}

fn lint_dlog_locked(
    project_root: &Path,
    file: &RelativePath,
    fix: bool,
) -> Result<Option<(DlogFile, Vec<LintIssue>)>> {
    let lock_timestamp = now_utc_millis()
        .map_err(|source| ArchivaError::cli(format!("Failed to read system time: {source}")))?;
    let command = if fix { "lint --fix" } else { "lint" };
    with_decision_file_lock(project_root, file, command, &lock_timestamp, || {
        let Some(mut dlog) = load_dlog(project_root, file)? else {
            return Ok(None);
        };
        let issues = lint_dlog(project_root, &mut dlog, fix)?;
        ensure_dmap_current_locked(project_root, &dlog)?;
        Ok(Some((dlog, issues)))
    })
}

fn parser_lint_issue(file: &RelativePath, extraction: &AnchorExtraction) -> LintIssue {
    LintIssue {
        rule: LintRule::Parser,
        severity: LintSeverity::Error,
        file: file.clone(),
        anchor: "parser".to_string(),
        message: parser_lint_message(extraction),
        fixable: false,
    }
}

fn parser_lint_message(extraction: &AnchorExtraction) -> String {
    let Some(first) = extraction.diagnostics.first() else {
        return "parser could not complete anchor extraction".to_string();
    };
    let extra = extraction.diagnostics.len().saturating_sub(1);
    if extra == 0 {
        format!(
            "parser could not complete anchor extraction at line {}, column {}: {}",
            first.line, first.column, first.message
        )
    } else {
        format!(
            "parser could not complete anchor extraction at line {}, column {}: {} ({} more diagnostics)",
            first.line, first.column, first.message, extra
        )
    }
}

fn lint_dlog(project_root: &Path, dlog: &mut DlogFile, fix: bool) -> Result<Vec<LintIssue>> {
    let source = read_source_text_if_exists(project_root, &dlog.file)?;
    let source_exists = source.is_some();
    let source = source.unwrap_or_default();
    let extraction = if source_exists {
        Some(extract_anchors(&dlog.file, &source))
    } else {
        None
    };
    let extraction_complete = extraction
        .as_ref()
        .is_none_or(|extraction| extraction.complete);

    let mut issues = Vec::new();
    let mut changed = false;
    let mut remove_anchors = Vec::<String>::new();
    if let Some(extraction) = extraction
        .as_ref()
        .filter(|extraction| !extraction.complete)
    {
        issues.push(parser_lint_issue(&dlog.file, extraction));
    }

    for (anchor, decision) in dlog.decisions.iter_mut() {
        let anchor_exists = extraction
            .as_ref()
            .is_some_and(|extraction| extraction.anchors.get_str(anchor).is_some());
        if !anchor_exists {
            if !source_exists || extraction_complete {
                issues.push(LintIssue {
                    rule: LintRule::Orphan,
                    severity: LintSeverity::Warning,
                    file: dlog.file.clone(),
                    anchor: anchor.clone(),
                    message: format!("{} no longer exists in {}", anchor, dlog.file.as_str()),
                    fixable: true,
                });
                if fix {
                    remove_anchors.push(anchor.clone());
                    changed = true;
                }
            }
            continue;
        }

        if !extraction_complete {
            continue;
        }

        let fingerprint_mismatch = source_exists && is_fingerprint_stale(&source, decision);
        let was_already_stale = decision.status == Some(crate::core::dmap::DecisionStatus::Stale);

        if fingerprint_mismatch {
            issues.push(LintIssue {
                rule: LintRule::Stale,
                severity: LintSeverity::Error,
                file: dlog.file.clone(),
                anchor: anchor.clone(),
                message: format!("{} code fingerprint differs from recorded decision", anchor),
                fixable: false,
            });
            if !was_already_stale {
                mark_stale_now(decision).map_err(|source| {
                    ArchivaError::cli(format!("Failed to read system time: {source}"))
                })?;
                changed = true;
            } else {
                issues.push(LintIssue {
                    rule: LintRule::Supersede,
                    severity: LintSeverity::Error,
                    file: dlog.file.clone(),
                    anchor: anchor.clone(),
                    message: format!("{} is stale and has not been superseded", anchor),
                    fixable: false,
                });
            }
        } else if clear_recovered_status(decision) {
            changed = true;
        }
    }

    for anchor in remove_anchors {
        dlog.decisions.remove_str(&anchor);
    }

    if changed {
        write_dlog(project_root, dlog)?;
        write_dmap(project_root, dlog)?;
    }

    Ok(issues)
}

fn lint_complex_undecided(
    project_root: &Path,
    decisions_by_file: &HashMap<String, HashSet<String>>,
    collect_issues: bool,
    issues: &mut Vec<LintIssue>,
    issue_count: &mut usize,
) -> Result<()> {
    for absolute_file in list_lint_source_files(project_root)? {
        let file = project_relative_path(project_root, &absolute_file)?;
        let decided = decisions_by_file.get(file.as_str());
        *issue_count +=
            lint_complex_undecided_file(project_root, &file, decided, collect_issues, issues)?
                .len();
    }

    Ok(())
}

fn lint_complex_undecided_file(
    project_root: &Path,
    file: &RelativePath,
    decided: Option<&HashSet<String>>,
    collect_issues: bool,
    issues: &mut Vec<LintIssue>,
) -> Result<Vec<LintIssue>> {
    let Some(content) = read_source_text_if_exists(project_root, file)? else {
        return Ok(Vec::new());
    };
    let anchors = extract_anchors(file, &content);
    let mut file_issues = Vec::new();
    if !anchors.complete {
        if decided.is_none() {
            file_issues.push(parser_lint_issue(file, &anchors));
        }
    } else {
        for (anchor, info) in anchors.anchors.iter() {
            if matches!(
                info.kind,
                AnchorKind::Class
                    | AnchorKind::Struct
                    | AnchorKind::Enum
                    | AnchorKind::Trait
                    | AnchorKind::Module
                    | AnchorKind::Impl
                    | AnchorKind::Export
                    | AnchorKind::Block
            ) {
                continue;
            }
            if info.complexity >= 5 && !decided.is_some_and(|decided| decided.contains(anchor)) {
                file_issues.push(LintIssue {
                    rule: LintRule::Undecided,
                    severity: LintSeverity::Warning,
                    file: file.clone(),
                    anchor: anchor.clone(),
                    message: format!(
                        "{} has complexity {} and no decision",
                        anchor, info.complexity
                    ),
                    fixable: false,
                });
            }
        }
    }
    if collect_issues {
        issues.extend(file_issues.iter().cloned());
    }
    Ok(file_issues)
}

fn read_source_text(project_root: &Path, file: &RelativePath) -> Result<String> {
    let source_path = canonical_source_path_if_exists(project_root, file)?;
    read_text_file_with_limit(&source_path, SOURCE_FILE_MAX_BYTES, "read source file")
}

fn read_source_text_if_exists(project_root: &Path, file: &RelativePath) -> Result<Option<String>> {
    let source_path = canonical_source_path_if_exists(project_root, file)?;
    read_text_if_exists_with_limit(&source_path, SOURCE_FILE_MAX_BYTES, "read source file")
}

fn load_gitignore_matcher(project_root: &Path) -> Result<GitignoreMatcher> {
    let content = read_text_if_exists(&project_root.join(".gitignore"))?;
    Ok(GitignoreMatcher::from_gitignore(
        content.as_deref().unwrap_or(""),
    ))
}

fn is_source_file_path(path: &Path) -> bool {
    if path
        .components()
        .any(|component| component.as_os_str() == ".decisions")
    {
        return false;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| SOURCE_EXTENSIONS.contains(&extension))
}

fn project_relative_path(project_root: &Path, path: &Path) -> Result<RelativePath> {
    let root = project_root.canonicalize().map_err(|source| {
        ArchivaError::io(
            Some(project_root.to_path_buf()),
            "resolve project root",
            source,
        )
    })?;
    let relative = path.strip_prefix(&root).map_err(|_| {
        ArchivaError::cli(format!(
            "Source file {} is not under {}",
            path.display(),
            root.display()
        ))
    })?;
    let relative = path_to_forward_slashes(relative).ok_or_else(|| {
        ArchivaError::cli(format!(
            "Source file path {} is not valid UTF-8",
            path.display()
        ))
    })?;
    Ok(RelativePath::new(&relative)?)
}

fn path_to_forward_slashes(path: &Path) -> Option<String> {
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => segments.push(segment.to_str()?.to_string()),
            _ => return None,
        }
    }
    Some(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{
        decision_file_to_source, history, lint_project, list_dlog_files, list_lint_source_files,
        post_tool_use, session_start, status, why, why_for_line, write_decision_with_context,
    };
    use crate::core::dlog::{DecisionRecord, DlogFile, LineRange, RejectedAlternative};
    use crate::core::dmap::DecisionStatus;
    use crate::core::fingerprint::{fingerprint, get_lines};
    use crate::core::ordered_map::OrderedMap;
    use crate::core::paths::{decision_lock_path, dlog_path, dmap_path, RelativePath};
    use crate::core::storage::{load_dlog, write_dlog_and_dmap_locked};
    use crate::core::version::DLOG_SCHEMA_VERSION;
    use crate::core::{decision::WriteDecisionInput, dlog::RejectedAlternative as Rejected};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn reads_why_why_for_line_and_history_from_project_storage() {
        let root = unique_temp_dir("archiva-project-read");
        let dlog = fixture_dlog("src/explain.ts");
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();
        let file = RelativePath::new("src/explain.ts").unwrap();

        assert_eq!(
            why(&root, &file, Some("fn:first")).unwrap(),
            "fn:first dec_001 (lines 1-3) [STALE]\nChose: first approach with extra whitespace\nBecause: first reason\nRejected:\n  - class wrapper -> adds no behavior\n  - global helper -> hides coupling\n  - third hidden -> not shown in session map\nRecorded: 2026-06-26T20:31:18.340Z  Session: sess_a\nExpires if: api changes"
        );
        assert_eq!(
            why_for_line(&root, &file, 6).unwrap(),
            "fn:second dec_002 (lines 5-8)\nChose: second approach\nwith newlines and      spaces\nBecause: second reason\nRecorded: 2026-06-26T20:32:18.340Z"
        );
        assert_eq!(
            history(&root, &file, "fn:first").unwrap(),
            "dec_000 2026-06-25T10:00:00.000Z\n  Chose: older approach\n  Because: older reason\n\ndec_001 2026-06-26T20:31:18.340Z\n  Chose: first approach with extra whitespace\n  Because: first reason"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reports_missing_project_decisions_like_typescript_contract() {
        let root = unique_temp_dir("archiva-project-missing");
        let file = RelativePath::new("src/missing.ts").unwrap();

        assert_eq!(
            why(&root, &file, None).unwrap(),
            "No decisions found for src/missing.ts."
        );
        assert_eq!(
            why_for_line(&root, &file, 7).unwrap(),
            "No decisions found for src/missing.ts."
        );
        assert_eq!(
            history(&root, &file, "fn:missing").unwrap(),
            "No decision found for src/missing.ts at fn:missing."
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lists_dlog_files_and_converts_decision_file_paths_to_sources() {
        let root = unique_temp_dir("archiva-project-list");
        let dlog = fixture_dlog("src/nested/a.ts");
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();
        fs::write(root.join(".decisions").join("ignored.dmap"), "").unwrap();

        let files = list_dlog_files(&root).unwrap();
        assert_eq!(files, vec![dlog_path(&root, &dlog.file)]);
        assert_eq!(
            decision_file_to_source(&root, &files[0]).unwrap().as_str(),
            "src/nested/a.ts"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discovers_decision_logs_under_generated_source_directory_names() {
        let root = unique_temp_dir("archiva-project-list-generated-names");
        let source = "function kept() {\n  return 1;\n}\n";
        let source_path = root.join("src").join("build").join("a.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, source).unwrap();
        let dlog = single_decision_dlog("src/build/a.ts", "fn:kept", source);
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        assert_eq!(
            list_dlog_files(&root).unwrap(),
            vec![dlog_path(&root, &dlog.file)]
        );
        assert!(session_start(&root).unwrap().contains("src/build/a.ts"));
        assert!(status(&root).unwrap().contains("src/build/a.ts"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn formats_session_start_from_project_dlogs_and_preserves_discovered_count() {
        let root = unique_temp_dir("archiva-project-session");
        let dlog = fixture_dlog("src/explain.ts");
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        assert_eq!(
            session_start(&root).unwrap(),
            "[Archiva] Decision map loaded for 1 files:\n\nsrc/explain.ts\n  1-3 fn:first STALE -> first approach with extra whitespace | x class wrapper(adds no behavior), global helper(hides coupling)\n  5-8 fn:second -> second approach with newlines and spaces"
        );
        assert_eq!(
            session_start(&unique_temp_dir("archiva-project-empty")).unwrap(),
            "[Archiva] No decision map found."
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn formats_session_start_in_sorted_path_order() {
        let root = unique_temp_dir("archiva-project-session-sorted");
        let zeta = single_decision_dlog(
            "src/zeta.ts",
            "fn:zeta",
            "function zeta() {\n  return 1;\n}\n",
        );
        let alpha = single_decision_dlog(
            "src/alpha.ts",
            "fn:alpha",
            "function alpha() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &zeta, "test", "2026-06-26T20:31:18.340Z").unwrap();
        write_dlog_and_dmap_locked(&root, &alpha, "test", "2026-06-26T20:31:18.340Z").unwrap();

        assert_eq!(
            session_start(&root).unwrap(),
            "[Archiva] Decision map loaded for 2 files:\n\nsrc/alpha.ts\n  1-3 fn:alpha -> record behavior\n\nsrc/zeta.ts\n  1-3 fn:zeta -> record behavior"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_start_displays_path_derived_file_like_typescript() {
        let root = unique_temp_dir("archiva-project-session-path-derived");
        let dlog_path = root
            .join(".decisions")
            .join("src")
            .join("path-derived.ts.dlog");
        fs::create_dir_all(dlog_path.parent().unwrap()).unwrap();
        fs::write(
            &dlog_path,
            "file: src/declared.ts\nschema: 1\ndecisions:\n  fn:mismatch:\n    id: dec_001\n    lines_hint:\n      - 1\n      - 3\n    fingerprint: deadbeef\n    chose: path-derived display\n    because: fixture\n    rejected: []\n    timestamp: '2026-06-26T20:31:18.340Z'\n    history: []\n",
        )
        .unwrap();

        assert_eq!(
            session_start(&root).unwrap(),
            "[Archiva] Decision map loaded for 1 files:\n\nsrc/path-derived.ts\n  1-3 fn:mismatch -> path-derived display"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_decision_reads_source_validates_anchor_and_persists() {
        let root = unique_temp_dir("archiva-project-write-decision");
        let source_path = root.join("src").join("session.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "export function processCheckout() {\n  return \"ok\";\n}\n",
        )
        .unwrap();
        let input = write_input("src/session.ts", "fn:processCheckout", None);

        let decision = write_decision_with_context(
            &root,
            &input,
            "2026-06-26T20:31:18.340Z",
            Some("env_session_contract"),
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap();

        assert_eq!(decision.id, "dec_001");
        assert_eq!(decision.session.as_deref(), Some("env_session_contract"));
        assert_eq!(
            decision.fingerprint,
            fingerprint(&get_lines(
                "export function processCheckout() {\n  return \"ok\";\n}\n",
                1,
                3
            ))
        );
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &input.file)).unwrap(),
            "1-3:fn:processCheckout\n"
        );
        assert!(load_dlog(&root, &input.file)
            .unwrap()
            .unwrap()
            .decisions
            .get_str("fn:processCheckout")
            .is_some());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_decision_rejects_missing_anchor_before_writing() {
        let root = unique_temp_dir("archiva-project-write-missing-anchor");
        let source_path = root.join("src").join("session.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "export function processCheckout() {\n  return \"ok\";\n}\n",
        )
        .unwrap();
        let input = write_input("src/session.ts", "fn:doesNotExist", None);

        let error = write_decision_with_context(
            &root,
            &input,
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap_err()
        .user_message();

        assert_eq!(
            error,
            "Anchor \"fn:doesNotExist\" does not exist in src/session.ts. A decision recorded against a missing anchor is an immediate orphan. Available anchors in src/session.ts: export:processCheckout, fn:processCheckout."
        );
        assert!(!dlog_path(&root, &input.file).exists());
        assert!(!dmap_path(&root, &input.file).exists());
        assert!(!decision_lock_path(&root, &input.file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_decision_reports_missing_source_before_writing() {
        let root = unique_temp_dir("archiva-project-write-missing-source");
        fs::create_dir_all(&root).unwrap();
        let input = write_input("src/missing.ts", "fn:missing", None);

        let error = write_decision_with_context(
            &root,
            &input,
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap_err()
        .user_message();

        assert!(error.contains("Failed to read source file"));
        assert!(!dlog_path(&root, &input.file).exists());
        assert!(!dmap_path(&root, &input.file).exists());
        assert!(!decision_lock_path(&root, &input.file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_write_decision_attempts_leave_valid_storage() {
        let root = unique_temp_dir("archiva-project-write-concurrent");
        let source_path = root.join("src").join("concurrent.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        let functions = 8_usize;
        let source = (0..functions)
            .map(|index| format!("export function fn{index}() {{\n  return {index};\n}}\n"))
            .collect::<String>();
        fs::write(&source_path, source).unwrap();

        let root = Arc::new(root);
        let barrier = Arc::new(Barrier::new(functions));
        let handles = (0..functions)
            .map(|index| {
                let root = Arc::clone(&root);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let file = RelativePath::new("src/concurrent.ts").unwrap();
                    let input = WriteDecisionInput {
                        file,
                        anchor: format!("fn:fn{index}"),
                        lines: LineRange {
                            start: (index as u32 * 3) + 1,
                            end: (index as u32 * 3) + 3,
                        },
                        chose: format!("record function {index}"),
                        because: "concurrent writer fixture".to_string(),
                        rejected: Vec::new(),
                        expires_if: None,
                        supersedes: None,
                        session: None,
                    };
                    barrier.wait();
                    write_decision_with_context(
                        &root,
                        &input,
                        &format!("2026-06-26T20:31:{index:02}.340Z"),
                        None,
                        &format!("2026-06-26T20:31:{index:02}.341Z"),
                    )
                    .map(|decision| (input.anchor, decision.id))
                    .map_err(|error| error.user_message())
                })
            })
            .collect::<Vec<_>>();

        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        let successes = results
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .collect::<Vec<_>>();
        assert!(
            !successes.is_empty(),
            "at least one concurrent writer should acquire the lock"
        );
        for error in results.iter().filter_map(|result| result.as_ref().err()) {
            assert!(
                error.contains("Archiva lock already exists"),
                "unexpected concurrent writer error: {error}"
            );
        }

        let file = RelativePath::new("src/concurrent.ts").unwrap();
        let stored = load_dlog(&root, &file).unwrap().unwrap();
        assert_eq!(stored.decisions.len(), successes.len());
        let mut ids = successes
            .iter()
            .map(|(_, id)| id.as_str())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), successes.len());
        for (anchor, _) in successes {
            assert!(
                stored.decisions.get_str(anchor).is_some(),
                "missing successful anchor {anchor}"
            );
        }
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &file))
                .unwrap()
                .lines()
                .count(),
            stored.decisions.len()
        );
        assert!(!decision_lock_path(&root, &file).exists());
        assert!(temp_siblings(dlog_path(&root, &file).parent().unwrap()).is_empty());

        let _ = fs::remove_dir_all(root.as_ref());
    }

    #[test]
    fn lint_project_marks_stale_and_reports_supersede_on_next_scan() {
        let root = unique_temp_dir("archiva-project-lint-stale");
        let source_path = root.join("src").join("stale.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function makeThing() {\n  return 2;\n}\n").unwrap();
        let dlog = single_decision_dlog(
            "src/stale.ts",
            "fn:makeThing",
            "function makeThing() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        let issues = lint_project(&root, false).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule.as_str(), "arc/stale");
        assert_eq!(issues[0].severity.as_str(), "error");
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        let decision = stored.decisions.get_str("fn:makeThing").unwrap();
        assert_eq!(decision.status, Some(DecisionStatus::Stale));
        assert!(decision.stale_since.is_some());
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            "1-3:fn:makeThing:STALE\n"
        );

        let second = lint_project(&root, false).unwrap();
        assert_eq!(
            second
                .iter()
                .map(|issue| issue.rule.as_str())
                .collect::<Vec<_>>(),
            vec!["arc/stale", "arc/supersede"]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lint_project_refuses_existing_lock_without_rewriting_decision_files() {
        let root = unique_temp_dir("archiva-project-lint-existing-lock");
        let source_path = root.join("src").join("stale.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function makeThing() {\n  return 2;\n}\n").unwrap();
        let dlog = single_decision_dlog(
            "src/stale.ts",
            "fn:makeThing",
            "function makeThing() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();
        let lock_path = decision_lock_path(&root, &dlog.file);
        fs::write(&lock_path, "pid=999\ncommand=other\ntimestamp=old\n").unwrap();

        let error = lint_project(&root, false).unwrap_err().user_message();

        assert!(error.contains("Archiva lock already exists"));
        assert_eq!(
            fs::read_to_string(&lock_path).unwrap(),
            "pid=999\ncommand=other\ntimestamp=old\n"
        );
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        let decision = stored.decisions.get_str("fn:makeThing").unwrap();
        assert_eq!(decision.status, None);
        assert_eq!(decision.stale_since, None);
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            "1-3:fn:makeThing\n"
        );

        let _ = fs::remove_file(lock_path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lint_project_recovers_expired_lock_before_rewriting_decision_files() {
        let root = unique_temp_dir("archiva-project-lint-expired-lock");
        let source_path = root.join("src").join("stale.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function makeThing() {\n  return 2;\n}\n").unwrap();
        let dlog = single_decision_dlog(
            "src/stale.ts",
            "fn:makeThing",
            "function makeThing() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();
        let lock_path = decision_lock_path(&root, &dlog.file);
        fs::write(
            &lock_path,
            "version=1\npid=999\ntoken=stale\ncommand=other\ntimestamp=1970-01-01T00:00:00.000Z\n",
        )
        .unwrap();

        let issues = lint_project(&root, false).unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule.as_str(), "arc/stale");
        assert!(!lock_path.exists());
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        let decision = stored.decisions.get_str("fn:makeThing").unwrap();
        assert_eq!(decision.status, Some(DecisionStatus::Stale));
        assert!(decision.stale_since.is_some());
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            "1-3:fn:makeThing:STALE\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lint_project_fix_deletes_orphan_decisions() {
        let root = unique_temp_dir("archiva-project-lint-orphan");
        let source_path = root.join("src").join("orphan.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function kept() {\n  return 1;\n}\n").unwrap();
        let dlog = single_decision_dlog(
            "src/orphan.ts",
            "fn:gone",
            "function gone() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        let issues = lint_project(&root, true).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule.as_str(), "arc/orphan");
        assert!(issues[0].fixable);
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        assert!(stored.decisions.is_empty());
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            ""
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lint_project_parser_incomplete_blocks_orphan_fix() {
        let root = unique_temp_dir("archiva-project-lint-parser-incomplete");
        let source_path = root.join("src").join("parser.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "function kept() {\n  return 1;\n}\nfunction broken() {\n",
        )
        .unwrap();
        let dlog = single_decision_dlog(
            "src/parser.ts",
            "fn:gone",
            "function gone() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        let issues = lint_project(&root, true).unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].rule.as_str(), "arc/parser");
        assert!(!issues[0].fixable);
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        assert!(stored.decisions.get_str("fn:gone").is_some());
        assert_eq!(stored.decisions.get_str("fn:gone").unwrap().status, None);
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            "1-3:fn:gone\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn lint_and_post_tool_use_reject_symlinked_sources_outside_project() {
        use std::os::unix::fs::symlink;

        let root = unique_temp_dir("archiva-project-source-symlink-root");
        let outside = unique_temp_dir("archiva-project-source-symlink-outside");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            outside.join("linked.ts"),
            "function linked() {\n  return 1;\n}\n",
        )
        .unwrap();
        symlink(
            outside.join("linked.ts"),
            root.join("src").join("linked.ts"),
        )
        .unwrap();
        let dlog = single_decision_dlog(
            "src/linked.ts",
            "fn:linked",
            "function linked() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        let lint_error = lint_project(&root, false).unwrap_err().user_message();
        assert!(lint_error.contains("path resolves outside the project root"));

        let post_error = post_tool_use(&root, &dlog.file).unwrap_err().user_message();
        assert!(post_error.contains("path resolves outside the project root"));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn lint_project_reports_complex_undecided_and_respects_gitignore() {
        let root = unique_temp_dir("archiva-project-lint-undecided");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".decisions").join("src")).unwrap();
        fs::write(root.join(".gitignore"), "*.test.ts\n").unwrap();
        fs::write(
            root.join(".decisions").join("src").join("hidden.ts"),
            "function hidden() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src").join("ignored.test.ts"),
            "function ignored() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("src").join("complex.ts"),
            "function complex(a, b, c) {\n  if (a) { return 1; }\n  if (b) { return 2; }\n  while (c) { break; }\n  return a && b ? 3 : 4;\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src").join("complex.rs"),
            "fn complex_rs(a: bool, b: bool, c: bool) -> i32 {\n  if a { return 1; }\n  if b { return 2; }\n  while c { break; }\n  if a && b { 3 } else { 4 }\n}\n",
        )
        .unwrap();

        let lint_files = list_lint_source_files(&root).unwrap();
        assert_eq!(
            lint_files
                .iter()
                .map(|path| path
                    .strip_prefix(root.canonicalize().unwrap())
                    .unwrap()
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"))
                .collect::<Vec<_>>(),
            vec!["src/complex.rs".to_string(), "src/complex.ts".to_string()]
        );

        let issues = lint_project(&root, false).unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].rule.as_str(), "arc/undecided");
        assert_eq!(issues[0].file.as_str(), "src/complex.rs");
        assert_eq!(issues[0].anchor, "fn:complex_rs");
        assert!(issues[0].message.contains("complexity"));
        assert_eq!(issues[1].rule.as_str(), "arc/undecided");
        assert_eq!(issues[1].file.as_str(), "src/complex.ts");
        assert_eq!(issues[1].anchor, "fn:complex");
        assert!(issues[1].message.contains("complexity"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_counts_lint_issues_after_loading_decision_totals() {
        let root = unique_temp_dir("archiva-project-status");
        let source_path = root.join("src").join("status.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function makeThing() {\n  return 2;\n}\n").unwrap();
        let dlog = single_decision_dlog(
            "src/status.ts",
            "fn:makeThing",
            "function makeThing() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        assert_eq!(
            status(&root).unwrap(),
            "src/status.ts                    1 decisions  0 stale  0 orphan\n\nTotal: 1 decisions  0 stale  0 orphan  1 issues"
        );
        assert_eq!(
            load_dlog(&root, &dlog.file)
                .unwrap()
                .unwrap()
                .decisions
                .get_str("fn:makeThing")
                .unwrap()
                .status,
            Some(DecisionStatus::Stale)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_counts_parser_issue_without_orphan_side_effect() {
        let root = unique_temp_dir("archiva-project-status-parser-incomplete");
        let source_path = root.join("src").join("parser.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "function kept() {\n  return 1;\n}\nfunction broken() {\n",
        )
        .unwrap();
        let dlog = single_decision_dlog(
            "src/parser.ts",
            "fn:gone",
            "function gone() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        assert_eq!(
            status(&root).unwrap(),
            "src/parser.ts                    1 decisions  0 stale  0 orphan\n\nTotal: 1 decisions  0 stale  0 orphan  1 issues"
        );
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        assert_eq!(stored.decisions.get_str("fn:gone").unwrap().status, None);
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            "1-3:fn:gone\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_repairs_stale_dmap_from_dlog_before_reporting() {
        let root = unique_temp_dir("archiva-project-status-dmap-repair");
        let source_path = root.join("src").join("status.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function makeThing() {\n  return 1;\n}\n").unwrap();
        let dlog = single_decision_dlog(
            "src/status.ts",
            "fn:makeThing",
            "function makeThing() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();
        fs::write(dmap_path(&root, &dlog.file), "99-100:fn:old\n").unwrap();

        assert_eq!(
            status(&root).unwrap(),
            "src/status.ts                    1 decisions  0 stale  0 orphan\n\nTotal: 1 decisions  0 stale  0 orphan  0 issues"
        );
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            "1-3:fn:makeThing\n"
        );
        assert!(!decision_lock_path(&root, &dlog.file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_reports_pre_lint_stale_count_before_recovery_side_effect() {
        let root = unique_temp_dir("archiva-project-status-recovered");
        let source = "function makeThing() {\n  return 1;\n}\n";
        let source_path = root.join("src").join("status.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, source).unwrap();
        let mut dlog = single_decision_dlog("src/status.ts", "fn:makeThing", source);
        let decision = dlog
            .decisions
            .iter_mut()
            .find(|(anchor, _)| anchor.as_str() == "fn:makeThing")
            .map(|(_, decision)| decision)
            .unwrap();
        decision.status = Some(DecisionStatus::Stale);
        decision.stale_since = Some("2026-06-26T21:00:00.000Z".to_string());
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        assert_eq!(
            status(&root).unwrap(),
            "src/status.ts                    1 decisions  1 stale  0 orphan\n\nTotal: 1 decisions  1 stale  0 orphan  0 issues"
        );
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        let recovered = stored.decisions.get_str("fn:makeThing").unwrap();
        assert_eq!(recovered.status, None);
        assert_eq!(recovered.stale_since, None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_counts_undecided_source_issues_when_no_dlogs_exist() {
        let root = unique_temp_dir("archiva-project-status-undecided");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src").join("complex.ts"),
            "function complex(a, b, c) {\n  if (a) { return 1; }\n  if (b) { return 2; }\n  while (c) { break; }\n  return a && b ? 3 : 4;\n}\n",
        )
        .unwrap();

        assert_eq!(
            status(&root).unwrap(),
            "No decision logs found.\n\nTotal: 0 decisions  0 stale  0 orphan  1 issues"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn status_counts_undecided_rust_source_issues_when_no_dlogs_exist() {
        let root = unique_temp_dir("archiva-project-status-undecided-rust");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src").join("complex.rs"),
            "fn complex_rs(a: bool, b: bool, c: bool) -> i32 {\n  if a { return 1; }\n  if b { return 2; }\n  while c { break; }\n  if a && b { 3 } else { 4 }\n}\n",
        )
        .unwrap();

        assert_eq!(
            status(&root).unwrap(),
            "No decision logs found.\n\nTotal: 0 decisions  0 stale  0 orphan  1 issues"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_reports_missing_decisions_without_writing() {
        let root = unique_temp_dir("archiva-project-post-missing");
        fs::create_dir_all(&root).unwrap();
        let file = RelativePath::new("src/missing.ts").unwrap();

        assert_eq!(
            post_tool_use(&root, &file).unwrap(),
            "No decisions for src/missing.ts; nothing to re-anchor."
        );
        assert!(!dlog_path(&root, &file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_parser_incomplete_blocks_orphan_marking() {
        let root = unique_temp_dir("archiva-project-post-parser-incomplete");
        let source_path = root.join("src").join("parser.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "function kept() {\n  return 1;\n}\nfunction broken() {\n",
        )
        .unwrap();
        let dlog = single_decision_dlog(
            "src/parser.ts",
            "fn:gone",
            "function gone() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();

        assert_eq!(
            post_tool_use(&root, &dlog.file).unwrap(),
            "Re-anchored src/parser.ts: 0 stale, 0 orphan."
        );
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        assert_eq!(stored.decisions.get_str("fn:gone").unwrap().status, None);
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            "1-3:fn:gone\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_refuses_existing_lock_without_rewriting_decision_files() {
        let root = unique_temp_dir("archiva-project-post-existing-lock");
        let source_path = root.join("src").join("stale.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function makeThing() {\n  return 2;\n}\n").unwrap();
        let dlog = single_decision_dlog(
            "src/stale.ts",
            "fn:makeThing",
            "function makeThing() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();
        let lock_path = decision_lock_path(&root, &dlog.file);
        fs::write(&lock_path, "pid=999\ncommand=other\ntimestamp=old\n").unwrap();

        let error = post_tool_use(&root, &dlog.file).unwrap_err().user_message();

        assert!(error.contains("Archiva lock already exists"));
        assert_eq!(
            fs::read_to_string(&lock_path).unwrap(),
            "pid=999\ncommand=other\ntimestamp=old\n"
        );
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        let decision = stored.decisions.get_str("fn:makeThing").unwrap();
        assert_eq!(decision.status, None);
        assert_eq!(decision.stale_since, None);
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            "1-3:fn:makeThing\n"
        );

        let _ = fs::remove_file(lock_path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_recovers_expired_lock_before_reanchoring() {
        let root = unique_temp_dir("archiva-project-post-expired-lock");
        let source_path = root.join("src").join("stale.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function makeThing() {\n  return 2;\n}\n").unwrap();
        let dlog = single_decision_dlog(
            "src/stale.ts",
            "fn:makeThing",
            "function makeThing() {\n  return 1;\n}\n",
        );
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();
        let lock_path = decision_lock_path(&root, &dlog.file);
        fs::write(
            &lock_path,
            "version=1\npid=999\ntoken=stale\ncommand=other\ntimestamp=1970-01-01T00:00:00.000Z\n",
        )
        .unwrap();

        let output = post_tool_use(&root, &dlog.file).unwrap();

        assert_eq!(output, "Re-anchored src/stale.ts: 1 stale, 0 orphan.");
        assert!(!lock_path.exists());
        let stored = load_dlog(&root, &dlog.file).unwrap().unwrap();
        let decision = stored.decisions.get_str("fn:makeThing").unwrap();
        assert_eq!(decision.status, Some(DecisionStatus::Stale));
        assert!(decision.stale_since.is_some());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_shifts_lines_from_git_head_before_fingerprint_check() {
        let root = unique_temp_dir("archiva-project-post-shift");
        let source_path = root.join("src").join("shift.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function kept() {\n  return 1;\n}\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/shift.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );

        let input = write_input("src/shift.ts", "fn:kept", None);
        write_decision_with_context(
            &root,
            &input,
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap();
        fs::write(
            &source_path,
            "// inserted\nfunction kept() {\n  return 1;\n}\n",
        )
        .unwrap();
        let file = RelativePath::new("src/shift.ts").unwrap();

        assert_eq!(
            post_tool_use(&root, &file).unwrap(),
            "Re-anchored src/shift.ts: 0 stale, 0 orphan."
        );
        let stored = load_dlog(&root, &file).unwrap().unwrap();
        let decision = stored.decisions.get_str("fn:kept").unwrap();
        assert_eq!(decision.lines_hint, LineRange { start: 2, end: 4 });
        assert_eq!(decision.status, None);
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &file)).unwrap(),
            "2-4:fn:kept\n"
        );
        assert!(!decision_lock_path(&root, &file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_moves_decisions_after_git_file_rename() {
        let root = unique_temp_dir("archiva-project-post-git-rename");
        let old_source_path = root.join("src").join("old.ts");
        let new_source_path = root.join("src").join("new.ts");
        fs::create_dir_all(old_source_path.parent().unwrap()).unwrap();
        fs::write(&old_source_path, "function kept() {\n  return 1;\n}\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/old.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );

        let old_file = RelativePath::new("src/old.ts").unwrap();
        let new_file = RelativePath::new("src/new.ts").unwrap();
        let input = write_input("src/old.ts", "fn:kept", None);
        write_decision_with_context(
            &root,
            &input,
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap();
        git(&root, &["mv", "src/old.ts", "src/new.ts"]);
        fs::write(
            &new_source_path,
            "// inserted\nfunction kept() {\n  return 1;\n}\n",
        )
        .unwrap();

        assert_eq!(
            post_tool_use(&root, &new_file).unwrap(),
            "Re-anchored src/new.ts: 0 stale, 0 orphan."
        );
        assert!(load_dlog(&root, &old_file).unwrap().is_none());
        assert!(!dlog_path(&root, &old_file).exists());
        assert!(!dmap_path(&root, &old_file).exists());
        let stored = load_dlog(&root, &new_file).unwrap().unwrap();
        let decision = stored.decisions.get_str("fn:kept").unwrap();
        assert_eq!(stored.file, new_file);
        assert_eq!(decision.lines_hint, LineRange { start: 2, end: 4 });
        assert_eq!(decision.status, None);
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &stored.file)).unwrap(),
            "2-4:fn:kept\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_moves_unique_deleted_dlog_candidate_without_git() {
        let root = unique_temp_dir("archiva-project-post-move-no-git");
        let old_source_path = root.join("src").join("old.ts");
        let new_source_path = root.join("src").join("new.ts");
        fs::create_dir_all(old_source_path.parent().unwrap()).unwrap();
        let source = "function kept() {\n  return 1;\n}\n";
        fs::write(&old_source_path, source).unwrap();

        let old_file = RelativePath::new("src/old.ts").unwrap();
        let new_file = RelativePath::new("src/new.ts").unwrap();
        let input = write_input("src/old.ts", "fn:kept", None);
        write_decision_with_context(
            &root,
            &input,
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap();
        fs::remove_file(&old_source_path).unwrap();
        fs::write(&new_source_path, source).unwrap();

        assert_eq!(
            post_tool_use(&root, &new_file).unwrap(),
            "Re-anchored src/new.ts: 0 stale, 0 orphan."
        );
        assert!(load_dlog(&root, &old_file).unwrap().is_none());
        let stored = load_dlog(&root, &new_file).unwrap().unwrap();
        assert!(stored.decisions.get_str("fn:kept").is_some());
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &new_file)).unwrap(),
            "1-3:fn:kept\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_keeps_same_file_anchor_rename_as_orphan() {
        let root = unique_temp_dir("archiva-project-post-anchor-rename");
        let source_path = root.join("src").join("rename.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function kept() {\n  return 1;\n}\n").unwrap();
        let input = write_input("src/rename.ts", "fn:kept", None);
        write_decision_with_context(
            &root,
            &input,
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap();
        fs::write(&source_path, "function renamed() {\n  return 1;\n}\n").unwrap();

        assert_eq!(
            post_tool_use(&root, &input.file).unwrap(),
            "Re-anchored src/rename.ts: 0 stale, 1 orphan."
        );
        assert_eq!(
            load_dlog(&root, &input.file)
                .unwrap()
                .unwrap()
                .decisions
                .get_str("fn:kept")
                .unwrap()
                .status,
            Some(DecisionStatus::Orphan)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_shifts_multiple_decisions_from_one_precomputed_diff() {
        let root = unique_temp_dir("archiva-project-post-shift-many");
        let source_path = root.join("src").join("multi-shift.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "function first() {\n  return 1;\n}\n\nfunction second() {\n  return 2;\n}\n",
        )
        .unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "src/multi-shift.ts"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Archiva Test",
                "-c",
                "user.email=archiva@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );

        let first = write_input("src/multi-shift.ts", "fn:first", None);
        write_decision_with_context(
            &root,
            &first,
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap();
        let mut second = write_input("src/multi-shift.ts", "fn:second", None);
        second.lines = LineRange { start: 5, end: 7 };
        write_decision_with_context(
            &root,
            &second,
            "2026-06-26T20:32:18.340Z",
            None,
            "2026-06-26T20:32:18.341Z",
        )
        .unwrap();
        fs::write(
            &source_path,
            "// inserted\nfunction first() {\n  return 1;\n}\n\nfunction second() {\n  return 2;\n}\n",
        )
        .unwrap();
        let file = RelativePath::new("src/multi-shift.ts").unwrap();

        assert_eq!(
            post_tool_use(&root, &file).unwrap(),
            "Re-anchored src/multi-shift.ts: 0 stale, 0 orphan."
        );
        let stored = load_dlog(&root, &file).unwrap().unwrap();
        let first = stored.decisions.get_str("fn:first").unwrap();
        let second = stored.decisions.get_str("fn:second").unwrap();
        assert_eq!(first.lines_hint, LineRange { start: 2, end: 4 });
        assert_eq!(second.lines_hint, LineRange { start: 6, end: 8 });
        assert_eq!(first.status, None);
        assert_eq!(second.status, None);
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &file)).unwrap(),
            "2-4:fn:first\n6-8:fn:second\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_clears_orphan_when_anchor_returns() {
        let root = unique_temp_dir("archiva-project-post-orphan-return");
        let source_path = root.join("src").join("orphan-return.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        let with_both = "function kept() {\n  return 1;\n}\nfunction removed() {\n  return 2;\n}\n";
        fs::write(&source_path, with_both).unwrap();
        let input = WriteDecisionInput {
            file: RelativePath::new("src/orphan-return.ts").unwrap(),
            anchor: "fn:removed".to_string(),
            lines: LineRange { start: 4, end: 6 },
            chose: "keep removed".to_string(),
            because: "fixture".to_string(),
            rejected: Vec::new(),
            expires_if: None,
            supersedes: None,
            session: None,
        };
        write_decision_with_context(
            &root,
            &input,
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap();

        fs::write(&source_path, "function kept() {\n  return 1;\n}\n").unwrap();
        post_tool_use(&root, &input.file).unwrap();
        assert_eq!(
            load_dlog(&root, &input.file)
                .unwrap()
                .unwrap()
                .decisions
                .get_str("fn:removed")
                .unwrap()
                .status,
            Some(DecisionStatus::Orphan)
        );

        fs::write(&source_path, with_both).unwrap();
        post_tool_use(&root, &input.file).unwrap();
        assert_eq!(
            load_dlog(&root, &input.file)
                .unwrap()
                .unwrap()
                .decisions
                .get_str("fn:removed")
                .unwrap()
                .status,
            None
        );

        let _ = fs::remove_dir_all(root);
    }

    fn fixture_dlog(file: &str) -> DlogFile {
        DlogFile {
            file: RelativePath::new(file).unwrap(),
            schema: DLOG_SCHEMA_VERSION,
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
                        history: vec![crate::core::dlog::DecisionHistoryEntry {
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

    fn write_input(file: &str, anchor: &str, supersedes: Option<&str>) -> WriteDecisionInput {
        WriteDecisionInput {
            file: RelativePath::new(file).unwrap(),
            anchor: anchor.to_string(),
            lines: LineRange { start: 1, end: 3 },
            chose: match supersedes {
                Some(_) => "superseding choice".to_string(),
                None => "optimistic locking via version field".to_string(),
            },
            because: match supersedes {
                Some(_) => "superseding reason".to_string(),
                None => "checkout and inventory deduction race under concurrent carts".to_string(),
            },
            rejected: vec![Rejected {
                approach: "SELECT FOR UPDATE".to_string(),
                reason: "deadlocks on hot SKUs".to_string(),
            }],
            expires_if: None,
            supersedes: supersedes.map(str::to_string),
            session: None,
        }
    }

    fn single_decision_dlog(file: &str, anchor: &str, source_for_fingerprint: &str) -> DlogFile {
        DlogFile {
            file: RelativePath::new(file).unwrap(),
            schema: DLOG_SCHEMA_VERSION,
            decisions: OrderedMap::from_entries(vec![(
                anchor.to_string(),
                DecisionRecord {
                    id: "dec_001".to_string(),
                    lines_hint: LineRange { start: 1, end: 3 },
                    fingerprint: fingerprint(&get_lines(source_for_fingerprint, 1, 3)),
                    chose: "record behavior".to_string(),
                    because: "fixture".to_string(),
                    rejected: Vec::new(),
                    expires_if: None,
                    session: None,
                    timestamp: "2026-06-26T20:31:18.340Z".to_string(),
                    history: Vec::new(),
                    status: None,
                    stale_since: None,
                    supersedes: None,
                },
            )]),
        }
    }

    fn git(root: &PathBuf, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?}\nstdout={}\nstderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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

    fn temp_siblings(parent: &std::path::Path) -> Vec<PathBuf> {
        fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".archiva-tmp-"))
            })
            .collect()
    }
}
