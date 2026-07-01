use std::env;
use std::io::{self, Write};
use std::process;

fn main() {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    // A global `--verbose`/`-v` flag raises the diagnostic level (audit blocker
    // B9). Strip it here before dispatch so per-command argument parsers, which
    // reject unknown options, never see it. `ARCHIVA_LOG` still works and an
    // explicit flag wins over it.
    if let Some(index) = args
        .iter()
        .position(|arg| arg == "--verbose" || arg == "-v")
    {
        args.remove(index);
        archiva::core::diagnostics::set_level(archiva::core::diagnostics::Level::Trace);
    }
    let stdin = if should_read_stdin(&args) {
        match archiva::cli::read_stdin_to_string() {
            Ok(input) => input,
            Err(error) => {
                // The PostToolUse hook fires after every Claude Code tool call,
                // including ones whose payload we cannot read (non-UTF-8 or
                // oversized). Refusing to start would surface a hard error in
                // the agent's tool stream, so degrade to an empty payload and
                // let the hook no-op instead of aborting.
                if is_post_tool_use_hook(&args) {
                    String::new()
                } else {
                    let _ = writeln!(io::stderr(), "{}", error.user_message());
                    process::exit(1);
                }
            }
        }
    } else {
        String::new()
    };
    let cwd = match env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            let _ = writeln!(io::stderr(), "Failed to read current directory: {error}");
            process::exit(1);
        }
    };

    if args.len() == 1 && args.first().is_some_and(|arg| arg == "mcp") {
        match archiva::mcp::serve_stdio(&cwd) {
            Ok(()) => process::exit(0),
            Err(error) => {
                let _ = writeln!(io::stderr(), "{}", error.user_message());
                process::exit(1);
            }
        }
    }

    let result = archiva::cli::run_cli(&args, &stdin, &cwd);
    if !result.stdout.is_empty() {
        let _ = write!(io::stdout(), "{}", result.stdout);
    }
    if !result.stderr.is_empty() {
        let _ = write!(io::stderr(), "{}", result.stderr);
    }
    process::exit(result.status);
}

fn should_read_stdin(args: &[String]) -> bool {
    let wants_help = args.iter().any(|arg| arg == "-h" || arg == "--help");
    if wants_help {
        return false;
    }
    let is_write_decision = args.first().is_some_and(|arg| arg == "write-decision")
        && !args
            .iter()
            .any(|arg| arg == "--json" || arg.starts_with("--json="));
    is_write_decision || is_post_tool_use_hook(args)
}

/// True for `archiva hooks post-tool-use` (with no explicit positional file
/// path), the Claude Code automation entry point that receives its JSON payload
/// on stdin.
fn is_post_tool_use_hook(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("hooks")
        || args.get(1).map(String::as_str) != Some("post-tool-use")
    {
        return false;
    }
    // If the caller already passed an explicit positional file path, there is
    // no stdin payload to read.
    !args[2..]
        .iter()
        .any(|arg| !arg.starts_with('-') && arg.as_str() != "--")
}
