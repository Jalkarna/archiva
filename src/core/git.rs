use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::error::{ArchivaError, Result};
use crate::core::fs::read_text_file_with_limit;
use crate::core::paths::RelativePath;

const GIT_OUTPUT_MAX_BYTES: usize = 10 * 1024 * 1024;
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const PIPE_READ_CHUNK_BYTES: usize = 8 * 1024;
const GIT_MARKER_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct BoundedCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct BoundedPipeRead {
    bytes: Vec<u8>,
    exceeded: bool,
}

pub fn find_git_root(start_dir: &Path) -> Result<Option<PathBuf>> {
    let mut dir = start_dir.canonicalize().map_err(|source| {
        ArchivaError::io(
            Some(start_dir.to_path_buf()),
            "resolve git search root",
            source,
        )
    })?;

    loop {
        if has_git_work_tree_marker(&dir) {
            return Ok(Some(dir));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

pub fn read_git_head_file(project_root: &Path, file: &RelativePath) -> Result<String> {
    let git_root = find_git_root(project_root)?.ok_or_else(|| ArchivaError::Git {
        message: "Not a git repository".to_string(),
    })?;
    let project_root = canonical_project_root(project_root)?;
    let relative_to_git = project_file_to_git_relative(&project_root, &git_root, file)?;

    let mut command = Command::new("git");
    command
        .args(["show", &format!("HEAD:{relative_to_git}")])
        .current_dir(&git_root);
    let output = run_command_bounded(
        &mut command,
        "run git show",
        GIT_OUTPUT_MAX_BYTES,
        GIT_OUTPUT_MAX_BYTES,
        GIT_COMMAND_TIMEOUT,
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ArchivaError::Git {
            message: if stderr.is_empty() {
                format!("git show HEAD:{relative_to_git} failed")
            } else {
                stderr
            },
        });
    }

    String::from_utf8(output.stdout).map_err(|source| ArchivaError::Git {
        message: format!("git show HEAD:{relative_to_git} returned non-UTF-8 output: {source}"),
    })
}

pub fn git_renamed_from(project_root: &Path, file: &RelativePath) -> Result<Option<RelativePath>> {
    let Some(git_root) = find_git_root(project_root)? else {
        return Ok(None);
    };
    let project_root = canonical_project_root(project_root)?;
    let target = project_file_to_git_relative(&project_root, &git_root, file)?;

    let mut command = Command::new("git");
    command
        .args(["status", "--porcelain=v1", "-z", "--find-renames"])
        .current_dir(&git_root);
    let Ok(output) = run_command_bounded(
        &mut command,
        "run git status",
        GIT_OUTPUT_MAX_BYTES,
        GIT_OUTPUT_MAX_BYTES,
        GIT_COMMAND_TIMEOUT,
    ) else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }

    let Some(old_git_relative) = parse_porcelain_rename_source(&output.stdout, &target) else {
        return Ok(None);
    };
    git_relative_to_project_file(&project_root, &git_root, old_git_relative)
}

fn run_command_bounded(
    command: &mut Command,
    action: &'static str,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: Duration,
) -> Result<BoundedCommandOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|source| ArchivaError::io(None, action, source))?;
    let stdout = child.stdout.take().ok_or_else(|| ArchivaError::Git {
        message: format!("{action} did not provide stdout"),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| ArchivaError::Git {
        message: format!("{action} did not provide stderr"),
    })?;
    let stdout_reader = thread::spawn(move || read_pipe_with_limit(stdout, stdout_limit));
    let stderr_reader = thread::spawn(move || read_pipe_with_limit(stderr, stderr_limit));
    let started = Instant::now();

    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|source| ArchivaError::io(None, action, source))?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = join_pipe_reader(stdout_reader, action, "stdout");
            let _ = join_pipe_reader(stderr_reader, action, "stderr");
            return Err(ArchivaError::Git {
                message: format!("{action} timed out after {}s", timeout.as_secs()),
            });
        }
        thread::sleep(Duration::from_millis(10));
    };

    let stdout = join_pipe_reader(stdout_reader, action, "stdout")?;
    let stderr = join_pipe_reader(stderr_reader, action, "stderr")?;
    if stdout.exceeded {
        return Err(ArchivaError::Git {
            message: format!("{action} stdout exceeded {stdout_limit} bytes"),
        });
    }
    if stderr.exceeded {
        return Err(ArchivaError::Git {
            message: format!("{action} stderr exceeded {stderr_limit} bytes"),
        });
    }

    Ok(BoundedCommandOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

fn read_pipe_with_limit<R: Read>(mut reader: R, limit: usize) -> io::Result<BoundedPipeRead> {
    let mut bytes = Vec::new();
    let mut exceeded = false;
    let mut chunk = [0_u8; PIPE_READ_CHUNK_BYTES];
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        if remaining > 0 {
            let stored = remaining.min(count);
            bytes.extend_from_slice(&chunk[..stored]);
        }
        if count > remaining {
            exceeded = true;
        }
    }
    Ok(BoundedPipeRead { bytes, exceeded })
}

fn join_pipe_reader(
    handle: thread::JoinHandle<io::Result<BoundedPipeRead>>,
    action: &'static str,
    stream: &'static str,
) -> Result<BoundedPipeRead> {
    handle
        .join()
        .map_err(|_| ArchivaError::Git {
            message: format!("{action} {stream} reader panicked"),
        })?
        .map_err(|source| ArchivaError::io(None, action, source))
}

