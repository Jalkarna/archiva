use std::fs;
use std::io;
use std::path::Path;

use crate::core::decision::{
    apply_decision_record, build_decision_record, next_decision_id, prepare_supersede,
    WriteDecisionInput,
};
use crate::core::dlog::{parse_dlog_yaml, render_dlog_yaml, DlogFile};
use crate::core::dmap::{parse_dmap, render_dmap, DmapEntry};
use crate::core::error::{ArchivaError, Result};
use crate::core::fs::{acquire_file_lock, atomic_write_text, read_text_if_exists};
use crate::core::ordered_map::OrderedMap;
use crate::core::paths::{decision_lock_path, dlog_path, dmap_path, RelativePath};
use crate::core::time::now_utc_millis;
use crate::core::version::DLOG_SCHEMA_VERSION;

pub fn create_empty_dlog(file: RelativePath) -> DlogFile {
    DlogFile {
        file,
        schema: DLOG_SCHEMA_VERSION,
        decisions: OrderedMap::new(),
    }
}

pub fn load_dlog(project_root: &Path, file: &RelativePath) -> Result<Option<DlogFile>> {
    let Some(content) = read_text_if_exists(&dlog_path(project_root, file))? else {
        return Ok(None);
    };
    let mut dlog = parse_dlog_yaml(&content)?;
    dlog.file = file.clone();
    Ok(Some(dlog))
}

pub fn load_or_create_dlog(project_root: &Path, file: RelativePath) -> Result<DlogFile> {
    match load_dlog(project_root, &file)? {
        Some(dlog) => Ok(dlog),
        None => Ok(create_empty_dlog(file)),
    }
}

pub fn dmap_entries_from_dlog(dlog: &DlogFile) -> Vec<DmapEntry> {
    dlog.decisions
        .iter()
        .map(|(anchor, decision)| DmapEntry {
            start_line: i64::from(decision.lines_hint.start),
            end_line: i64::from(decision.lines_hint.end),
            anchor: anchor.clone(),
            status: decision.status.clone(),
        })
        .collect()
}

pub fn render_dmap_from_dlog(dlog: &DlogFile) -> String {
    render_dmap(&dmap_entries_from_dlog(dlog))
}

