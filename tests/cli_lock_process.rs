use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use archiva::core::dmap::DecisionStatus;
use archiva::core::fs::acquire_file_lock_now;
use archiva::core::paths::{decision_lock_path, dlog_path, dmap_path, RelativePath};
use archiva::core::storage::load_dlog;

static PROCESS_LOCK_TEST_MUTEX: Mutex<()> = Mutex::new(());

#[test]
fn concurrent_cli_write_decisions_serialize_across_processes() {
    let _serial = serialize_process_lock_test();
    let root = unique_temp_dir("archiva-cli-process-lock");
    let source_path = root.join("src").join("concurrent.ts");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let writers = 6_usize;
    let source = (0..writers)
        .map(|index| format!("export function fn{index}() {{\n  return {index};\n}}\n"))
        .collect::<String>();
    fs::write(&source_path, source).unwrap();

    let root = Arc::new(root);
    let barrier = Arc::new(Barrier::new(writers));
    let handles = (0..writers)
        .map(|index| {
            let root = Arc::clone(&root);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                let output = Command::new(env!("CARGO_BIN_EXE_archiva"))
                    .args(["write-decision", "--json", &decision_json(index)])
                    .current_dir(root.as_ref())
                    .output()
                    .unwrap();
                (
                    index,
                    output.status.code(),
                    String::from_utf8(output.stdout).unwrap(),
                    String::from_utf8(output.stderr).unwrap(),
                )
            })
        })
        .collect::<Vec<_>>();

    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    for (index, status, stdout, stderr) in &results {
        assert_eq!(
            *status,
            Some(0),
            "writer {index} failed stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(stdout.starts_with("Recorded dec_"));
        assert_eq!(stderr, "");
    }

    let file = RelativePath::new("src/concurrent.ts").unwrap();
    let stored = load_dlog(&root, &file).unwrap().unwrap();
    assert_eq!(stored.decisions.len(), writers);
    let ids = stored
        .decisions
        .iter()
        .map(|(_, decision)| decision.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), writers);
    for index in 0..writers {
        assert!(stored.decisions.get_str(&format!("fn:fn{index}")).is_some());
    }
    assert_eq!(
        fs::read_to_string(dmap_path(&root, &file))
            .unwrap()
            .lines()
            .count(),
        writers
    );
    assert!(!decision_lock_path(&root, &file).exists());
    assert!(temp_siblings(dlog_path(&root, &file).parent().unwrap()).is_empty());

    let _ = fs::remove_dir_all(root.as_ref());
}

#[test]
fn concurrent_cli_write_decisions_recover_one_stale_lock_and_serialize() {
    let _serial = serialize_process_lock_test();
    let root = unique_temp_dir("archiva-cli-stale-process-lock");
    let source_path = root.join("src").join("stale.ts");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    let writers = 6_usize;
    let source = (0..writers)
        .map(|index| format!("export function stale{index}() {{\n  return {index};\n}}\n"))
        .collect::<String>();
    fs::write(&source_path, source).unwrap();

    let file = RelativePath::new("src/stale.ts").unwrap();
    let lock_path = decision_lock_path(&root, &file);
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    fs::write(
        &lock_path,
        "version=1\npid=4294967295\ntoken=stale\ncommand=crashed\ntimestamp=2000-01-01T00:00:00.000Z\n",
    )
    .unwrap();

    let root = Arc::new(root);
    let barrier = Arc::new(Barrier::new(writers));
    let handles = (0..writers)
        .map(|index| {
            let root = Arc::clone(&root);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                let json = stale_decision_json(index);
                let output = Command::new(env!("CARGO_BIN_EXE_archiva"))
                    .args(["write-decision", "--json", &json])
                    .current_dir(root.as_ref())
                    .output()
                    .unwrap();
                (
                    index,
                    output.status.code(),
                    String::from_utf8(output.stdout).unwrap(),
                    String::from_utf8(output.stderr).unwrap(),
                )
            })
        })
        .collect::<Vec<_>>();

    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    for (index, status, stdout, stderr) in &results {
        assert_eq!(
            *status,
            Some(0),
            "stale writer {index} failed stdout={stdout:?} stderr={stderr:?}"
        );
        assert!(stdout.starts_with("Recorded dec_"));
        assert_eq!(stderr, "");
    }

    let stored = load_dlog(&root, &file).unwrap().unwrap();
    assert_eq!(stored.decisions.len(), writers);
    for index in 0..writers {
        assert!(stored
            .decisions
            .get_str(&format!("fn:stale{index}"))
            .is_some());
    }
    assert!(!lock_path.exists());
    assert!(!recovery_lock_path(&lock_path).exists());
    assert!(temp_siblings(dlog_path(&root, &file).parent().unwrap()).is_empty());

    let _ = fs::remove_dir_all(root.as_ref());
}

