//! Minimal, std-only diagnostic logging (audit blocker B9).
//!
//! Archiva runs unattended as an agent hook, so silent automatic recovery
//! (dmap repair, stale-lock takeover, corrupt-file skips, git-HEAD fallback)
//! previously left no trace to investigate misbehavior. This module adds an
//! env-gated stderr channel — no dependencies, no global logger framework.
//!
//! Activation and level are controlled by the `ARCHIVA_LOG` environment
//! variable (`error`, `warn`, `info`, `debug`, `trace`; also accepts `1`/`true`
//! as `info`, `0`/`false`/empty as off). Diagnostics always go to stderr so
//! they never corrupt stdout — which for `mcp` carries the JSON-RPC stream and
//! for the CLI carries command output.

use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Level {
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    fn from_env_value(value: &str) -> Level {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "0" | "off" | "false" | "none" => Level::Off,
            "1" | "true" | "on" | "info" => Level::Info,
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "debug" => Level::Debug,
            "trace" | "verbose" => Level::Trace,
            // Unknown value: enable at info so misconfiguration still yields
            // output rather than silence.
            _ => Level::Info,
        }
    }
}

// 0 means "not yet initialized"; otherwise stores `level as u8 + 1`.
static LEVEL: AtomicU8 = AtomicU8::new(0);

fn encode(level: Level) -> u8 {
    level as u8 + 1
}

fn decode(raw: u8) -> Option<Level> {
    match raw {
        0 => None,
        1 => Some(Level::Off),
        2 => Some(Level::Error),
        3 => Some(Level::Warn),
        4 => Some(Level::Info),
        5 => Some(Level::Debug),
        _ => Some(Level::Trace),
    }
}

/// Explicitly set the active level (used by `--verbose`), overriding the env.
pub fn set_level(level: Level) {
    LEVEL.store(encode(level), Ordering::Relaxed);
}

/// The active level, initializing from `ARCHIVA_LOG` on first use.
pub fn level() -> Level {
    if let Some(level) = decode(LEVEL.load(Ordering::Relaxed)) {
        return level;
    }
    let resolved = std::env::var("ARCHIVA_LOG")
        .ok()
        .map(|value| Level::from_env_value(&value))
        .unwrap_or(Level::Off);
    // Only store if still uninitialized, so an explicit set_level wins a race.
    let _ = LEVEL.compare_exchange(0, encode(resolved), Ordering::Relaxed, Ordering::Relaxed);
    decode(LEVEL.load(Ordering::Relaxed)).unwrap_or(Level::Off)
}

pub fn enabled(target: Level) -> bool {
    target != Level::Off && level() >= target
}

/// Emit a diagnostic line to stderr if `target` is enabled. Never touches
/// stdout, so it is safe to call from the MCP server and every CLI command.
pub fn log(target: Level, message: &str) {
    if !enabled(target) {
        return;
    }
    // Best-effort: diagnostics must never fail a command.
    let _ = writeln!(
        std::io::stderr(),
        "[archiva {}] {}",
        target.label(),
        message
    );
}

#[macro_export]
macro_rules! diag {
    ($level:expr, $($arg:tt)*) => {{
        if $crate::core::diagnostics::enabled($level) {
            $crate::core::diagnostics::log($level, &format!($($arg)*));
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::{Level, LEVEL};
    use std::sync::atomic::Ordering;

    #[test]
    fn parses_env_values_to_levels() {
        assert_eq!(Level::from_env_value(""), Level::Off);
        assert_eq!(Level::from_env_value("0"), Level::Off);
        assert_eq!(Level::from_env_value("false"), Level::Off);
        assert_eq!(Level::from_env_value("1"), Level::Info);
        assert_eq!(Level::from_env_value("true"), Level::Info);
        assert_eq!(Level::from_env_value("warn"), Level::Warn);
        assert_eq!(Level::from_env_value("WARNING"), Level::Warn);
        assert_eq!(Level::from_env_value("debug"), Level::Debug);
        assert_eq!(Level::from_env_value("trace"), Level::Trace);
        assert_eq!(Level::from_env_value("verbose"), Level::Trace);
        assert_eq!(Level::from_env_value("nonsense"), Level::Info);
    }

    #[test]
    fn set_level_controls_enabled() {
        super::set_level(Level::Warn);
        assert!(super::enabled(Level::Error));
        assert!(super::enabled(Level::Warn));
        assert!(!super::enabled(Level::Info));
        assert!(!super::enabled(Level::Debug));

        super::set_level(Level::Off);
        assert!(!super::enabled(Level::Error));

        super::set_level(Level::Trace);
        assert!(super::enabled(Level::Trace));

        // Reset so other tests observe a clean slate.
        LEVEL.store(0, Ordering::Relaxed);
    }

    #[test]
    fn level_ordering_is_monotonic() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
    }
}