pub fn load_dmap(project_root: &Path, file: &RelativePath) -> Result<Vec<DmapEntry>> {
    let path = dmap_path(project_root, file);
    let content = match read_text_if_exists(&path) {
        Ok(Some(content)) => content,
        Ok(None) => {
            if let Some(dlog) = load_dlog(project_root, file)? {
                ensure_dmap_current(project_root, &dlog, "load-dmap")?;
                return Ok(dmap_entries_from_dlog(&dlog));
            }
            return Ok(Vec::new());
        }
        Err(error @ ArchivaError::FileTooLarge { .. }) => {
            if let Some(dlog) = load_dlog(project_root, file)? {
                ensure_dmap_current(project_root, &dlog, "load-dmap")?;
                return Ok(dmap_entries_from_dlog(&dlog));
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    match parse_dmap(&content) {
        Ok(entries) => {
            if let Some(dlog) = load_dlog(project_root, file)? {
                let expected = render_dmap_from_dlog(&dlog);
                if content != expected {
                    ensure_dmap_current(project_root, &dlog, "load-dmap")?;
                    return Ok(dmap_entries_from_dlog(&dlog));
                }
            }
            Ok(entries)
        }
        Err(error) => {
            if let Some(dlog) = load_dlog(project_root, file)? {
                ensure_dmap_current(project_root, &dlog, "load-dmap")?;
                Ok(dmap_entries_from_dlog(&dlog))
            } else {
                Err(error.into())
            }
        }
    }
}

pub fn ensure_dmap_current(project_root: &Path, dlog: &DlogFile, command: &str) -> Result<()> {
    let path = dmap_path(project_root, &dlog.file);
    let expected = render_dmap_from_dlog(dlog);
    match read_text_if_exists(&path) {
        Ok(Some(content)) if content == expected => return Ok(()),
        Ok(_) | Err(ArchivaError::FileTooLarge { .. }) => {}
        Err(error) => return Err(error),
    }

    let timestamp = now_utc_millis().map_err(|source| {
        crate::core::error::ArchivaError::cli(format!("Failed to read system time: {source}"))
    })?;
    with_decision_file_lock(project_root, &dlog.file, command, &timestamp, || {
        let Some(current_dlog) = load_dlog(project_root, &dlog.file)? else {
            return Ok(());
        };
        ensure_dmap_current_locked(project_root, &current_dlog)
    })
}

pub fn ensure_dmap_current_locked(project_root: &Path, dlog: &DlogFile) -> Result<()> {
    let path = dmap_path(project_root, &dlog.file);
    let expected = render_dmap_from_dlog(dlog);
    match read_text_if_exists(&path) {
        Ok(Some(content)) if content == expected => return Ok(()),
        Ok(_) | Err(ArchivaError::FileTooLarge { .. }) => {}
        Err(error) => return Err(error),
    }
    write_dmap(project_root, dlog)
}

pub fn write_dlog(project_root: &Path, dlog: &DlogFile) -> Result<()> {
    let rendered = render_dlog_yaml(dlog)?;
    parse_dlog_yaml(&rendered)?;
    atomic_write_text(&dlog_path(project_root, &dlog.file), &rendered)
}

pub fn write_dmap(project_root: &Path, dlog: &DlogFile) -> Result<()> {
    atomic_write_text(
        &dmap_path(project_root, &dlog.file),
        &render_dmap_from_dlog(dlog),
    )
}

pub fn move_dlog_and_dmap_locked(
    project_root: &Path,
    old_file: &RelativePath,
    new_file: &RelativePath,
    command: &str,
    timestamp: &str,
) -> Result<Option<DlogFile>> {
    if old_file == new_file {
        return load_dlog(project_root, new_file);
    }

    with_two_decision_file_locks(project_root, old_file, new_file, command, timestamp, || {
        if let Some(current) = load_dlog(project_root, new_file)? {
            return Ok(Some(current));
        }
        let Some(mut dlog) = load_dlog(project_root, old_file)? else {
            return Ok(None);
        };

        dlog.file = new_file.clone();
        write_dlog(project_root, &dlog)?;
        write_dmap(project_root, &dlog)?;
        remove_file_if_exists(&dlog_path(project_root, old_file), "remove old dlog")?;
        remove_file_if_exists(&dmap_path(project_root, old_file), "remove old dmap")?;
        Ok(Some(dlog))
    })
}

pub fn write_dlog_and_dmap_locked(
    project_root: &Path,
    dlog: &DlogFile,
    command: &str,
    timestamp: &str,
) -> Result<()> {
    with_decision_file_lock(project_root, &dlog.file, command, timestamp, || {
        write_dlog(project_root, dlog)?;
        write_dmap(project_root, dlog)
    })
}

pub fn with_decision_file_lock<T>(
    project_root: &Path,
    file: &RelativePath,
    command: &str,
    timestamp: &str,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let lock_path = decision_lock_path(project_root, file);
    let lock = acquire_file_lock(&lock_path, command, timestamp)?;
    let output = action()?;
    lock.release()?;
    Ok(output)
}

fn with_two_decision_file_locks<T>(
    project_root: &Path,
    first_file: &RelativePath,
    second_file: &RelativePath,
    command: &str,
    timestamp: &str,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if first_file.as_str() <= second_file.as_str() {
        with_decision_file_lock(project_root, first_file, command, timestamp, || {
            with_decision_file_lock(project_root, second_file, command, timestamp, action)
        })
    } else {
        with_decision_file_lock(project_root, second_file, command, timestamp, || {
            with_decision_file_lock(project_root, first_file, command, timestamp, action)
        })
    }
}

fn remove_file_if_exists(path: &Path, action: &'static str) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(crate::core::error::ArchivaError::io(
            Some(path.to_path_buf()),
            action,
            source,
        )),
    }
}

