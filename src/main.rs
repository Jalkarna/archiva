use std::env;
use std::io::{self, Write};
use std::process;

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let stdin = if should_read_stdin(&args) {
        match archiva::cli::read_stdin_to_string() {
            Ok(input) => input,
            Err(error) => {
                let _ = writeln!(io::stderr(), "{}", error.user_message());
                process::exit(1);
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
    args.first().is_some_and(|arg| arg == "write-decision")
        && !args.iter().any(|arg| {
            arg == "--json" || arg.starts_with("--json=") || arg == "-h" || arg == "--help"
        })
}