#[test]
fn write_decision_and_lint_serialize_across_processes() {
    run_mixed_lock_race("lint");
}

#[test]
fn write_decision_and_post_tool_use_serialize_across_processes() {
    run_mixed_lock_race("post-tool-use");
}

#[test]
fn write_decision_and_status_serialize_across_processes() {
    run_write_status_race();
}

#[test]
fn write_decision_and_lint_fix_serialize_across_processes() {
    run_write_lint_fix_race();
}

#[test]
fn lint_and_post_tool_use_serialize_across_processes() {
    run_stale_mutator_pair_race(
        "lint-post-tool-use",
        RaceCommand::Lint,
        RaceCommand::PostToolUse,
    );
}

#[test]
fn concurrent_lint_commands_serialize_across_processes() {
    run_stale_mutator_pair_race("lint-lint", RaceCommand::Lint, RaceCommand::Lint);
}

#[test]
fn concurrent_post_tool_use_commands_serialize_across_processes() {
    run_stale_mutator_pair_race(
        "post-tool-use-post-tool-use",
        RaceCommand::PostToolUse,
        RaceCommand::PostToolUse,
    );
}

fn run_mixed_lock_race(mode: &str) {
    let root = unique_temp_dir(&format!("archiva-cli-mixed-{mode}"));
    let file = RelativePath::new("src/mixed.ts").unwrap();
    let source_path = root.join("src").join("mixed.ts");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(
        &source_path,
        "export function kept() {\n  return 1;\n}\n\nexport function added() {\n  return 2;\n}\n",
    )
    .unwrap();

    let initial = run_archiva(
        &root,
        &[
            "write-decision",
            "--json",
            r#"{"file":"src/mixed.ts","anchor":"fn:kept","lines":[1,3],"chose":"record kept","because":"mixed process fixture","rejected":[]}"#,
        ],
    );
    assert_command_success(&initial, "initial write-decision");
    fs::write(
        &source_path,
        "export function kept() {\n  return 10;\n}\n\nexport function added() {\n  return 2;\n}\n",
    )
    .unwrap();

    let lock_path = decision_lock_path(&root, &file);
    let lock = acquire_file_lock_now(&lock_path, "integration-test").unwrap();
    let root_for_write = root.clone();
    let write_handle = thread::spawn(move || {
        run_archiva(
            &root_for_write,
            &[
                "write-decision",
                "--json",
                r#"{"file":"src/mixed.ts","anchor":"fn:added","lines":[5,7],"chose":"record added","because":"mixed process fixture","rejected":[]}"#,
            ],
        )
    });
    let root_for_mutator = root.clone();
    let mode = mode.to_string();
    let mutator_mode = mode.clone();
    let mutator_handle = thread::spawn(move || match mutator_mode.as_str() {
        "lint" => run_archiva(&root_for_mutator, &["lint"]),
        "post-tool-use" => run_archiva(
            &root_for_mutator,
            &["hooks", "post-tool-use", "src/mixed.ts"],
        ),
        _ => unreachable!(),
    });

    thread::sleep(Duration::from_millis(200));
    lock.release().unwrap();

    let write = write_handle.join().unwrap();
    let mutator = mutator_handle.join().unwrap();
    assert_command_success(&write, "concurrent write-decision");
    match mode.as_str() {
        "lint" => {
            assert_eq!(
                mutator.status,
                Some(1),
                "lint stdout={:?} stderr={:?}",
                mutator.stdout,
                mutator.stderr
            );
            assert!(mutator.stdout.contains("arc/stale"));
            assert_eq!(mutator.stderr, "");
        }
        "post-tool-use" => {
            assert_command_success(&mutator, "concurrent post-tool-use");
            assert_eq!(
                mutator.stdout,
                "Re-anchored src/mixed.ts: 1 stale, 0 orphan.\n"
            );
        }
        _ => unreachable!(),
    }

    let stored = load_dlog(&root, &file).unwrap().unwrap();
    assert_eq!(stored.decisions.len(), 2);
    let kept = stored.decisions.get_str("fn:kept").unwrap();
    let added = stored.decisions.get_str("fn:added").unwrap();
    assert_eq!(kept.status, Some(DecisionStatus::Stale));
    assert!(kept.stale_since.is_some());
    assert_eq!(added.status, None);
    assert_eq!(added.stale_since, None);
    assert_eq!(
        fs::read_to_string(dmap_path(&root, &file)).unwrap(),
        "1-3:fn:kept:STALE\n5-7:fn:added\n"
    );
    assert!(!lock_path.exists());
    assert!(temp_siblings(dlog_path(&root, &file).parent().unwrap()).is_empty());

    let _ = fs::remove_dir_all(root);
}

