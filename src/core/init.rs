use std::fs;
use std::path::Path;

use crate::core::error::{ArchivaError, Result};
use crate::core::fs::{atomic_write_text, read_text_if_exists};
use crate::core::settings::merge_claude_settings_json;

pub const INIT_SUCCESS: &str = "Archiva initialized.";
pub const GITIGNORE_DECISIONS_ENTRY: &str = ".decisions/";
pub const AGENTS_MARKER: &str = "## Decision Logging (Archiva)";
pub const AGENTS_BLOCK: &str = r#"## Decision Logging (Archiva)

This project uses Archiva for decision tracking.

### Before modifying any file
- Read the decision map injected at session start (prefixed `[Archiva]`)
- Or call the `why` MCP tool: `why(file, anchor)`
- Do NOT modify code marked with a decision without reading it first

### After any non-trivial implementation choice
Call `write_decision` with:
- `file` and `anchor` (function or block name)
- `chose` - what approach you selected
- `because` - the specific reason, not a generic description
- `rejected` - every alternative you considered, with specific disqualifying reasons

Required for: algorithm choices, concurrency patterns, error handling strategies,
any point where you weighed 2+ approaches.

Not required for: imports, type declarations, formatting, variable names.

If changing code that has an existing decision:
- If your change preserves the reasoning -> keep the decision, update `lines_hint`
- If your change invalidates the reasoning -> call `write_decision` with `supersedes: <id>`
"#;

pub fn init_project(project_root: &Path, gitignore_decisions: bool) -> Result<&'static str> {
    let decisions_dir = project_root.join(".decisions");
    fs::create_dir_all(&decisions_dir).map_err(|source| {
        ArchivaError::io(Some(decisions_dir), "create decisions directory", source)
    })?;

    let settings_path = project_root.join(".claude").join("settings.json");
    let settings = merge_claude_settings_json(read_text_if_exists(&settings_path)?.as_deref())?;
    atomic_write_text(&settings_path, &settings)?;

    let agents_path = project_root.join("AGENTS.md");
    let existing_agents = read_text_if_exists(&agents_path)?;
    let merged_agents = merge_agents_md(existing_agents.as_deref());
    if existing_agents.as_deref() != Some(merged_agents.as_str()) {
        atomic_write_text(&agents_path, &merged_agents)?;
    }

    if gitignore_decisions {
        let gitignore_path = project_root.join(".gitignore");
        let existing_gitignore = read_text_if_exists(&gitignore_path)?;
        let merged_gitignore =
            ensure_gitignore_entry(existing_gitignore.as_deref(), GITIGNORE_DECISIONS_ENTRY);
        if existing_gitignore.as_deref() != Some(merged_gitignore.as_str()) {
            atomic_write_text(&gitignore_path, &merged_gitignore)?;
        }
    }

    Ok(INIT_SUCCESS)
}

pub fn merge_agents_md(existing: Option<&str>) -> String {
    let existing = existing.unwrap_or("");
    if existing.contains(AGENTS_MARKER) {
        return existing.to_string();
    }

    let prefix = if existing.trim().is_empty() {
        String::new()
    } else {
        format!("{}\n\n", existing.trim_end())
    };
    format!("{prefix}{AGENTS_BLOCK}")
}

pub fn ensure_gitignore_entry(existing: Option<&str>, entry: &str) -> String {
    let existing = existing.unwrap_or("");
    if gitignore_lines(existing).iter().any(|line| line == entry) {
        return existing.to_string();
    }

    let prefix = if existing.trim().is_empty() {
        String::new()
    } else {
        format!("{}\n", existing.trim_end())
    };
    format!("{prefix}{entry}\n")
}

