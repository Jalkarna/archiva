use crate::core::paths::RelativePath;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LintSeverity {
    Error,
    Warning,
}

impl LintSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }

    pub fn as_output_prefix(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warning => "WARNING",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LintRule {
    Parser,
    Stale,
    Orphan,
    Undecided,
    Supersede,
    Corrupt,
}

impl LintRule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parser => "arc/parser",
            Self::Stale => "arc/stale",
            Self::Orphan => "arc/orphan",
            Self::Undecided => "arc/undecided",
            Self::Supersede => "arc/supersede",
            Self::Corrupt => "arc/corrupt",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintIssue {
    pub rule: LintRule,
    pub severity: LintSeverity,
    pub file: RelativePath,
    pub anchor: String,
    pub message: String,
    pub fixable: bool,
}

pub fn format_lint_issues(issues: &[LintIssue]) -> String {
    if issues.is_empty() {
        return "No decision issues found.".to_string();
    }

    issues
        .iter()
        .map(|issue| {
            format!(
                "{} {} {} {}: {}",
                issue.severity.as_output_prefix(),
                issue.rule.as_str(),
                issue.file.as_str(),
                issue.anchor,
                issue.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn format_ghost_check_result(file: &RelativePath, issues: &[LintIssue]) -> String {
    if issues.is_empty() {
        return format!("No issues found for {}.", file.as_str());
    }

    issues
        .iter()
        .map(|issue| {
            format!(
                "{} {}: {}",
                issue.rule.as_str(),
                issue.anchor,
                issue.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn has_error_issue(issues: &[LintIssue]) -> bool {
    issues
        .iter()
        .any(|issue| issue.severity == LintSeverity::Error)
}

#[cfg(test)]
mod tests {
    use super::{
        format_ghost_check_result, format_lint_issues, has_error_issue, LintIssue, LintRule,
        LintSeverity,
    };
    use crate::core::paths::RelativePath;

    #[test]
    fn formats_clean_lint_output_like_typescript_contract() {
        assert_eq!(format_lint_issues(&[]), "No decision issues found.");
        assert!(!has_error_issue(&[]));
    }

    #[test]
    fn formats_warning_issue_like_lint_fix_orphan_contract() {
        let issue = LintIssue {
            rule: LintRule::Orphan,
            severity: LintSeverity::Warning,
            file: RelativePath::new("src/missing.ts").unwrap(),
            anchor: "fn:gone".to_string(),
            message: "fn:gone no longer exists in src/missing.ts".to_string(),
            fixable: true,
        };

        assert_eq!(
            format_lint_issues(&[issue]),
            "WARNING arc/orphan src/missing.ts fn:gone: fn:gone no longer exists in src/missing.ts"
        );
    }

    #[test]
    fn formats_error_issues_and_reports_error_exit_condition() {
        let issues = vec![
            LintIssue {
                rule: LintRule::Stale,
                severity: LintSeverity::Error,
                file: RelativePath::new("src/drift.ts").unwrap(),
                anchor: "fn:compute".to_string(),
                message: "fn:compute code fingerprint differs from recorded decision".to_string(),
                fixable: false,
            },
            LintIssue {
                rule: LintRule::Supersede,
                severity: LintSeverity::Error,
                file: RelativePath::new("src/drift.ts").unwrap(),
                anchor: "fn:compute".to_string(),
                message: "fn:compute is stale and has not been superseded".to_string(),
                fixable: false,
            },
        ];

        assert_eq!(
            format_lint_issues(&issues),
            "ERROR arc/stale src/drift.ts fn:compute: fn:compute code fingerprint differs from recorded decision\nERROR arc/supersede src/drift.ts fn:compute: fn:compute is stale and has not been superseded"
        );
        assert!(has_error_issue(&issues));
    }

    #[test]
    fn formats_ghost_check_text_like_mcp_contract() {
        let file = RelativePath::new("src/drift.ts").unwrap();
        let issue = LintIssue {
            rule: LintRule::Stale,
            severity: LintSeverity::Error,
            file: file.clone(),
            anchor: "fn:compute".to_string(),
            message: "fn:compute code fingerprint differs from recorded decision".to_string(),
            fixable: false,
        };

        assert_eq!(
            format_ghost_check_result(&file, &[]),
            "No issues found for src/drift.ts."
        );
        assert_eq!(
            format_ghost_check_result(&file, &[issue]),
            "arc/stale fn:compute: fn:compute code fingerprint differs from recorded decision"
        );
    }

    #[test]
    fn exposes_all_current_rule_and_severity_strings() {
        assert_eq!(LintSeverity::Error.as_str(), "error");
        assert_eq!(LintSeverity::Warning.as_str(), "warning");
        assert_eq!(LintRule::Parser.as_str(), "arc/parser");
        assert_eq!(LintRule::Stale.as_str(), "arc/stale");
        assert_eq!(LintRule::Orphan.as_str(), "arc/orphan");
        assert_eq!(LintRule::Undecided.as_str(), "arc/undecided");
        assert_eq!(LintRule::Supersede.as_str(), "arc/supersede");
    }
}