#[derive(Clone, Copy)]
enum RaceCommand {
    Lint,
    PostToolUse,
}

fn run_stale_mutator_pair_race(name: &str, first: RaceCommand, second: RaceCommand) {
    let root = unique_temp_dir(&format!("archiva-cli-mutator-{name}"));
    let file = RelativePath::new("src/mutator-race.ts").unwrap();
    let source_path = root.join("src").join("mutator-race.ts");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(&source_path, "export function kept() {\n  return 1;\n}\n").unwrap();

    assert_command_success(
        &run_archiva(
            &root,
            &[
                "write-decision",
                "--json",
                r#"{"file":"src/mutator-race.ts","anchor":"fn:kept","lines":[1,3],"chose":"record kept","because":"same-file mutator fixture","rejected":[]}"#,
            ],
        ),
        "initial write-decision",
    );
    fs::write(&source_path, "export function kept() {\n  return 10;\n}\n").unwrap();

    let lock_path = decision_lock_path(&root, &file);
    let lock = acquire_file_lock_now(&lock_path, "integration-test").unwrap();
    let root_for_first = root.clone();
    let first_handle = thread::spawn(move || run_race_command(&root_for_first, first));
    let root_for_second = root.clone();
    let second_handle = thread::spawn(move || run_race_command(&root_for_second, second));

    thread::sleep(Duration::from_millis(200));
    lock.release().unwrap();

    let first_result = first_handle.join().unwrap();
    let second_result = second_handle.join().unwrap();
    assert_race_pair_results(first, &first_result, second, &second_result);

    let stored = load_dlog(&root, &file).unwrap().unwrap();
    assert_eq!(stored.decisions.len(), 1);
    let kept = stored.decisions.get_str("fn:kept").unwrap();
    assert_eq!(kept.status, Some(DecisionStatus::Stale));
    assert!(kept.stale_since.is_some());
    assert_eq!(
        fs::read_to_string(dmap_path(&root, &file)).unwrap(),
        "1-3:fn:kept:STALE\n"
    );
    assert!(!lock_path.exists());
    assert!(temp_siblings(dlog_path(&root, &file).parent().unwrap()).is_empty());

    let _ = fs::remove_dir_all(root);
}

fn run_write_status_race() {
    let root = unique_temp_dir("archiva-cli-mixed-status");
    let file = RelativePath::new("src/status-race.ts").unwrap();
    let source_path = root.join("src").join("status-race.ts");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(
        &source_path,
        "export function kept() {\n  return 1;\n}\n\nexport function added() {\n  return 2;\n}\n",
    )
    .unwrap();

    assert_command_success(
        &run_archiva(
            &root,
            &[
                "write-decision",
                "--json",
                r#"{"file":"src/status-race.ts","anchor":"fn:kept","lines":[1,3],"chose":"record kept","because":"mixed status fixture","rejected":[]}"#,
            ],
        ),
        "initial write-decision",
    );
    fs::write(
        &source_path,
        "export function kept() {\n  return 10;\n}\n\nexport function added() {\n  return 2;\n}\n",
    )
    .unwrap();

    let lock_path = decision_lock_path(&root, &file);
    let lock = acquire_file_lock_now(&lock_path, "integration-test").unwrap();
    let root_for_write = root.clone();
    let write_handle = thread::spawn(move || {
        run_archiva(
            &root_for_write,
            &[
                "write-decision",
                "--json",
                r#"{"file":"src/status-race.ts","anchor":"fn:added","lines":[5,7],"chose":"record added","because":"mixed status fixture","rejected":[]}"#,
            ],
        )
    });
    let root_for_status = root.clone();
    let status_handle = thread::spawn(move || run_archiva(&root_for_status, &["status"]));

    thread::sleep(Duration::from_millis(200));
    lock.release().unwrap();

    let write = write_handle.join().unwrap();
    let status = status_handle.join().unwrap();
    assert_command_success(&write, "concurrent write-decision");
    assert_command_success(&status, "concurrent status");
    assert_status_snapshot(&status.stdout);
    assert_eq!(status.stderr, "");

    assert_stale_kept_and_clean_added(&root, &file);
    assert!(!lock_path.exists());
    assert!(temp_siblings(dlog_path(&root, &file).parent().unwrap()).is_empty());

    let _ = fs::remove_dir_all(root);
}

