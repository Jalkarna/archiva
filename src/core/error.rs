use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use crate::core::dmap::DmapError;
use crate::core::json::JsonError;
use crate::core::paths::PathError;
use crate::core::yaml::YamlError;

pub type Result<T> = std::result::Result<T, ArchivaError>;

#[derive(Debug)]
pub enum ArchivaError {
    Io {
        path: Option<PathBuf>,
        action: &'static str,
        source: io::Error,
    },
    FileTooLarge {
        path: PathBuf,
        limit: usize,
    },
    InvalidPath {
        input: String,
        reason: &'static str,
    },
    Json {
        line: usize,
        column: usize,
        message: String,
    },
    Yaml {
        line: usize,
        column: usize,
        message: String,
    },
    Schema {
        field: String,
        message: String,
    },
    Cli {
        message: String,
    },
    Anchor {
        message: String,
    },
    Git {
        message: String,
    },
    Mcp {
        code: i32,
        message: String,
    },
    Dmap {
        message: String,
    },
    /// Wraps another error with the file it came from, so parse/schema/IO
    /// failures on committed data name the offending file (audit blocker B5).
    Contextualized {
        path: PathBuf,
        source: Box<ArchivaError>,
    },
}

impl ArchivaError {
    pub fn io(path: impl Into<Option<PathBuf>>, action: &'static str, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            action,
            source,
        }
    }

    pub fn cli(message: impl Into<String>) -> Self {
        Self::Cli {
            message: message.into(),
        }
    }

    pub fn schema(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Schema {
            field: field.into(),
            message: message.into(),
        }
    }

    /// Attach the originating file to an error so whole-repo commands can point
    /// operators at the exact corrupt file. Idempotent-ish: re-wrapping keeps
    /// the innermost (first-attached) path, since that is the concrete file.
    pub fn with_path(self, path: impl Into<PathBuf>) -> Self {
        match self {
            Self::Contextualized { .. } => self,
            other => Self::Contextualized {
                path: path.into(),
                source: Box::new(other),
            },
        }
    }

    /// The file this error was attributed to, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Contextualized { path, .. } => Some(path.as_path()),
            _ => None,
        }
    }

    /// True when this error reflects corrupt *committed data* (a malformed
    /// `.dlog`/`.dmap`: bad YAML/JSON, a schema violation, or a dmap parse
    /// failure) rather than an environmental/IO problem. Whole-repo commands
    /// skip-and-report these per file instead of aborting (audit blocker B5);
    /// genuine IO errors still propagate because they usually indicate a
    /// systemic problem, not one bad file.
    pub fn is_corrupt_data(&self) -> bool {
        match self {
            Self::Yaml { .. } | Self::Json { .. } | Self::Schema { .. } | Self::Dmap { .. } => true,
            Self::Contextualized { source, .. } => source.is_corrupt_data(),
            _ => false,
        }
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::Io {
                path,
                action,
                source,
            } => match path {
                Some(path) => format!("Failed to {action} {}: {source}", path.display()),
                None => format!("Failed to {action}: {source}"),
            },
            Self::FileTooLarge { path, limit } => format!(
                "{} exceeds configured byte limit of {} bytes",
                path.display(),
                limit
            ),
            Self::InvalidPath { input, reason } => {
                format!("Invalid project-relative path {:?}: {}", input, reason)
            }
            Self::Json { message, .. } => message.clone(),
            Self::Yaml { message, .. } => message.clone(),
            Self::Schema { field, message } if field.is_empty() => message.clone(),
            Self::Schema { field, message } => format!("{field}: {message}"),
            Self::Cli { message }
            | Self::Anchor { message }
            | Self::Git { message }
            | Self::Dmap { message } => message.clone(),
            Self::Mcp { message, .. } => message.clone(),
            Self::Contextualized { path, source } => {
                format!("{}: {}", path.display(), source.user_message())
            }
        }
    }
}

impl fmt::Display for ArchivaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.user_message())
    }
}

impl Error for ArchivaError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Contextualized { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<JsonError> for ArchivaError {
    fn from(error: JsonError) -> Self {
        Self::Json {
            line: 0,
            column: error.offset() + 1,
            message: error.message().to_string(),
        }
    }
}

impl From<YamlError> for ArchivaError {
    fn from(error: YamlError) -> Self {
        Self::Yaml {
            line: error.line(),
            column: 0,
            message: error.message().to_string(),
        }
    }
}

impl From<DmapError> for ArchivaError {
    fn from(error: DmapError) -> Self {
        Self::Dmap {
            message: error.to_string(),
        }
    }
}

impl From<PathError> for ArchivaError {
    fn from(error: PathError) -> Self {
        Self::InvalidPath {
            input: error.input().to_string(),
            reason: error.kind().as_reason(),
        }
    }
}

pub trait PathErrorReason {
    fn as_reason(&self) -> &'static str;
}

impl PathErrorReason for crate::core::paths::PathErrorKind {
    fn as_reason(&self) -> &'static str {
        match self {
            Self::Empty => "path is empty",
            Self::ContainsNul => "path contains a NUL byte",
            Self::Absolute => "absolute paths are not allowed",
            Self::DrivePrefix => "drive-prefix paths are not allowed",
            Self::UncOrDevicePrefix => "UNC or device-prefix paths are not allowed",
            Self::Backslash => "backslashes are not allowed",
            Self::DotSegment => "dot path segments are not allowed",
            Self::ParentSegment => "parent path segments are not allowed",
            Self::EmptySegment => "empty path segments are not allowed",
            Self::WindowsInvalidCharacter => "Windows-invalid path characters are not allowed",
            Self::WindowsTrailingName => "path segments cannot end with a space or dot",
            Self::WindowsReservedName => "Windows reserved names are not allowed",
            Self::EscapesProjectRoot => "path resolves outside the project root",
            Self::Io => "path validation failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ArchivaError, Result};
    use crate::core::json;
    use crate::core::paths::RelativePath;

    #[test]
    fn renders_user_facing_messages() {
        let error = ArchivaError::schema(
            "decisions.fn:next.lines_hint",
            "expected two positive integers",
        );
        assert_eq!(
            error.user_message(),
            "decisions.fn:next.lines_hint: expected two positive integers"
        );
        assert_eq!(
            ArchivaError::cli("Missing file path").to_string(),
            "Missing file path"
        );
    }

    #[test]
    fn converts_parser_and_path_errors() {
        let json_error: ArchivaError = json::parse("{bad").unwrap_err().into();
        assert_eq!(json_error.user_message(), "Expected object key string");

        let path_error: ArchivaError = RelativePath::new("../outside.ts").unwrap_err().into();
        assert_eq!(
            path_error.user_message(),
            "Invalid project-relative path \"../outside.ts\": parent path segments are not allowed"
        );
    }

    #[test]
    fn exposes_result_alias() -> Result<()> {
        Ok(())
    }
}