fn canonical_project_root(project_root: &Path) -> Result<PathBuf> {
    project_root.canonicalize().map_err(|source| {
        ArchivaError::io(
            Some(project_root.to_path_buf()),
            "resolve project root",
            source,
        )
    })
}

fn project_file_to_git_relative(
    project_root: &Path,
    git_root: &Path,
    file: &RelativePath,
) -> Result<String> {
    let absolute_source = project_root.join(file.to_path_buf());
    let relative_to_git =
        absolute_source
            .strip_prefix(git_root)
            .map_err(|_| ArchivaError::Git {
                message: format!("File {:?} is outside the git repository", file.as_str()),
            })?;
    path_to_forward_slashes(relative_to_git).ok_or_else(|| ArchivaError::Git {
        message: format!(
            "File {:?} is not valid UTF-8 relative to the git repository",
            file.as_str()
        ),
    })
}

fn git_relative_to_project_file(
    project_root: &Path,
    git_root: &Path,
    git_relative: &str,
) -> Result<Option<RelativePath>> {
    let absolute = git_root.join(path_from_forward_slashes(git_relative));
    let Ok(relative) = absolute.strip_prefix(project_root) else {
        return Ok(None);
    };
    let Some(relative) = path_to_forward_slashes(relative) else {
        return Ok(None);
    };
    Ok(Some(RelativePath::new(&relative)?))
}

fn parse_porcelain_rename_source<'a>(output: &'a [u8], target: &str) -> Option<&'a str> {
    let target = target.as_bytes();
    let mut fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue;
        }
        let status = &entry[0..2];
        if status.contains(&b'R') {
            let path = &entry[3..];
            let old_path = fields.next()?;
            if path == target {
                return std::str::from_utf8(old_path).ok();
            }
        }
    }
    None
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

fn path_from_forward_slashes(path: &str) -> PathBuf {
    path.split('/').collect()
}

fn has_git_work_tree_marker(dir: &Path) -> bool {
    let marker = dir.join(".git");
    let metadata = match fs::metadata(&marker) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if metadata.is_dir() {
        return marker.join("HEAD").is_file();
    }
    if metadata.is_file() {
        return read_text_file_with_limit(&marker, GIT_MARKER_MAX_BYTES, "read git marker")
            .map(|content| {
                content
                    .lines()
                    .next()
                    .map(|line| line.trim_start().starts_with("gitdir:"))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{
        find_git_root, git_renamed_from, has_git_work_tree_marker, parse_porcelain_rename_source,
        read_git_head_file, run_command_bounded, GIT_OUTPUT_MAX_BYTES,
    };
    use crate::core::paths::RelativePath;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    #[test]
    fn returns_none_when_no_git_repository_exists() {
        let root = unique_temp_dir("archiva-git-none");
        fs::create_dir_all(&root).unwrap();
        assert_eq!(find_git_root(&root).unwrap(), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ignores_invalid_git_marker_directory() {
        let root = unique_temp_dir("archiva-git-invalid-marker");
        fs::create_dir_all(root.join(".git")).unwrap();
        assert!(!has_git_work_tree_marker(&root));
        assert_eq!(find_git_root(&root).unwrap(), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_file_from_head_relative_to_git_root() {
        let root = unique_temp_dir("archiva-git-head");
        fs::create_dir_all(root.join("pkg").join("src")).unwrap();
        fs::write(root.join("pkg").join("src").join("a.ts"), "initial\n").unwrap();
        git(&root, &["init"]);
        git(&root, &["add", "pkg/src/a.ts"]);
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
        fs::write(root.join("pkg").join("src").join("a.ts"), "changed\n").unwrap();

        assert_eq!(
            read_git_head_file(&root.join("pkg"), &RelativePath::new("src/a.ts").unwrap()).unwrap(),
            "initial\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn detects_git_status_rename_source_for_project_file() {
        let root = unique_temp_dir("archiva-git-rename");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("old.ts"), "function kept() {}\n").unwrap();
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
        git(&root, &["mv", "src/old.ts", "src/new.ts"]);

        assert_eq!(
            git_renamed_from(&root, &RelativePath::new("src/new.ts").unwrap()).unwrap(),
            Some(RelativePath::new("src/old.ts").unwrap())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bounded_git_command_rejects_stdout_beyond_limit_without_losing_stderr() {
        let mut command = Command::new("git");
        command.arg("--version");

        let error = run_command_bounded(
            &mut command,
            "run git --version",
            4,
            GIT_OUTPUT_MAX_BYTES,
            Duration::from_secs(5),
        )
        .unwrap_err()
        .user_message();

        assert!(error.contains("stdout exceeded 4 bytes"));
    }

    #[test]
    fn parses_nul_porcelain_rename_records_with_spaces() {
        let output = b"R  src/new file.ts\0src/old file.ts\0 M src/other.ts\0";
        assert_eq!(
            parse_porcelain_rename_source(output, "src/new file.ts"),
            Some("src/old file.ts")
        );
        assert_eq!(parse_porcelain_rename_source(output, "src/other.ts"), None);
    }

    #[test]
    fn skips_unrelated_non_utf8_porcelain_rename_records() {
        let output = b"R  src/bad-\xff.ts\0src/old-\xff.ts\0R  src/new.ts\0src/old.ts\0";
        assert_eq!(
            parse_porcelain_rename_source(output, "src/new.ts"),
            Some("src/old.ts")
        );
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
}