fn run_write_lint_fix_race() {
    let root = unique_temp_dir("archiva-cli-mixed-lint-fix");
    let file = RelativePath::new("src/lint-fix-race.ts").unwrap();
    let source_path = root.join("src").join("lint-fix-race.ts");
    fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    fs::write(
        &source_path,
        "export function kept() {\n  return 1;\n}\n\nexport function gone() {\n  return 2;\n}\n\nexport function added() {\n  return 3;\n}\n",
    )
    .unwrap();

    assert_command_success(
        &run_archiva(
            &root,
            &[
                "write-decision",
                "--json",
                r#"{"file":"src/lint-fix-race.ts","anchor":"fn:gone","lines":[5,7],"chose":"record gone","because":"mixed lint fix fixture","rejected":[]}"#,
            ],
        ),
        "initial gone write-decision",
    );
    assert_command_success(
        &run_archiva(
            &root,
            &[
                "write-decision",
                "--json",
                r#"{"file":"src/lint-fix-race.ts","anchor":"fn:kept","lines":[1,3],"chose":"record kept","because":"mixed lint fix fixture","rejected":[]}"#,
            ],
        ),
        "initial kept write-decision",
    );
    fs::write(
        &source_path,
        "export function kept() {\n  return 1;\n}\n\nexport function added() {\n  return 3;\n}\n",
    )
    .unwrap();

    let lock_path = decision_lock_path(&root, &file);
    let lock = acquire_file_lock_now(&lock_path, "integration-test").unwrap();
    let root_for_write = root.clone();
    let write_handle = thread::spawn(move || {
        run_archiva(
            &root_for_write,
            &[
                "write-decision",
                "--json",
                r#"{"file":"src/lint-fix-race.ts","anchor":"fn:added","lines":[5,7],"chose":"record added","because":"mixed lint fix fixture","rejected":[]}"#,
            ],
        )
    });
    let root_for_lint = root.clone();
    let lint_handle = thread::spawn(move || run_archiva(&root_for_lint, &["lint", "--fix"]));

    thread::sleep(Duration::from_millis(200));
    lock.release().unwrap();

    let write = write_handle.join().unwrap();
    let lint = lint_handle.join().unwrap();
    assert_command_success(&write, "concurrent write-decision");
    assert_command_success(&lint, "concurrent lint --fix");
    assert!(lint.stdout.contains("arc/orphan"));
    assert_eq!(lint.stderr, "");

    let stored = load_dlog(&root, &file).unwrap().unwrap();
    assert_eq!(stored.decisions.len(), 2);
    assert!(stored.decisions.get_str("fn:gone").is_none());
    let kept = stored.decisions.get_str("fn:kept").unwrap();
    let added = stored.decisions.get_str("fn:added").unwrap();
    assert_eq!(kept.id, "dec_002");
    assert_eq!(added.id, "dec_003");
    assert_eq!(kept.status, None);
    assert_eq!(kept.stale_since, None);
    assert_eq!(added.status, None);
    assert_eq!(added.stale_since, None);
    assert_eq!(
        fs::read_to_string(dmap_path(&root, &file)).unwrap(),
        "1-3:fn:kept\n5-7:fn:added\n"
    );
    assert!(!lock_path.exists());
    assert!(temp_siblings(dlog_path(&root, &file).parent().unwrap()).is_empty());

    let _ = fs::remove_dir_all(root);
}

fn decision_json(index: usize) -> String {
    format!(
        "{{\"file\":\"src/concurrent.ts\",\"anchor\":\"fn:fn{index}\",\"lines\":[{},{}],\"chose\":\"record function {index}\",\"because\":\"multi-process lock fixture\",\"rejected\":[]}}",
        index * 3 + 1,
        index * 3 + 3
    )
}

fn stale_decision_json(index: usize) -> String {
    format!(
        "{{\"file\":\"src/stale.ts\",\"anchor\":\"fn:stale{index}\",\"lines\":[{},{}],\"chose\":\"record stale function {index}\",\"because\":\"stale lock process fixture\",\"rejected\":[]}}",
        index * 3 + 1,
        index * 3 + 3
    )
}

fn serialize_process_lock_test() -> MutexGuard<'static, ()> {
    PROCESS_LOCK_TEST_MUTEX.lock().unwrap()
}