pub fn write_decision_record_locked(
    project_root: &Path,
    input: &WriteDecisionInput,
    source: &str,
    decision_timestamp: &str,
    env_session: Option<&str>,
    lock_timestamp: &str,
) -> Result<crate::core::dlog::DecisionRecord> {
    let lock_path = decision_lock_path(project_root, &input.file);
    let lock = acquire_file_lock(&lock_path, "write-decision", lock_timestamp)?;
    let mut dlog = load_or_create_dlog(project_root, input.file.clone())?;
    let id = next_decision_id(&dlog)?;
    let supersede = prepare_supersede(&dlog, input.supersedes.as_deref(), &input.because)?;
    let history = supersede
        .as_ref()
        .map(|plan| plan.history.clone())
        .unwrap_or_default();
    let superseded_anchor = supersede.as_ref().map(|plan| plan.anchor.as_str());
    let decision =
        build_decision_record(input, id, source, decision_timestamp, env_session, history);

    apply_decision_record(
        &mut dlog,
        input.anchor.clone(),
        decision.clone(),
        superseded_anchor,
    );
    write_dlog(project_root, &dlog)?;
    write_dmap(project_root, &dlog)?;
    lock.release()?;

    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::{
        create_empty_dlog, dmap_entries_from_dlog, load_dlog, load_dmap, load_or_create_dlog,
        move_dlog_and_dmap_locked, render_dmap_from_dlog, write_decision_record_locked, write_dlog,
        write_dlog_and_dmap_locked,
    };
    use crate::core::dlog::{DecisionRecord, DlogFile, LineRange};
    use crate::core::dmap::{DecisionStatus, DmapEntry};
    use crate::core::fingerprint::{fingerprint, get_lines};
    use crate::core::fs::TEXT_FILE_MAX_BYTES;
    use crate::core::ordered_map::OrderedMap;
    use crate::core::paths::{decision_lock_path, dlog_path, dmap_path, RelativePath};
    use crate::core::version::DLOG_SCHEMA_VERSION;
    use crate::core::yaml::DEFAULT_MAX_DEPTH;
    use crate::core::{decision::WriteDecisionInput, dlog::RejectedAlternative};
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn creates_empty_dlog_and_loads_missing_files_like_typescript_storage() {
        let root = unique_temp_dir("archiva-storage-empty");
        let file = RelativePath::new("src/missing.ts").unwrap();

        assert_eq!(load_dlog(&root, &file).unwrap(), None);
        assert_eq!(load_dmap(&root, &file).unwrap(), Vec::<DmapEntry>::new());
        assert_eq!(
            load_or_create_dlog(&root, file.clone()).unwrap(),
            create_empty_dlog(file)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn derives_dmap_entries_from_dlog_in_decision_order_and_renders_sorted_map() {
        let dlog = fixture_dlog();
        assert_eq!(
            dmap_entries_from_dlog(&dlog),
            vec![
                DmapEntry {
                    start_line: 9,
                    end_line: 12,
                    anchor: "fn:late".to_string(),
                    status: Some(DecisionStatus::Stale),
                },
                DmapEntry {
                    start_line: 1,
                    end_line: 3,
                    anchor: "fn:first".to_string(),
                    status: None,
                },
            ]
        );
        assert_eq!(
            render_dmap_from_dlog(&dlog),
            "1-3:fn:first\n9-12:fn:late:STALE\n"
        );
    }

    #[test]
    fn writes_dlog_and_dmap_under_lock_and_loads_written_dlog() {
        let root = unique_temp_dir("archiva-storage-write");
        let dlog = fixture_dlog();
        let lock_path = decision_lock_path(&root, &dlog.file);

        write_dlog_and_dmap_locked(&root, &dlog, "write-decision", "2026-06-26T20:31:18.340Z")
            .unwrap();

        assert!(!lock_path.exists());
        assert_eq!(load_dlog(&root, &dlog.file).unwrap(), Some(dlog.clone()));
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            "1-3:fn:first\n9-12:fn:late:STALE\n"
        );
        assert_eq!(
            load_dmap(&root, &dlog.file).unwrap(),
            vec![
                DmapEntry {
                    start_line: 1,
                    end_line: 3,
                    anchor: "fn:first".to_string(),
                    status: None,
                },
                DmapEntry {
                    start_line: 9,
                    end_line: 12,
                    anchor: "fn:late".to_string(),
                    status: Some(DecisionStatus::Stale),
                },
            ]
        );
        assert!(temp_siblings(dlog_path(&root, &dlog.file).parent().unwrap()).is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_dlog_uses_requested_path_as_storage_identity() {
        let root = unique_temp_dir("archiva-storage-path-identity");
        let requested = RelativePath::new("src/requested.ts").unwrap();
        let other = RelativePath::new("src/declared.ts").unwrap();
        fs::create_dir_all(dlog_path(&root, &requested).parent().unwrap()).unwrap();
        fs::write(
            dlog_path(&root, &requested),
            "file: src/declared.ts\nschema: 1\ndecisions:\n  fn:kept:\n    id: dec_001\n    lines_hint:\n      - 1\n      - 3\n    fingerprint: abcdef\n    chose: keep requested path\n    because: fixture\n    rejected: []\n    timestamp: '2026-06-26T20:31:18.340Z'\n    history: []\n",
        )
        .unwrap();

        let loaded = load_dlog(&root, &requested).unwrap().unwrap();
        assert_eq!(loaded.file, requested);
        write_dlog(&root, &loaded).unwrap();

        assert!(dlog_path(&root, &requested).exists());
        assert!(!dlog_path(&root, &other).exists());
        assert!(fs::read_to_string(dlog_path(&root, &requested))
            .unwrap()
            .starts_with("file: src/requested.ts\n"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn moves_dlog_and_dmap_to_new_source_path_under_locks() {
        let root = unique_temp_dir("archiva-storage-move");
        let mut dlog = fixture_dlog();
        dlog.file = RelativePath::new("src/old.ts").unwrap();
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();
        let old_file = RelativePath::new("src/old.ts").unwrap();
        let new_file = RelativePath::new("src/new.ts").unwrap();

        let moved = move_dlog_and_dmap_locked(
            &root,
            &old_file,
            &new_file,
            "post-tool-use",
            "2026-06-26T20:32:18.340Z",
        )
        .unwrap()
        .unwrap();

        assert_eq!(moved.file, new_file);
        assert!(load_dlog(&root, &old_file).unwrap().is_none());
        assert!(load_dlog(&root, &new_file).unwrap().is_some());
        assert!(!dlog_path(&root, &old_file).exists());
        assert!(!dmap_path(&root, &old_file).exists());
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &new_file)).unwrap(),
            render_dmap_from_dlog(&moved)
        );
        assert!(!decision_lock_path(&root, &old_file).exists());
        assert!(!decision_lock_path(&root, &new_file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_locked_write_without_overwriting_existing_lock_or_files() {
        let root = unique_temp_dir("archiva-storage-existing-lock");
        let dlog = fixture_dlog();
        let lock_path = decision_lock_path(&root, &dlog.file);
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        fs::write(&lock_path, "pid=999\ncommand=other\ntimestamp=old\n").unwrap();

        let error =
            write_dlog_and_dmap_locked(&root, &dlog, "write-decision", "2026-06-26T20:31:18.340Z")
                .unwrap_err()
                .user_message();

        assert!(error.contains("Archiva lock already exists"));
        assert_eq!(
            fs::read_to_string(&lock_path).unwrap(),
            "pid=999\ncommand=other\ntimestamp=old\n"
        );
        assert!(!dlog_path(&root, &dlog.file).exists());
        assert!(!dmap_path(&root, &dlog.file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovers_expired_lock_before_writing_dlog_and_dmap() {
        let root = unique_temp_dir("archiva-storage-expired-lock");
        let dlog = fixture_dlog();
        let lock_path = decision_lock_path(&root, &dlog.file);
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        fs::write(
            &lock_path,
            "version=1\ntoken=stale\ncommand=other\ntimestamp=2026-06-26T20:00:00.000Z\n",
        )
        .unwrap();

        write_dlog_and_dmap_locked(&root, &dlog, "write-decision", "2026-06-26T20:03:00.000Z")
            .unwrap();

        assert!(!lock_path.exists());
        assert_eq!(load_dlog(&root, &dlog.file).unwrap(), Some(dlog.clone()));
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap(),
            render_dmap_from_dlog(&dlog)
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_to_write_invalid_dlog_domain_values() {
        let root = unique_temp_dir("archiva-storage-invalid-domain");
        let mut dlog = fixture_dlog();
        dlog.schema = 2;

        let error =
            write_dlog_and_dmap_locked(&root, &dlog, "write-decision", "2026-06-26T20:31:18.340Z")
                .unwrap_err()
                .user_message();

        assert_eq!(error, "schema: expected schema version 1");
        assert!(!dlog_path(&root, &dlog.file).exists());
        assert!(!dmap_path(&root, &dlog.file).exists());
        assert!(!decision_lock_path(&root, &dlog.file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn writes_decision_transaction_with_dlog_dmap_session_and_fingerprint() {
        let root = unique_temp_dir("archiva-storage-write-decision");
        let input = write_input("src/session.ts", "fn:processCheckout", None);
        let source = "export function processCheckout() {\n  return \"ok\";\n}\n";

        let decision = write_decision_record_locked(
            &root,
            &input,
            source,
            "2026-06-26T20:31:18.340Z",
            Some("env_session_contract"),
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap();

        assert_eq!(decision.id, "dec_001");
        assert_eq!(decision.session.as_deref(), Some("env_session_contract"));
        assert_eq!(decision.fingerprint, fingerprint(&get_lines(source, 1, 3)));

        let dlog = load_dlog(&root, &input.file).unwrap().unwrap();
        assert_eq!(
            dlog.decisions
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:processCheckout"]
        );
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &input.file)).unwrap(),
            "1-3:fn:processCheckout\n"
        );
        assert!(!decision_lock_path(&root, &input.file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_decision_transaction_supersedes_across_anchors_and_regenerates_dmap() {
        let root = unique_temp_dir("archiva-storage-write-supersede");
        let source = "function first() {\n  return 1;\n}\nfunction second() {\n  return 2;\n}\n";
        let first = write_input("src/supersede.ts", "fn:first", None);
        let first_decision = write_decision_record_locked(
            &root,
            &first,
            source,
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap();
        let second = write_input(
            "src/supersede.ts",
            "fn:second",
            Some(first_decision.id.as_str()),
        );

        let second_decision = write_decision_record_locked(
            &root,
            &second,
            source,
            "2026-06-26T20:32:18.340Z",
            None,
            "2026-06-26T20:32:18.341Z",
        )
        .unwrap();

        assert_eq!(second_decision.id, "dec_002");
        assert_eq!(second_decision.history.len(), 1);
        assert_eq!(second_decision.history[0].id, "dec_001");
        assert_eq!(
            second_decision.history[0].superseded_reason.as_deref(),
            Some("superseding reason")
        );

        let dlog = load_dlog(&root, &second.file).unwrap().unwrap();
        assert_eq!(
            dlog.decisions
                .iter()
                .map(|(anchor, _)| anchor.as_str())
                .collect::<Vec<_>>(),
            vec!["fn:second"]
        );
        assert_eq!(
            fs::read_to_string(dmap_path(&root, &second.file)).unwrap(),
            "1-3:fn:second\n"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn next_locked_write_regenerates_dmap_after_crash_left_stale_derivative() {
        let root = unique_temp_dir("archiva-storage-stale-dmap-recovery");
        let mut dlog = fixture_dlog();
        write_dlog_and_dmap_locked(&root, &dlog, "test", "2026-06-26T20:31:18.340Z").unwrap();
        dlog.decisions.insert(
            "fn:crashed".to_string(),
            DecisionRecord {
                id: "dec_003".to_string(),
                lines_hint: LineRange { start: 20, end: 22 },
                fingerprint: "33333333".to_string(),
                chose: "crash-visible dlog entry".to_string(),
                because: "simulate dlog write succeeding before dmap rewrite".to_string(),
                rejected: Vec::new(),
                expires_if: None,
                session: None,
                timestamp: "2026-06-26T20:33:18.340Z".to_string(),
                history: Vec::new(),
                status: None,
                stale_since: None,
                supersedes: None,
            },
        );
        write_dlog(&root, &dlog).unwrap();
        let stale_dmap = fs::read_to_string(dmap_path(&root, &dlog.file)).unwrap();
        assert!(!stale_dmap.contains("fn:crashed"));

        let input = write_input("src/store.ts", "fn:after", None);
        write_decision_record_locked(
            &root,
            &input,
            "function after() {\n  return 1;\n}\n",
            "2026-06-26T20:34:18.340Z",
            None,
            "2026-06-26T20:34:18.341Z",
        )
        .unwrap();

        let stored = load_dlog(&root, &input.file).unwrap().unwrap();
        let regenerated = fs::read_to_string(dmap_path(&root, &input.file)).unwrap();
        assert_eq!(regenerated, render_dmap_from_dlog(&stored));
        assert!(regenerated.contains("fn:crashed"));
        assert!(regenerated.contains("fn:after"));
        assert!(!decision_lock_path(&root, &input.file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_decision_transaction_rejects_unknown_supersedes_before_writing() {
        let root = unique_temp_dir("archiva-storage-write-bad-supersede");
        let input = write_input("src/bad-history.ts", "fn:next", Some("dec_404"));

        let error = write_decision_record_locked(
            &root,
            &input,
            "function next() {\n  return 1;\n}\n",
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap_err()
        .user_message();

        assert!(error.contains("Cannot supersede unknown decision id \"dec_404\""));
        assert!(!dlog_path(&root, &input.file).exists());
        assert!(!dmap_path(&root, &input.file).exists());
        assert!(!decision_lock_path(&root, &input.file).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_or_create_dlog_reports_corruption_instead_of_treating_it_as_missing() {
        let root = unique_temp_dir("archiva-storage-corrupt-load");
        let file = RelativePath::new("src/corrupt.ts").unwrap();
        let path = dlog_path(&root, &file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "file: src/corrupt.ts\nschema: 1\ndecisions:\n  fn:bad:\n    id: dec_001\n",
        )
        .unwrap();

        let error = load_or_create_dlog(&root, file).unwrap_err().user_message();

        assert_eq!(error, "decisions.fn:bad.lines_hint: missing required field");
        assert!(path.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_dlog_reports_yaml_depth_limit_without_rewriting_files() {
        let root = unique_temp_dir("archiva-storage-deep-dlog-load");
        let file = RelativePath::new("src/deep.ts").unwrap();
        let path = dlog_path(&root, &file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content = deep_unknown_dlog(file.as_str(), DEFAULT_MAX_DEPTH + 2);
        fs::write(&path, &content).unwrap();

        let error = load_dlog(&root, &file).unwrap_err().user_message();

        assert!(error.contains("YAML nesting exceeds configured depth limit"));
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        assert!(!dmap_path(&root, &file).exists());
        assert!(!decision_lock_path(&root, &file).exists());
        assert!(temp_siblings(path.parent().unwrap()).is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_dmap_reports_yaml_depth_limit_when_dlog_repair_requires_deep_dlog() {
        let root = unique_temp_dir("archiva-storage-deep-dmap-repair");
        let file = RelativePath::new("src/deep.ts").unwrap();
        let path = dlog_path(&root, &file);
        let map_path = dmap_path(&root, &file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content = deep_unknown_dlog(file.as_str(), DEFAULT_MAX_DEPTH + 2);
        let stale_map = "99-100:fn:old\n";
        fs::write(&path, &content).unwrap();
        fs::write(&map_path, stale_map).unwrap();

        let error = load_dmap(&root, &file).unwrap_err().user_message();

        assert!(error.contains("YAML nesting exceeds configured depth limit"));
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        assert_eq!(fs::read_to_string(&map_path).unwrap(), stale_map);
        assert!(!decision_lock_path(&root, &file).exists());
        assert!(temp_siblings(path.parent().unwrap()).is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_dlog_reports_oversized_file_without_rewriting_files() {
        let root = unique_temp_dir("archiva-storage-oversized-dlog-load");
        let file = RelativePath::new("src/huge.ts").unwrap();
        let path = dlog_path(&root, &file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![b'x'; TEXT_FILE_MAX_BYTES + 1]).unwrap();

        let error = load_dlog(&root, &file).unwrap_err().user_message();

        assert!(error.contains("exceeds configured byte limit"));
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            (TEXT_FILE_MAX_BYTES + 1) as u64
        );
        assert!(!dmap_path(&root, &file).exists());
        assert!(!decision_lock_path(&root, &file).exists());
        assert!(temp_siblings(path.parent().unwrap()).is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_dmap_repairs_oversized_derivative_from_valid_dlog() {
        let root = unique_temp_dir("archiva-storage-oversized-dmap-repair");
        let dlog = fixture_dlog();
        write_dlog(&root, &dlog).unwrap();
        let map_path = dmap_path(&root, &dlog.file);
        fs::create_dir_all(map_path.parent().unwrap()).unwrap();
        fs::write(&map_path, vec![b'x'; TEXT_FILE_MAX_BYTES + 1]).unwrap();

        let entries = load_dmap(&root, &dlog.file).unwrap();

        assert_eq!(entries, dmap_entries_from_dlog(&dlog));
        assert_eq!(
            fs::read_to_string(&map_path).unwrap(),
            render_dmap_from_dlog(&dlog)
        );
        assert!(!decision_lock_path(&root, &dlog.file).exists());
        assert!(temp_siblings(map_path.parent().unwrap()).is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn write_decision_transaction_preserves_corrupt_dlog_and_releases_lock() {
        let root = unique_temp_dir("archiva-storage-corrupt-write");
        let input = write_input("src/corrupt.ts", "fn:next", None);
        let path = dlog_path(&root, &input.file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corrupt = "file: src/corrupt.ts\nschema: 1\ndecisions:\n  fn:bad:\n    id: dec_001\n";
        fs::write(&path, corrupt).unwrap();

        let error = write_decision_record_locked(
            &root,
            &input,
            "function next() {\n  return 1;\n}\n",
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap_err()
        .user_message();

        assert_eq!(error, "decisions.fn:bad.lines_hint: missing required field");
        assert_eq!(fs::read_to_string(&path).unwrap(), corrupt);
        assert!(!dmap_path(&root, &input.file).exists());
        assert!(!decision_lock_path(&root, &input.file).exists());
        assert!(temp_siblings(path.parent().unwrap()).is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_dmap_reports_corrupt_derivative_map_lines() {
        let root = unique_temp_dir("archiva-storage-corrupt-dmap");
        let file = RelativePath::new("src/corrupt.ts").unwrap();
        let path = dmap_path(&root, &file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not-a-range:fn:bad\n").unwrap();

        let error = load_dmap(&root, &file).unwrap_err().user_message();

        assert_eq!(error, "Invalid .dmap range: not-a-range:fn:bad");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn load_dmap_repairs_missing_stale_and_corrupt_derivatives_from_valid_dlog() {
        for mode in ["missing", "stale", "corrupt"] {
            let root = unique_temp_dir(&format!("archiva-storage-repair-dmap-{mode}"));
            let dlog = fixture_dlog();
            write_dlog(&root, &dlog).unwrap();
            let map_path = dmap_path(&root, &dlog.file);

            match mode {
                "missing" => {}
                "stale" => {
                    fs::create_dir_all(map_path.parent().unwrap()).unwrap();
                    fs::write(&map_path, "1-3:fn:old\n").unwrap();
                }
                "corrupt" => {
                    fs::create_dir_all(map_path.parent().unwrap()).unwrap();
                    fs::write(&map_path, "not-a-range:fn:old\n").unwrap();
                }
                _ => unreachable!(),
            }

            let entries = load_dmap(&root, &dlog.file).unwrap();

            assert_eq!(entries, dmap_entries_from_dlog(&dlog));
            assert_eq!(
                fs::read_to_string(&map_path).unwrap(),
                render_dmap_from_dlog(&dlog)
            );
            assert!(!decision_lock_path(&root, &dlog.file).exists());
            assert!(temp_siblings(map_path.parent().unwrap()).is_empty());

            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn write_decision_record_lock_blocks_before_loading_or_writing() {
        let root = unique_temp_dir("archiva-storage-record-existing-lock");
        let input = write_input("src/session.ts", "fn:processCheckout", None);
        let lock_path = decision_lock_path(&root, &input.file);
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        fs::write(&lock_path, "pid=999\ncommand=other\ntimestamp=old\n").unwrap();

        let error = write_decision_record_locked(
            &root,
            &input,
            "export function processCheckout() {\n  return \"ok\";\n}\n",
            "2026-06-26T20:31:18.340Z",
            None,
            "2026-06-26T20:31:18.341Z",
        )
        .unwrap_err()
        .user_message();

        assert!(error.contains("Archiva lock already exists"));
        assert_eq!(
            fs::read_to_string(&lock_path).unwrap(),
            "pid=999\ncommand=other\ntimestamp=old\n"
        );
        assert!(!dlog_path(&root, &input.file).exists());
        assert!(!dmap_path(&root, &input.file).exists());

        let _ = fs::remove_dir_all(root);
    }

    fn fixture_dlog() -> DlogFile {
        DlogFile {
            file: RelativePath::new("src/store.ts").unwrap(),
            schema: DLOG_SCHEMA_VERSION,
            decisions: OrderedMap::from_entries(vec![
                (
                    "fn:late".to_string(),
                    DecisionRecord {
                        id: "dec_002".to_string(),
                        lines_hint: LineRange { start: 9, end: 12 },
                        fingerprint: "22222222".to_string(),
                        chose: "late".to_string(),
                        because: "fixture".to_string(),
                        rejected: Vec::new(),
                        expires_if: None,
                        session: None,
                        timestamp: "2026-06-26T20:32:18.340Z".to_string(),
                        history: Vec::new(),
                        status: Some(DecisionStatus::Stale),
                        stale_since: Some("2026-06-26T21:00:00.000Z".to_string()),
                        supersedes: None,
                    },
                ),
                (
                    "fn:first".to_string(),
                    DecisionRecord {
                        id: "dec_001".to_string(),
                        lines_hint: LineRange { start: 1, end: 3 },
                        fingerprint: "11111111".to_string(),
                        chose: "first".to_string(),
                        because: "fixture".to_string(),
                        rejected: Vec::new(),
                        expires_if: None,
                        session: None,
                        timestamp: "2026-06-26T20:31:18.340Z".to_string(),
                        history: Vec::new(),
                        status: None,
                        stale_since: None,
                        supersedes: None,
                    },
                ),
            ]),
        }
    }

    fn write_input(file: &str, anchor: &str, supersedes: Option<&str>) -> WriteDecisionInput {
        WriteDecisionInput {
            file: RelativePath::new(file).unwrap(),
            anchor: anchor.to_string(),
            lines: LineRange { start: 1, end: 3 },
            chose: match supersedes {
                Some(_) => "superseding choice".to_string(),
                None => "optimistic locking via version field".to_string(),
            },
            because: match supersedes {
                Some(_) => "superseding reason".to_string(),
                None => "checkout and inventory deduction race under concurrent carts".to_string(),
            },
            rejected: vec![RejectedAlternative {
                approach: "SELECT FOR UPDATE".to_string(),
                reason: "deadlocks on hot SKUs".to_string(),
            }],
            expires_if: None,
            supersedes: supersedes.map(str::to_string),
            session: None,
        }
    }

    fn deep_unknown_dlog(file: &str, depth: usize) -> String {
        format!(
            "file: {file}\nschema: 1\ndecisions:\n  fn:deep:\n    id: dec_001\n    lines_hint: [1, 3]\n    fingerprint: abc123ef\n    chose: bounded yaml\n    because: fixture\n    rejected: []\n    timestamp: '2026-06-26T20:31:18.340Z'\n    history: []\nignored: {}0{}\n",
            "[".repeat(depth),
            "]".repeat(depth)
        )
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
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("{prefix}-{}-{nanos}", std::process::id()));
        path
    }
}
