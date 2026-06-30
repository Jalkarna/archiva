use std::{env, io::Read, path::Path};

use crate::core::decision::parse_write_decision_input_json;
use crate::core::error::{ArchivaError, Result};
use crate::core::init::init_project;
use crate::core::json::DEFAULT_MAX_BYTES;
use crate::core::lint::{format_lint_issues, has_error_issue};
use crate::core::paths::RelativePath;
use crate::core::project;
use crate::core::version::APPLICATION_VERSION;

pub struct CliResult {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CliResult {
    fn ok(stdout: impl Into<String>) -> Self {
        Self::with_status(0, stdout)
    }

    fn with_status(status: i32, stdout: impl Into<String>) -> Self {
        Self {
            status,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    fn err(message: impl Into<String>) -> Self {
        Self {
            status: 1,
            stdout: String::new(),
            stderr: format!("{}\n", message.into()),
        }
    }
}

pub fn run_cli(args: &[String], stdin: &str, project_root: &Path) -> CliResult {
    if args.first().is_some_and(|command| command == "lint") {
        return run_lint_cli(&args[1..], project_root);
    }

    match run_cli_result(args, stdin, project_root) {
        Ok(output) => CliResult::ok(output),
        Err(error) => CliResult::err(error.user_message()),
    }
}

fn run_cli_result(args: &[String], stdin: &str, project_root: &Path) -> Result<String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(main_help());
    };

    match command {
        "-V" | "--version" => Ok(format!("{APPLICATION_VERSION}\n")),
        "-h" | "--help" => Ok(main_help()),
        "help" => run_help(&args[1..]),
        "init" => run_init(&args[1..], project_root),
        "why" => run_why(&args[1..], project_root),
        "history" => run_history(&args[1..], project_root),
        "hooks" => run_hooks(&args[1..], project_root),
        "write-decision" => run_write_decision(&args[1..], stdin, project_root),
        "status" => run_status(&args[1..], project_root),
        "lint" => unreachable!("lint is handled before run_cli_result"),
        "mcp" => run_mcp(&args[1..]),
        value if value.starts_with('-') => Err(ArchivaError::cli(format!(
            "error: unknown option '{}'",
            value
        ))),
        value => Err(ArchivaError::cli(format!(
            "error: unknown command '{}'",
            value
        ))),
    }
}

fn run_help(args: &[String]) -> Result<String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(main_help());
    };
    if args.len() > 1 {
        return Err(ArchivaError::cli(format!(
            "error: unexpected argument '{}'",
            args[1]
        )));
    }
    Ok(match command {
        "init" => init_help(),
        "why" => why_help(),
        "history" => history_help(),
        "hooks" => hooks_help(),
        "status" => status_help(),
        "lint" => lint_help(),
        "mcp" => mcp_help(),
        "write-decision" => write_decision_help(),
        value => {
            return Err(ArchivaError::cli(format!(
                "error: unknown command '{}'",
                value
            )));
        }
    })
}

fn run_init(args: &[String], project_root: &Path) -> Result<String> {
    let mut gitignore_decisions = false;
    for arg in args {
        match arg.as_str() {
            "--gitignore-decisions" => gitignore_decisions = true,
            "-h" | "--help" => return Ok(init_help()),
            value if value.starts_with('-') => {
                return Err(ArchivaError::cli(format!(
                    "error: unknown option '{}'",
                    value
                )));
            }
            value => {
                return Err(ArchivaError::cli(format!(
                    "error: unexpected argument '{}'",
                    value
                )));
            }
        }
    }
    Ok(format!(
        "{}\n",
        init_project(project_root, gitignore_decisions)?
    ))
}

fn run_why(args: &[String], project_root: &Path) -> Result<String> {
    let Some(args) = parse_positional_args(args)? else {
        return Ok(why_help());
    };
    let Some(file) = args.first() else {
        return Err(ArchivaError::cli("error: missing required argument 'file'"));
    };
    if args.len() > 2 {
        return Err(ArchivaError::cli(format!(
            "error: unexpected argument '{}'",
            args[2]
        )));
    }
    let relative = RelativePath::new(file)?;
    let output = match args.get(1) {
        Some(line_or_anchor)
            if !line_or_anchor.is_empty()
                && line_or_anchor.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            let line = line_or_anchor
                .parse::<u32>()
                .map_err(|_| ArchivaError::cli("line must be a positive integer"))?;
            project::why_for_line(project_root, &relative, line)?
        }
        Some(anchor) => project::why(project_root, &relative, Some(anchor))?,
        None => project::why(project_root, &relative, None)?,
    };
    Ok(format!("{output}\n"))
}

