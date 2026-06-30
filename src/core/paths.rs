use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::core::error::{ArchivaError, Result as ArchivaResult};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelativePath(String);

impl RelativePath {
    pub fn new(input: &str) -> Result<Self, PathError> {
        Ok(Self(normalize_relative_path(input)?))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_path_buf(&self) -> PathBuf {
        let mut path = PathBuf::new();
        for segment in self.0.split('/') {
            path.push(segment);
        }
        path
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathErrorKind {
    Empty,
    ContainsNul,
    Absolute,
    DrivePrefix,
    UncOrDevicePrefix,
    Backslash,
    DotSegment,
    ParentSegment,
    EmptySegment,
    WindowsInvalidCharacter,
    WindowsTrailingName,
    WindowsReservedName,
    EscapesProjectRoot,
    Io,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathError {
    kind: PathErrorKind,
    input: String,
    detail: Option<String>,
}

impl PathError {
    fn new(kind: PathErrorKind, input: impl Into<String>) -> Self {
        Self {
            kind,
            input: input.into(),
            detail: None,
        }
    }

    fn io(input: impl Into<String>, error: std::io::Error) -> Self {
        Self {
            kind: PathErrorKind::Io,
            input: input.into(),
            detail: Some(error.to_string()),
        }
    }

    pub fn kind(&self) -> &PathErrorKind {
        &self.kind
    }

    pub fn input(&self) -> &str {
        &self.input
    }
}

impl fmt::Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(detail) = &self.detail {
            write!(
                formatter,
                "Invalid project-relative path {:?}: {}",
                self.input, detail
            )
        } else {
            write!(
                formatter,
                "Invalid project-relative path {:?}: {:?}",
                self.input, self.kind
            )
        }
    }
}

impl Error for PathError {}

pub fn source_path(project_root: &Path, relative: &RelativePath) -> PathBuf {
    project_root.join(relative.to_path_buf())
}

pub fn decision_base_path(project_root: &Path, relative: &RelativePath) -> PathBuf {
    project_root.join(".decisions").join(relative.to_path_buf())
}

pub fn dlog_path(project_root: &Path, relative: &RelativePath) -> PathBuf {
    with_extension_suffix(decision_base_path(project_root, relative), "dlog")
}

pub fn dmap_path(project_root: &Path, relative: &RelativePath) -> PathBuf {
    with_extension_suffix(decision_base_path(project_root, relative), "dmap")
}

pub fn decision_lock_path(project_root: &Path, relative: &RelativePath) -> PathBuf {
    with_extension_suffix(decision_base_path(project_root, relative), "lock")
}

pub fn source_path_from_decision_file(
    project_root: &Path,
    decision_file_path: &Path,
) -> ArchivaResult<RelativePath> {
    let decisions_root = project_root.join(".decisions");
    let relative = decision_file_path
        .strip_prefix(&decisions_root)
        .map_err(|_| {
            ArchivaError::cli(format!(
                "Decision file {} is not under {}",
                decision_file_path.display(),
                decisions_root.display()
            ))
        })?;
    let mut normalized = path_to_forward_slashes(relative)?;
    if let Some(stripped) = normalized.strip_suffix(".dlog") {
        normalized = stripped.to_string();
    } else if let Some(stripped) = normalized.strip_suffix(".dmap") {
        normalized = stripped.to_string();
    }
    Ok(RelativePath::new(&normalized)?)
}

pub fn canonical_source_path_if_exists(
    project_root: &Path,
    relative: &RelativePath,
) -> Result<PathBuf, PathError> {
    let root = project_root
        .canonicalize()
        .map_err(|error| PathError::io(project_root.display().to_string(), error))?;
    let source = root.join(relative.to_path_buf());
    if !source
        .try_exists()
        .map_err(|error| PathError::io(relative.as_str(), error))?
    {
        return Ok(source);
    }
    let canonical = source
        .canonicalize()
        .map_err(|error| PathError::io(relative.as_str(), error))?;
    if canonical.starts_with(&root) {
        Ok(canonical)
    } else {
        Err(PathError::new(
            PathErrorKind::EscapesProjectRoot,
            relative.as_str(),
        ))
    }
}

fn normalize_relative_path(input: &str) -> Result<String, PathError> {
    if input.is_empty() {
        return Err(PathError::new(PathErrorKind::Empty, input));
    }
    if input.contains('\0') {
        return Err(PathError::new(PathErrorKind::ContainsNul, input));
    }
    if starts_with_unc_or_device_prefix(input) {
        return Err(PathError::new(PathErrorKind::UncOrDevicePrefix, input));
    }
    if starts_with_drive_prefix(input) {
        return Err(PathError::new(PathErrorKind::DrivePrefix, input));
    }

    let mut normalized = input.replace('\\', "/");
    while normalized.starts_with("./") {
        let slash_count = normalized[1..]
            .bytes()
            .take_while(|byte| *byte == b'/')
            .count();
        normalized = normalized[1 + slash_count..].to_string();
    }
    if normalized.is_empty() {
        return Err(PathError::new(PathErrorKind::Empty, input));
    }
    if normalized.starts_with('/') {
        return Err(PathError::new(PathErrorKind::Absolute, input));
    }

    for segment in normalized.split('/') {
        if segment.is_empty() {
            return Err(PathError::new(PathErrorKind::EmptySegment, input));
        }
        if segment == "." {
            return Err(PathError::new(PathErrorKind::DotSegment, input));
        }
        if segment == ".." {
            return Err(PathError::new(PathErrorKind::ParentSegment, input));
        }
        if has_windows_invalid_character(segment) {
            return Err(PathError::new(
                PathErrorKind::WindowsInvalidCharacter,
                input,
            ));
        }
        if segment.ends_with([' ', '.']) {
            return Err(PathError::new(PathErrorKind::WindowsTrailingName, input));
        }
        if is_windows_reserved_name(segment) {
            return Err(PathError::new(PathErrorKind::WindowsReservedName, input));
        }
    }

    Ok(normalized)
}

fn starts_with_drive_prefix(input: &str) -> bool {
    let bytes = input.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn starts_with_unc_or_device_prefix(input: &str) -> bool {
    input.starts_with("//") || input.starts_with("\\\\") || input.starts_with("\\\\?\\")
}

fn is_windows_reserved_name(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || (stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn has_windows_invalid_character(segment: &str) -> bool {
    segment
        .chars()
        .any(|character| matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
}

fn with_extension_suffix(mut path: PathBuf, suffix: &str) -> PathBuf {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return path;
    };
    let file_name = format!("{file_name}.{suffix}");
    path.set_file_name(file_name);
    path
}

fn path_to_forward_slashes(path: &Path) -> ArchivaResult<String> {
    let mut segments = Vec::new();
    for component in path.components() {
        let Some(segment) = component.as_os_str().to_str() else {
            return Err(ArchivaError::cli(format!(
                "Decision file path {} is not valid UTF-8",
                path.display()
            )));
        };
        segments.push(segment);
    }
    Ok(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_source_path_if_exists, decision_base_path, decision_lock_path, dlog_path,
        dmap_path, source_path, source_path_from_decision_file, PathErrorKind, RelativePath,
    };
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn accepts_stable_project_relative_paths() {
        let relative = RelativePath::new("Src/Feature.test.ts").unwrap();
        assert_eq!(relative.as_str(), "Src/Feature.test.ts");
        assert_eq!(
            relative.to_path_buf(),
            PathBuf::from("Src").join("Feature.test.ts")
        );
        assert_eq!(
            source_path(PathBuf::from("/repo").as_path(), &relative),
            PathBuf::from("/repo").join("Src").join("Feature.test.ts")
        );
        assert_eq!(
            decision_base_path(PathBuf::from("/repo").as_path(), &relative),
            PathBuf::from("/repo")
                .join(".decisions")
                .join("Src")
                .join("Feature.test.ts")
        );
        assert_eq!(
            dlog_path(PathBuf::from("/repo").as_path(), &relative),
            PathBuf::from("/repo")
                .join(".decisions")
                .join("Src")
                .join("Feature.test.ts.dlog")
        );
        assert_eq!(
            decision_lock_path(PathBuf::from("/repo").as_path(), &relative),
            PathBuf::from("/repo")
                .join(".decisions")
                .join("Src")
                .join("Feature.test.ts.lock")
        );
        assert_eq!(
            source_path_from_decision_file(
                PathBuf::from("/repo").as_path(),
                PathBuf::from("/repo")
                    .join(".decisions")
                    .join("Src")
                    .join("Feature.test.ts.dlog")
                    .as_path()
            )
            .unwrap(),
            relative
        );
    }

    #[test]
    fn normalizes_common_tool_supplied_relative_paths() {
        let cases = [
            ("./src/a.ts", "src/a.ts"),
            (".//src/a.ts", "src/a.ts"),
            ("src\\a.ts", "src/a.ts"),
            (".\\src\\a.ts", "src/a.ts"),
        ];

        for (input, expected) in cases {
            let relative = RelativePath::new(input).unwrap();
            assert_eq!(relative.as_str(), expected, "{input}");
            assert_eq!(
                relative.to_path_buf(),
                PathBuf::from("src").join("a.ts"),
                "{input}"
            );
        }
    }

    #[test]
    fn maps_decision_files_back_to_source_paths_without_suffix_confusion() {
        let root = PathBuf::from("/repo");

        let source = RelativePath::new("src/a.ts").unwrap();
        assert_eq!(
            source_path_from_decision_file(&root, dlog_path(&root, &source).as_path()).unwrap(),
            source
        );

        let dlog_named_source = RelativePath::new("src/schema.dlog").unwrap();
        assert_eq!(
            source_path_from_decision_file(&root, dlog_path(&root, &dlog_named_source).as_path())
                .unwrap(),
            dlog_named_source
        );

        let dmap_named_source = RelativePath::new("src/schema.dmap").unwrap();
        assert_eq!(
            source_path_from_decision_file(&root, dmap_path(&root, &dmap_named_source).as_path())
                .unwrap(),
            dmap_named_source
        );

        let outside = source_path_from_decision_file(
            &root,
            PathBuf::from("/else")
                .join(".decisions")
                .join("src")
                .join("a.ts.dlog")
                .as_path(),
        )
        .unwrap_err();
        assert!(outside
            .user_message()
            .contains("is not under /repo/.decisions"));
    }

    #[test]
    fn rejects_paths_that_typescript_currently_allows_but_v2_hardens() {
        let cases = [
            ("", PathErrorKind::Empty),
            ("a\0b.ts", PathErrorKind::ContainsNul),
            ("/tmp/outside.ts", PathErrorKind::Absolute),
            ("//server/share/file.ts", PathErrorKind::UncOrDevicePrefix),
            (
                "\\\\server\\share\\file.ts",
                PathErrorKind::UncOrDevicePrefix,
            ),
            ("C:/repo/file.ts", PathErrorKind::DrivePrefix),
            ("C:\\repo\\file.ts", PathErrorKind::DrivePrefix),
            ("src/./a.ts", PathErrorKind::DotSegment),
            ("src/../a.ts", PathErrorKind::ParentSegment),
            ("src//a.ts", PathErrorKind::EmptySegment),
            ("src/", PathErrorKind::EmptySegment),
            ("src/a:b.ts", PathErrorKind::WindowsInvalidCharacter),
            ("src/a?.ts", PathErrorKind::WindowsInvalidCharacter),
            ("src/a*.ts", PathErrorKind::WindowsInvalidCharacter),
            ("src/name.", PathErrorKind::WindowsTrailingName),
            ("src/name ", PathErrorKind::WindowsTrailingName),
            ("src/name /file.ts", PathErrorKind::WindowsTrailingName),
            ("CON.ts", PathErrorKind::WindowsReservedName),
            ("src/NUL.txt", PathErrorKind::WindowsReservedName),
            ("COM1", PathErrorKind::WindowsReservedName),
            ("LPT9.log", PathErrorKind::WindowsReservedName),
        ];

        for (input, expected) in cases {
            assert_eq!(
                RelativePath::new(input).unwrap_err().kind(),
                &expected,
                "{input}"
            );
        }
    }

    #[test]
    fn keeps_missing_paths_lexical_after_validation() {
        let root = unique_temp_dir("archiva-paths-missing");
        fs::create_dir_all(&root).unwrap();
        let relative = RelativePath::new("src/missing.ts").unwrap();
        let resolved = canonical_source_path_if_exists(&root, &relative).unwrap();
        assert_eq!(
            resolved,
            root.canonicalize().unwrap().join("src").join("missing.ts")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_existing_symlinks_that_escape_the_project_root() {
        use std::os::unix::fs::symlink;

        let root = unique_temp_dir("archiva-paths-root");
        let outside = unique_temp_dir("archiva-paths-outside");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(root.join("src").join("inside.ts"), "export const x = 1;\n").unwrap();
        fs::write(outside.join("outside.ts"), "export const x = 2;\n").unwrap();
        symlink(
            root.join("src").join("inside.ts"),
            root.join("inside-link.ts"),
        )
        .unwrap();
        symlink(outside.join("outside.ts"), root.join("outside-link.ts")).unwrap();

        let inside = RelativePath::new("inside-link.ts").unwrap();
        assert!(canonical_source_path_if_exists(&root, &inside)
            .unwrap()
            .starts_with(root.canonicalize().unwrap()));

        let outside_link = RelativePath::new("outside-link.ts").unwrap();
        assert_eq!(
            canonical_source_path_if_exists(&root, &outside_link)
                .unwrap_err()
                .kind(),
            &PathErrorKind::EscapesProjectRoot
        );

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
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