fn gitignore_lines(input: &str) -> Vec<String> {
    let parts = input.split('\n').collect::<Vec<_>>();
    parts
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if index + 1 < parts.len() {
                line.strip_suffix('\r').unwrap_or(line).to_string()
            } else {
                (*line).to_string()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_gitignore_entry, init_project, merge_agents_md, AGENTS_BLOCK, AGENTS_MARKER,
        GITIGNORE_DECISIONS_ENTRY, INIT_SUCCESS,
    };
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn exposes_current_init_success_text() {
        assert_eq!(INIT_SUCCESS, "Archiva initialized.");
    }

    #[test]
    fn init_project_creates_decisions_settings_agents_and_gitignore() {
        let root = unique_temp_dir("archiva-init-project");

        assert_eq!(init_project(&root, true).unwrap(), INIT_SUCCESS);

        assert!(root.join(".decisions").is_dir());
        let settings = fs::read_to_string(root.join(".claude").join("settings.json")).unwrap();
        assert!(settings.contains("\"command\": \"archiva\""));
        assert!(settings.contains("\"command\": \"archiva hooks session-start\""));
        assert!(settings.contains("\"command\": \"archiva hooks post-tool-use\""));
        assert_eq!(
            fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            AGENTS_BLOCK
        );
        assert_eq!(
            fs::read_to_string(root.join(".gitignore")).unwrap(),
            ".decisions/\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn init_project_merges_existing_files_and_respects_gitignore_option() {
        let root = unique_temp_dir("archiva-init-existing");
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::write(
            root.join(".claude").join("settings.json"),
            "{\n  \"mcpServers\": { \"other\": { \"command\": \"other\", \"args\": [] } },\n  \"hooks\": { \"SessionStart\": [{ \"hooks\": [{ \"type\": \"command\", \"command\": \"echo existing\" }] }] }\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("AGENTS.md"),
            format!("custom\n{AGENTS_MARKER}\nkeep\n"),
        )
        .unwrap();
        fs::write(root.join(".gitignore"), "dist/\n").unwrap();

        init_project(&root, false).unwrap();

        let settings = fs::read_to_string(root.join(".claude").join("settings.json")).unwrap();
        assert!(settings.contains("\"other\""));
        assert!(settings.contains("\"echo existing\""));
        assert!(settings.contains("\"archiva hooks session-start\""));
        assert!(settings.contains("\"archiva hooks post-tool-use\""));
        assert_eq!(
            fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            format!("custom\n{AGENTS_MARKER}\nkeep\n")
        );
        assert_eq!(
            fs::read_to_string(root.join(".gitignore")).unwrap(),
            "dist/\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn init_project_surfaces_invalid_existing_settings_after_creating_decisions_dir() {
        let root = unique_temp_dir("archiva-init-invalid-settings");
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::write(root.join(".claude").join("settings.json"), "[]").unwrap();

        let error = init_project(&root, true).unwrap_err();

        assert_eq!(
            error.user_message(),
            ".claude/settings.json: expected object"
        );
        assert!(root.join(".decisions").is_dir());
        assert!(!root.join("AGENTS.md").exists());
        assert!(!root.join(".gitignore").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn appends_agents_block_like_typescript_init() {
        assert_eq!(
            merge_agents_md(Some("Existing notes\n\n")),
            format!("Existing notes\n\n{AGENTS_BLOCK}")
        );
        assert_eq!(merge_agents_md(None), AGENTS_BLOCK);
        assert_eq!(merge_agents_md(Some("   \n\t")), AGENTS_BLOCK);
    }

    #[test]
    fn leaves_existing_agents_file_unchanged_when_marker_exists() {
        let existing = "custom\n## Decision Logging (Archiva)\nkeep\n";
        assert_eq!(merge_agents_md(Some(existing)), existing);
    }

    #[test]
    fn appends_gitignore_entry_with_typescript_trim_and_newline_behavior() {
        assert_eq!(
            ensure_gitignore_entry(Some("dist/\n\n"), GITIGNORE_DECISIONS_ENTRY),
            "dist/\n.decisions/\n"
        );
        assert_eq!(
            ensure_gitignore_entry(None, GITIGNORE_DECISIONS_ENTRY),
            ".decisions/\n"
        );
        assert_eq!(
            ensure_gitignore_entry(Some("   \n"), GITIGNORE_DECISIONS_ENTRY),
            ".decisions/\n"
        );
    }

    #[test]
    fn preserves_gitignore_when_entry_already_exists_as_exact_line() {
        let existing = "dist\r\n.decisions/\r\n";
        assert_eq!(
            ensure_gitignore_entry(Some(existing), GITIGNORE_DECISIONS_ENTRY),
            existing
        );

        assert_eq!(
            ensure_gitignore_entry(Some("dist/\n.decisions/\n"), GITIGNORE_DECISIONS_ENTRY),
            "dist/\n.decisions/\n"
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
}