fn run_history(args: &[String], project_root: &Path) -> Result<String> {
    let Some(args) = parse_positional_args(args)? else {
        return Ok(history_help());
    };
    let Some(file) = args.first() else {
        return Err(ArchivaError::cli("error: missing required argument 'file'"));
    };
    let Some(anchor) = args.get(1) else {
        return Err(ArchivaError::cli(
            "error: missing required argument 'anchor'",
        ));
    };
    if args.len() > 2 {
        return Err(ArchivaError::cli(format!(
            "error: unexpected argument '{}'",
            args[2]
        )));
    }
    let file = RelativePath::new(file)?;
    Ok(format!(
        "{}\n",
        project::history(project_root, &file, anchor)?
    ))
}

fn run_hooks(args: &[String], project_root: &Path) -> Result<String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(hooks_help());
    };
    match command {
        "-h" | "--help" => Ok(hooks_help()),
        "help" => {
            if let Some(extra) = args.get(2) {
                return Err(ArchivaError::cli(format!(
                    "error: unexpected argument '{}'",
                    extra
                )));
            }
            match args.get(1).map(String::as_str) {
                None => Ok(hooks_help()),
                Some("session-start") => Ok(hooks_session_start_help()),
                Some("post-tool-use") => Ok(post_tool_use_help()),
                Some(value) => Err(ArchivaError::cli(format!(
                    "error: unknown command '{}'",
                    value
                ))),
            }
        }
        "session-start" => {
            if args
                .get(1)
                .is_some_and(|arg| matches!(arg.as_str(), "-h" | "--help"))
            {
                return Ok(hooks_session_start_help());
            }
            if let Some(extra) = args.get(1) {
                return Err(ArchivaError::cli(format!(
                    "error: unexpected argument '{}'",
                    extra
                )));
            }
            Ok(format!("{}\n", project::session_start(project_root)?))
        }
        "post-tool-use" => run_post_tool_use(&args[1..], project_root),
        value => Err(ArchivaError::cli(format!(
            "error: unknown command '{}'",
            value
        ))),
    }
}

fn run_post_tool_use(args: &[String], project_root: &Path) -> Result<String> {
    let Some(args) = parse_positional_args(args)? else {
        return Ok(post_tool_use_help());
    };
    if args.len() > 1 {
        return Err(ArchivaError::cli(format!(
            "error: unexpected argument '{}'",
            args[1]
        )));
    }
    let target = args
        .first()
        .filter(|value| !value.is_empty())
        .map(|value| (*value).to_string())
        .or_else(|| {
            env::var("ARCHIVA_FILE")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .ok_or_else(|| ArchivaError::cli("Missing file path. Pass one or set ARCHIVA_FILE."))?;
    let file = RelativePath::new(&target)?;
    Ok(format!(
        "{}\n",
        project::post_tool_use(project_root, &file)?
    ))
}

fn parse_positional_args(args: &[String]) -> Result<Option<Vec<&String>>> {
    let mut escaped = false;
    let mut positionals = Vec::new();
    for arg in args {
        if !escaped && arg == "--" {
            escaped = true;
            continue;
        }
        if !escaped && matches!(arg.as_str(), "-h" | "--help") {
            return Ok(None);
        }
        if !escaped && arg.starts_with('-') {
            return Err(ArchivaError::cli(format!(
                "error: unknown option '{}'",
                arg
            )));
        }
        positionals.push(arg);
    }
    Ok(Some(positionals))
}

fn run_write_decision(args: &[String], stdin: &str, project_root: &Path) -> Result<String> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        return Ok(write_decision_help());
    }

    let mut json = None::<&str>;
    let mut index = 0_usize;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                let Some(value) = args.get(index + 1) else {
                    return Err(ArchivaError::cli(
                        "error: option '--json <json>' argument missing",
                    ));
                };
                json = Some(value);
                index += 2;
            }
            value if value.starts_with("--json=") => {
                json = Some(&value["--json=".len()..]);
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err(ArchivaError::cli(format!(
                    "error: unknown option '{}'",
                    value
                )));
            }
            value => {
                return Err(ArchivaError::cli(format!(
                    "error: unexpected argument '{}'",
                    value
                )));
            }
        }
    }

    let raw = json.unwrap_or(stdin);
    let input = parse_write_decision_input_json(raw)?;
    let decision = project::write_decision(project_root, &input)?;
    Ok(format!("Recorded {}.\n", decision.id))
}