fn recovery_lock_path(lock_path: &Path) -> PathBuf {
    let file_name = lock_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("archiva.lock"));
    let mut recovery_name = file_name;
    recovery_name.push(".recover");
    lock_path.with_file_name(recovery_name)
}

#[derive(Debug)]
struct CommandResult {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

const LINT_STALE_STDOUT: &str =
    "ERROR arc/stale src/mutator-race.ts fn:kept: fn:kept code fingerprint differs from recorded decision\n";
const LINT_STALE_SUPERSEDE_STDOUT: &str =
    "ERROR arc/stale src/mutator-race.ts fn:kept: fn:kept code fingerprint differs from recorded decision\nERROR arc/supersede src/mutator-race.ts fn:kept: fn:kept is stale and has not been superseded\n";
const POST_TOOL_USE_STDOUT: &str = "Re-anchored src/mutator-race.ts: 1 stale, 0 orphan.\n";

fn run_archiva(root: &Path, args: &[&str]) -> CommandResult {
    let output = Command::new(env!("CARGO_BIN_EXE_archiva"))
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    CommandResult {
        status: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn run_race_command(root: &Path, command: RaceCommand) -> CommandResult {
    match command {
        RaceCommand::Lint => run_archiva(root, &["lint"]),
        RaceCommand::PostToolUse => {
            run_archiva(root, &["hooks", "post-tool-use", "src/mutator-race.ts"])
        }
    }
}

fn assert_race_pair_results(
    first: RaceCommand,
    first_result: &CommandResult,
    second: RaceCommand,
    second_result: &CommandResult,
) {
    match (first, second) {
        (RaceCommand::Lint, RaceCommand::Lint) => {
            assert_lint_result(first_result);
            assert_lint_result(second_result);
            let mut outputs = [first_result.stdout.as_str(), second_result.stdout.as_str()];
            outputs.sort_unstable();
            let mut expected = [LINT_STALE_STDOUT, LINT_STALE_SUPERSEDE_STDOUT];
            expected.sort_unstable();
            assert_eq!(outputs, expected);
        }
        _ => {
            assert_race_command_result(first, first_result);
            assert_race_command_result(second, second_result);
        }
    }
}

fn assert_race_command_result(command: RaceCommand, result: &CommandResult) {
    match command {
        RaceCommand::Lint => {
            assert_lint_result(result);
            assert!(
                result.stdout == LINT_STALE_STDOUT || result.stdout == LINT_STALE_SUPERSEDE_STDOUT,
                "unexpected lint stdout: {:?}",
                result.stdout
            );
        }
        RaceCommand::PostToolUse => {
            assert_command_success(result, "post-tool-use");
            assert_eq!(result.stdout, POST_TOOL_USE_STDOUT);
        }
    }
}

fn assert_lint_result(result: &CommandResult) {
    assert_eq!(
        result.status,
        Some(1),
        "lint stdout={:?} stderr={:?}",
        result.stdout,
        result.stderr
    );
    assert_eq!(result.stderr, "");
}

fn assert_command_success(result: &CommandResult, label: &str) {
    assert_eq!(
        result.status,
        Some(0),
        "{label} failed stdout={:?} stderr={:?}",
        result.stdout,
        result.stderr
    );
    assert_eq!(result.stderr, "");
}

fn assert_status_snapshot(stdout: &str) {
    let one_decision = "Total: 1 decisions  0 stale  0 orphan  1 issues";
    let two_decisions = "Total: 2 decisions  0 stale  0 orphan  1 issues";
    assert!(
        stdout.contains(one_decision) || stdout.contains(two_decisions),
        "unexpected status stdout: {stdout:?}"
    );
}

fn assert_stale_kept_and_clean_added(root: &Path, file: &RelativePath) {
    let stored = load_dlog(root, file).unwrap().unwrap();
    assert_eq!(stored.decisions.len(), 2);
    let kept = stored.decisions.get_str("fn:kept").unwrap();
    let added = stored.decisions.get_str("fn:added").unwrap();
    assert_eq!(kept.status, Some(DecisionStatus::Stale));
    assert!(kept.stale_since.is_some());
    assert_eq!(added.status, None);
    assert_eq!(added.stale_since, None);
    assert_eq!(
        fs::read_to_string(dmap_path(root, file)).unwrap(),
        "1-3:fn:kept:STALE\n5-7:fn:added\n"
    );
}

fn temp_siblings(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.contains(".archiva-tmp-"))
        .collect()
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!("{prefix}-{}-{nanos}", std::process::id()));
    path
}
