use crate::core::dmap::DecisionStatus;
use crate::core::paths::RelativePath;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusFileSummary {
    pub file: RelativePath,
    pub decisions: usize,
    pub stale: usize,
    pub orphan: usize,
}

pub fn format_status_report(summaries: &[StatusFileSummary], issue_count: usize) -> String {
    let mut lines = Vec::new();
    let mut total_decisions = 0_usize;
    let mut total_stale = 0_usize;
    let mut total_orphan = 0_usize;

    for summary in summaries {
        total_decisions += summary.decisions;
        total_stale += summary.stale;
        total_orphan += summary.orphan;
        lines.push(format!(
            "{} {} decisions  {} stale  {} orphan",
            pad_end_js_ascii(summary.file.as_str(), 32),
            summary.decisions,
            summary.stale,
            summary.orphan
        ));
    }

    if lines.is_empty() {
        lines.push("No decision logs found.".to_string());
    }
    lines.push(String::new());
    lines.push(format!(
        "Total: {} decisions  {} stale  {} orphan  {} issues",
        total_decisions, total_stale, total_orphan, issue_count
    ));
    lines.join("\n")
}

pub fn status_summary_from_dlog(dlog: &crate::core::dlog::DlogFile) -> StatusFileSummary {
    let mut summary = StatusFileSummary {
        file: dlog.file.clone(),
        decisions: 0,
        stale: 0,
        orphan: 0,
    };
    for (_, decision) in dlog.decisions.iter() {
        summary.decisions += 1;
        match decision.status {
            Some(DecisionStatus::Stale) => summary.stale += 1,
            Some(DecisionStatus::Orphan) => summary.orphan += 1,
            _ => {}
        }
    }
    summary
}

fn pad_end_js_ascii(value: &str, width: usize) -> String {
    let mut output = value.to_string();
    let len = value.chars().count();
    if len < width {
        output.push_str(&" ".repeat(width - len));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{format_status_report, status_summary_from_dlog};
    use crate::core::dlog::{DecisionRecord, DlogFile, LineRange};
    use crate::core::dmap::DecisionStatus;
    use crate::core::ordered_map::OrderedMap;
    use crate::core::paths::RelativePath;

    #[test]
    fn formats_no_decision_logs_report() {
        assert_eq!(
            format_status_report(&[], 0),
            "No decision logs found.\n\nTotal: 0 decisions  0 stale  0 orphan  0 issues"
        );
    }

    #[test]
    fn formats_status_report_like_typescript_contract_before_lint_side_effects() {
        let dlog = DlogFile {
            file: RelativePath::new("src/s.ts").unwrap(),
            schema: 1,
            decisions: OrderedMap::from_entries(vec![(
                "fn:kept".to_string(),
                fixture_decision(None),
            )]),
        };

        assert_eq!(
            format_status_report(&[status_summary_from_dlog(&dlog)], 1),
            "src/s.ts                         1 decisions  0 stale  0 orphan\n\nTotal: 1 decisions  0 stale  0 orphan  1 issues"
        );
    }

    #[test]
    fn totals_stale_and_orphan_counts_across_files() {
        let first = DlogFile {
            file: RelativePath::new("src/first.ts").unwrap(),
            schema: 1,
            decisions: OrderedMap::from_entries(vec![
                (
                    "fn:a".to_string(),
                    fixture_decision(Some(DecisionStatus::Stale)),
                ),
                (
                    "fn:b".to_string(),
                    fixture_decision(Some(DecisionStatus::Orphan)),
                ),
            ]),
        };
        let second = DlogFile {
            file: RelativePath::new("src/second.ts").unwrap(),
            schema: 1,
            decisions: OrderedMap::from_entries(vec![
                (
                    "fn:c".to_string(),
                    fixture_decision(Some(DecisionStatus::Undecided)),
                ),
                ("fn:d".to_string(), fixture_decision(None)),
            ]),
        };

        assert_eq!(
            format_status_report(
                &[
                    status_summary_from_dlog(&first),
                    status_summary_from_dlog(&second)
                ],
                3
            ),
            "src/first.ts                     2 decisions  1 stale  1 orphan\nsrc/second.ts                    2 decisions  0 stale  0 orphan\n\nTotal: 4 decisions  1 stale  1 orphan  3 issues"
        );
    }

    #[test]
    fn builds_status_summary_without_retaining_full_dlog() {
        let dlog = DlogFile {
            file: RelativePath::new("src/summary.ts").unwrap(),
            schema: 1,
            decisions: OrderedMap::from_entries(vec![
                (
                    "fn:a".to_string(),
                    fixture_decision(Some(DecisionStatus::Stale)),
                ),
                (
                    "fn:b".to_string(),
                    fixture_decision(Some(DecisionStatus::Orphan)),
                ),
                ("fn:c".to_string(), fixture_decision(None)),
            ]),
        };

        let summary = status_summary_from_dlog(&dlog);
        assert_eq!(summary.file.as_str(), "src/summary.ts");
        assert_eq!(summary.decisions, 3);
        assert_eq!(summary.stale, 1);
        assert_eq!(summary.orphan, 1);
    }

    fn fixture_decision(status: Option<DecisionStatus>) -> DecisionRecord {
        DecisionRecord {
            id: "dec_001".to_string(),
            lines_hint: LineRange { start: 1, end: 3 },
            fingerprint: "deadbeef".to_string(),
            chose: "record behavior".to_string(),
            because: "fixture".to_string(),
            rejected: Vec::new(),
            expires_if: None,
            session: None,
            timestamp: "2026-06-26T20:31:18.340Z".to_string(),
            history: Vec::new(),
            status,
            stale_since: None,
            supersedes: None,
        }
    }
}