fn run_status(args: &[String], project_root: &Path) -> Result<String> {
    if let Some(arg) = args.first() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(status_help()),
            value if value.starts_with('-') => {
                return Err(ArchivaError::cli(format!(
                    "error: unknown option '{}'",
                    value
                )));
            }
            value => {
                return Err(ArchivaError::cli(format!(
                    "error: unexpected argument '{}'",
                    value
                )));
            }
        }
    }

    Ok(format!("{}\n", project::status(project_root)?))
}

fn run_lint_cli(args: &[String], project_root: &Path) -> CliResult {
    match run_lint(args, project_root) {
        Ok((status, output)) => CliResult::with_status(status, output),
        Err(error) => CliResult::err(error.user_message()),
    }
}

fn run_lint(args: &[String], project_root: &Path) -> Result<(i32, String)> {
    let mut fix = false;
    for arg in args {
        match arg.as_str() {
            "--fix" => fix = true,
            "-h" | "--help" => return Ok((0, lint_help())),
            value if value.starts_with('-') => {
                return Err(ArchivaError::cli(format!(
                    "error: unknown option '{}'",
                    value
                )));
            }
            value => {
                return Err(ArchivaError::cli(format!(
                    "error: unexpected argument '{}'",
                    value
                )));
            }
        }
    }

    let issues = project::lint_project(project_root, fix)?;
    let status = if has_error_issue(&issues) { 1 } else { 0 };
    Ok((status, format!("{}\n", format_lint_issues(&issues))))
}

fn run_mcp(args: &[String]) -> Result<String> {
    if args.is_empty() {
        return Err(ArchivaError::cli(
            "MCP stdio server is available through the native binary entrypoint: archiva mcp",
        ));
    }
    if let Some(arg) = args.first() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(mcp_help()),
            value if value.starts_with('-') => {
                return Err(ArchivaError::cli(format!(
                    "error: unknown option '{}'",
                    value
                )));
            }
            value => {
                return Err(ArchivaError::cli(format!(
                    "error: unexpected argument '{}'",
                    value
                )));
            }
        }
    }
    Ok(mcp_help())
}

pub fn read_stdin_to_string() -> Result<String> {
    let stdin = std::io::stdin();
    read_to_string_with_limit(stdin.lock(), DEFAULT_MAX_BYTES)
}

fn read_to_string_with_limit(reader: impl Read, max_bytes: usize) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| ArchivaError::io(None, "read stdin", source))?;
    if bytes.len() > max_bytes {
        return Err(ArchivaError::cli(
            "JSON input exceeds configured byte limit",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|source| ArchivaError::cli(format!("stdin was not valid UTF-8: {source}")))
}

fn main_help() -> String {
    "Usage: archiva [options] [command]\n\nDecision layer for agentic codebases.\n\nOptions:\n  -V, --version              output the version number\n  -h, --help                 display help for command\n\nCommands:\n  init [options]             Set up Archiva in the current project\n  status                     Show decision health across the repo\n  why <file> [lineOrAnchor]  Explain why code was written\n  history <file> <anchor>    Show the decision history chain for an anchor\n  lint [options]             Run decision lint rules\n  hooks                      Run Archiva hook commands\n  mcp                        Start the Archiva MCP server over stdio\n  write-decision [options]   Record a decision from JSON on stdin or --json\n  help [command]             display help for command\n".to_string()
}

fn hooks_help() -> String {
    "Usage: archiva hooks [options] [command]\n\nRun Archiva hook commands\n\nOptions:\n  -h, --help            display help for command\n\nCommands:\n  session-start         Print compact decision context\n  post-tool-use [file]  Re-anchor decisions after a file edit\n  help [command]        display help for command\n".to_string()
}

fn post_tool_use_help() -> String {
    "Usage: archiva hooks post-tool-use [options] [file]\n\nRe-anchor decisions after a file edit\n\nOptions:\n  -h, --help  display help for command\n".to_string()
}

fn hooks_session_start_help() -> String {
    "Usage: archiva hooks session-start [options]\n\nPrint compact decision context\n\nOptions:\n  -h, --help  display help for command\n".to_string()
}

fn init_help() -> String {
    "Usage: archiva init [options]\n\nSet up Archiva in the current project\n\nOptions:\n  --gitignore-decisions  add .decisions/ to .gitignore instead of tracking decisions\n  -h, --help             display help for command\n".to_string()
}

fn why_help() -> String {
    "Usage: archiva why [options] <file> [lineOrAnchor]\n\nExplain why code was written\n\nOptions:\n  -h, --help  display help for command\n".to_string()
}

fn history_help() -> String {
    "Usage: archiva history [options] <file> <anchor>\n\nShow the decision history chain for an anchor\n\nOptions:\n  -h, --help  display help for command\n".to_string()
}

fn status_help() -> String {
    "Usage: archiva status [options]\n\nShow decision health across the repo\n\nOptions:\n  -h, --help  display help for command\n".to_string()
}

fn lint_help() -> String {
    "Usage: archiva lint [options]\n\nRun decision lint rules\n\nOptions:\n  --fix       apply safe fixes\n  -h, --help  display help for command\n".to_string()
}

fn mcp_help() -> String {
    "Usage: archiva mcp [options]\n\nStart the Archiva MCP server over stdio\n\nOptions:\n  -h, --help  display help for command\n".to_string()
}

fn write_decision_help() -> String {
    "Usage: archiva write-decision [options]\n\nRecord a decision from JSON on stdin or --json\n\nOptions:\n  --json <json>  write_decision input JSON\n  -h, --help     display help for command\n".to_string()
}

#[cfg(test)]
mod tests {
    use super::{read_to_string_with_limit, run_cli};
    use crate::core::paths::{decision_lock_path, dlog_path, dmap_path, RelativePath};
    use crate::core::storage::load_dlog;
    use std::fs;
    use std::io::{self, Read};
    use std::path::{Path, PathBuf};

    #[test]
    fn prints_version_and_commander_compatible_help() {
        let root = unique_temp_dir("archiva-cli-help");
        let version = run_cli(&["--version".to_string()], "", &root);
        assert_eq!(version.status, 0);
        assert_eq!(version.stdout, concat!(env!("CARGO_PKG_VERSION"), "\n"));
        assert_eq!(version.stderr, "");

        let help = run_cli(&["--help".to_string()], "", &root);
        assert_eq!(help.status, 0);
        assert!(help.stdout.contains("Usage: archiva [options] [command]"));
        assert!(help.stdout.contains("write-decision [options]"));

        let hooks = run_cli(&["hooks".to_string(), "--help".to_string()], "", &root);
        assert_eq!(hooks.status, 0);
        assert!(hooks
            .stdout
            .contains("Usage: archiva hooks [options] [command]"));

        let mcp = run_cli(&["mcp".to_string(), "--help".to_string()], "", &root);
        assert_eq!(mcp.status, 0);
        assert!(mcp.stdout.contains("Usage: archiva mcp [options]"));

        let why_help = run_cli(&["help".to_string(), "why".to_string()], "", &root);
        assert_eq!(why_help.status, 0);
        assert!(why_help
            .stdout
            .contains("Usage: archiva why [options] <file> [lineOrAnchor]"));

        let hook_help = run_cli(
            &[
                "hooks".to_string(),
                "help".to_string(),
                "session-start".to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(hook_help.status, 0);
        assert!(hook_help
            .stdout
            .contains("Usage: archiva hooks session-start [options]"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn surfaces_current_argument_errors() {
        let root = unique_temp_dir("archiva-cli-errors");
        fs::create_dir_all(&root).unwrap();
        assert_eq!(
            run_cli(&["nope".to_string()], "", &root).stderr,
            "error: unknown command 'nope'\n"
        );
        assert_eq!(
            run_cli(&["init".to_string(), "--bad".to_string()], "", &root).stderr,
            "error: unknown option '--bad'\n"
        );
        assert_eq!(
            run_cli(&["why".to_string()], "", &root).stderr,
            "error: missing required argument 'file'\n"
        );
        assert_eq!(
            run_cli(
                &[
                    "why".to_string(),
                    "src/file.ts".to_string(),
                    "--bad".to_string()
                ],
                "",
                &root
            )
            .stderr,
            "error: unknown option '--bad'\n"
        );
        let escaped_why = run_cli(
            &[
                "why".to_string(),
                "src/file.ts".to_string(),
                "--".to_string(),
                "--bad".to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(escaped_why.status, 0);
        assert_eq!(escaped_why.stdout, "No decisions found for src/file.ts.\n");
        assert_eq!(
            run_cli(
                &[
                    "history".to_string(),
                    "src/file.ts".to_string(),
                    "--bad".to_string()
                ],
                "",
                &root
            )
            .stderr,
            "error: unknown option '--bad'\n"
        );
        let escaped_history = run_cli(
            &[
                "history".to_string(),
                "src/file.ts".to_string(),
                "--".to_string(),
                "--bad".to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(escaped_history.status, 0);
        assert_eq!(
            escaped_history.stdout,
            "No decision found for src/file.ts at --bad.\n"
        );
        assert_eq!(
            run_cli(
                &[
                    "why".to_string(),
                    "src/file.ts".to_string(),
                    "fn:a".to_string(),
                    "extra".to_string()
                ],
                "",
                &root
            )
            .stderr,
            "error: unexpected argument 'extra'\n"
        );
        assert_eq!(
            run_cli(
                &[
                    "hooks".to_string(),
                    "post-tool-use".to_string(),
                    "--bad".to_string()
                ],
                "",
                &root
            )
            .stderr,
            "error: unknown option '--bad'\n"
        );
        let escaped_post_tool_use = run_cli(
            &[
                "hooks".to_string(),
                "post-tool-use".to_string(),
                "--".to_string(),
                "--bad".to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(escaped_post_tool_use.status, 0);
        assert_eq!(
            escaped_post_tool_use.stdout,
            "No decisions for --bad; nothing to re-anchor.\n"
        );
        assert_eq!(
            run_cli(
                &[
                    "hooks".to_string(),
                    "session-start".to_string(),
                    "extra".to_string()
                ],
                "",
                &root
            )
            .stderr,
            "error: unexpected argument 'extra'\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runs_init_and_read_workflows() {
        let root = unique_temp_dir("archiva-cli-init-read");
        let init = run_cli(&["init".to_string()], "", &root);
        assert_eq!(init.status, 0);
        assert_eq!(init.stdout, "Archiva initialized.\n");
        assert!(root.join(".decisions").exists());
        assert!(root.join(".claude").join("settings.json").exists());

        let session = run_cli(
            &["hooks".to_string(), "session-start".to_string()],
            "",
            &root,
        );
        assert_eq!(session.status, 0);
        assert_eq!(session.stdout, "[Archiva] No decision map found.\n");

        let why = run_cli(
            &["why".to_string(), "src/missing.ts".to_string()],
            "",
            &root,
        );
        assert_eq!(why.status, 0);
        assert_eq!(why.stdout, "No decisions found for src/missing.ts.\n");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn writes_decision_from_json_and_reads_it_back() {
        let root = unique_temp_dir("archiva-cli-write");
        let source_path = root.join("src").join("a.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "export function makeThing() {\n  return 1;\n}\n",
        )
        .unwrap();

        let json = r#"{"file":"src/a.ts","anchor":"fn:makeThing","lines":[1,3],"chose":"plain function","because":"fixture","rejected":[{"approach":"class","reason":"unneeded"}],"session":"sess_cli"}"#;
        let write = run_cli(
            &[
                "write-decision".to_string(),
                "--json".to_string(),
                json.to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(write.status, 0);
        assert_eq!(write.stdout, "Recorded dec_001.\n");

        let file = RelativePath::new("src/a.ts").unwrap();
        let dlog = load_dlog(&root, &file).unwrap().unwrap();
        assert_eq!(
            dlog.decisions
                .get_str("fn:makeThing")
                .unwrap()
                .session
                .as_deref(),
            Some("sess_cli")
        );

        let why = run_cli(
            &[
                "why".to_string(),
                "src/a.ts".to_string(),
                "fn:makeThing".to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(why.status, 0);
        assert!(why.stdout.contains("plain function"));

        let status = run_cli(&["status".to_string()], "", &root);
        assert_eq!(status.status, 0);
        assert!(status.stdout.contains("src/a.ts"));
        assert!(status.stdout.contains("1 decisions"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bounded_stdin_reader_rejects_over_limit_without_eof() {
        struct EndlessReader {
            bytes_read: usize,
        }

        impl Read for &mut EndlessReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let count = buffer.len().min(2);
                for byte in &mut buffer[..count] {
                    *byte = b'{';
                }
                self.bytes_read += count;
                Ok(count)
            }
        }

        let mut reader = EndlessReader { bytes_read: 0 };
        let error = read_to_string_with_limit(&mut reader, 5).unwrap_err();

        assert_eq!(
            error.user_message(),
            "JSON input exceeds configured byte limit"
        );
        assert_eq!(reader.bytes_read, 6);
    }

    #[test]
    fn normalizes_tool_supplied_paths_across_cli_commands() {
        let root = unique_temp_dir("archiva-cli-normalized-paths");
        let source_path = root.join("src").join("path.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(
            &source_path,
            "export function pathTarget() {\n  return 1;\n}\n",
        )
        .unwrap();

        let json = r#"{"file":".//src/path.ts","anchor":"fn:pathTarget","lines":[1,3],"chose":"normalize tool paths","because":"fixture","rejected":[]}"#;
        let write = run_cli(
            &[
                "write-decision".to_string(),
                "--json".to_string(),
                json.to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(write.status, 0);
        assert_eq!(write.stdout, "Recorded dec_001.\n");

        let file = RelativePath::new("src/path.ts").unwrap();
        assert!(dlog_path(&root, &file).exists());

        let why = run_cli(
            &[
                "why".to_string(),
                "src\\path.ts".to_string(),
                "fn:pathTarget".to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(why.status, 0);
        assert!(why.stdout.contains("normalize tool paths"));

        fs::write(
            &source_path,
            "export function pathTarget() {\n  return 2;\n}\n",
        )
        .unwrap();
        let post = run_cli(
            &[
                "hooks".to_string(),
                "post-tool-use".to_string(),
                ".\\src\\path.ts".to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(post.status, 0);
        assert_eq!(post.stdout, "Re-anchored src/path.ts: 1 stale, 0 orphan.\n");
        assert_eq!(
            load_dlog(&root, &file)
                .unwrap()
                .unwrap()
                .decisions
                .get_str("fn:pathTarget")
                .unwrap()
                .status
                .as_ref()
                .map(|status| status.as_str()),
            Some("STALE")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lint_reports_errors_with_non_zero_status() {
        let root = unique_temp_dir("archiva-cli-lint");
        let source_path = root.join("src").join("stale.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function makeThing() {\n  return 1;\n}\n").unwrap();

        let json = r#"{"file":"src/stale.ts","anchor":"fn:makeThing","lines":[1,3],"chose":"plain function","because":"fixture","rejected":[]}"#;
        assert_eq!(
            run_cli(
                &[
                    "write-decision".to_string(),
                    "--json".to_string(),
                    json.to_string()
                ],
                "",
                &root
            )
            .status,
            0
        );
        fs::write(&source_path, "function makeThing() {\n  return 2;\n}\n").unwrap();

        let lint = run_cli(&["lint".to_string()], "", &root);
        assert_eq!(lint.status, 1);
        assert_eq!(lint.stderr, "");
        assert!(lint.stdout.contains(
            "ERROR arc/stale src/stale.ts fn:makeThing: fn:makeThing code fingerprint differs from recorded decision"
        ));

        let clean = run_cli(&["lint".to_string(), "--fix".to_string()], "", &root);
        assert_eq!(clean.status, 1);
        assert!(clean.stdout.contains("arc/supersede"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_dlog_fails_without_rewriting_or_leaving_locks() {
        let root = unique_temp_dir("archiva-cli-malformed-dlog");
        let file = RelativePath::new("src/bad.ts").unwrap();
        let source_path = root.join("src").join("bad.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function bad() {\n  return 1;\n}\n").unwrap();
        let dlog_path = dlog_path(&root, &file);
        fs::create_dir_all(dlog_path.parent().unwrap()).unwrap();
        let malformed = "schema: nope\nfile: src/bad.ts\ndecisions: {}\n";
        fs::write(&dlog_path, malformed).unwrap();

        let why = run_cli(&["why".to_string(), "src/bad.ts".to_string()], "", &root);
        assert_eq!(why.status, 1);
        assert_eq!(why.stdout, "");
        assert!(why.stderr.contains("schema"));

        let lint = run_cli(&["lint".to_string()], "", &root);
        assert_eq!(lint.status, 1);
        assert_eq!(lint.stdout, "");
        assert!(lint.stderr.contains("schema"));

        let write = run_cli(
            &[
                "write-decision".to_string(),
                "--json".to_string(),
                r#"{"file":"src/bad.ts","anchor":"fn:bad","lines":[1,3],"chose":"do not overwrite corruption","because":"corrupt dlog fixture","rejected":[]}"#
                    .to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(write.status, 1);
        assert_eq!(write.stdout, "");
        assert!(write.stderr.contains("schema"));
        assert_eq!(fs::read_to_string(&dlog_path).unwrap(), malformed);
        assert!(!dmap_path(&root, &file).exists());
        assert!(!decision_lock_path(&root, &file).exists());
        assert!(temp_siblings(dlog_path.parent().unwrap()).is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn session_start_corrupt_later_dlog_fails_without_partial_stdout() {
        let root = unique_temp_dir("archiva-cli-session-corrupt-dlog");
        let source_path = root.join("src").join("a.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function good() {\n  return 1;\n}\n").unwrap();

        let write = run_cli(
            &[
                "write-decision".to_string(),
                "--json".to_string(),
                r#"{"file":"src/a.ts","anchor":"fn:good","lines":[1,3],"chose":"valid first dlog","because":"fixture","rejected":[]}"#
                    .to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(write.status, 0);

        let bad_file = RelativePath::new("src/z-bad.ts").unwrap();
        let bad_dlog_path = dlog_path(&root, &bad_file);
        fs::create_dir_all(bad_dlog_path.parent().unwrap()).unwrap();
        fs::write(
            &bad_dlog_path,
            "schema: nope\nfile: src/z-bad.ts\ndecisions: {}\n",
        )
        .unwrap();

        let session = run_cli(
            &["hooks".to_string(), "session-start".to_string()],
            "",
            &root,
        );
        assert_eq!(session.status, 1);
        assert_eq!(session.stdout, "");
        assert!(session.stderr.contains("schema"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_tool_use_reanchors_from_cli_hook() {
        let root = unique_temp_dir("archiva-cli-post-tool-use");
        let source_path = root.join("src").join("hook.ts");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "function hookTarget() {\n  return 1;\n}\n").unwrap();

        let json = r#"{"file":"src/hook.ts","anchor":"fn:hookTarget","lines":[1,3],"chose":"plain function","because":"fixture","rejected":[]}"#;
        assert_eq!(
            run_cli(
                &[
                    "write-decision".to_string(),
                    "--json".to_string(),
                    json.to_string()
                ],
                "",
                &root
            )
            .status,
            0
        );
        fs::write(&source_path, "function hookTarget() {\n  return 2;\n}\n").unwrap();

        let post = run_cli(
            &[
                "hooks".to_string(),
                "post-tool-use".to_string(),
                "src/hook.ts".to_string(),
            ],
            "",
            &root,
        );
        assert_eq!(post.status, 0);
        assert_eq!(post.stdout, "Re-anchored src/hook.ts: 1 stale, 0 orphan.\n");

        let file = RelativePath::new("src/hook.ts").unwrap();
        assert_eq!(
            load_dlog(&root, &file)
                .unwrap()
                .unwrap()
                .decisions
                .get_str("fn:hookTarget")
                .unwrap()
                .status
                .as_ref()
                .map(|status| status.as_str()),
            Some("STALE")
        );

        let _ = fs::remove_dir_all(root);
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

    fn temp_siblings(parent: &Path) -> Vec<PathBuf> {
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
