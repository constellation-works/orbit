use std::fs;

use orbit_common::OrbitError;
use orbit_types::task::{
    TASK_ARTIFACT_FILES_DIR_NAME, TASK_ARTIFACTS_DIR_NAME, TASK_ENVELOPE_FILE_NAME,
    TASK_EVENTS_FILE_NAME,
};
use tempfile::TempDir;

use super::super::write::write_bundle_atomically;
use crate::repository::task::tests::test_support::{bundle_store, sample_bundle};

#[test]
fn write_and_read_bundle_round_trips_v2_shape() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");

    let created = store.create_bundle(&bundle).expect("create bundle");
    assert_eq!(created.binding.task_id, "ORB-00000");

    let read = store.read_bundle("ORB-00000").expect("read bundle");
    assert_eq!(read.envelope, bundle.envelope);
    assert_eq!(read.description, bundle.description);
    assert_eq!(read.acceptance, bundle.acceptance);
    assert_eq!(read.plan, bundle.plan);
    assert_eq!(read.events, bundle.events);
    assert_eq!(read.comments, bundle.comments);
    assert!(
        created
            .binding
            .canonical_path
            .join(TASK_ENVELOPE_FILE_NAME)
            .is_file()
    );
    assert!(
        created
            .binding
            .canonical_path
            .join(TASK_ARTIFACTS_DIR_NAME)
            .join(TASK_ARTIFACT_FILES_DIR_NAME)
            .is_dir()
    );
}

#[test]
fn interrupted_bundle_publish_never_exposes_partial_final_directory() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");

    let error = write_bundle_atomically(&bundle_dir, &bundle, None, |staging, final_path| {
        assert!(
            !final_path.exists(),
            "final path must remain absent while staging"
        );
        assert!(staging.join(TASK_ENVELOPE_FILE_NAME).is_file());
        assert!(staging.join(TASK_EVENTS_FILE_NAME).is_file());
        assert!(
            staging
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join(TASK_ARTIFACT_FILES_DIR_NAME)
                .is_dir()
        );
        Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "simulated interruption before rename",
        ))
    })
    .expect_err("interrupted publish must fail");

    assert!(matches!(error, OrbitError::Io(message) if message.contains("simulated interruption")));
    assert!(
        !bundle_dir.exists(),
        "an interrupted writer must not expose the canonical bundle path"
    );
    let parent = bundle_dir.parent().expect("bundle parent");
    let staging_entries = fs::read_dir(parent)
        .expect("read bundle parent")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".staging"))
        .collect::<Vec<_>>();
    assert!(
        staging_entries.is_empty(),
        "handled failures clean their private staging directories"
    );
}

#[test]
fn erofs_bundle_publish_names_path_and_hints_sandbox() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    let bundle = sample_bundle("ORB-00000");
    let bundle_dir = store.bundle_path("ORB-00000").expect("bundle path");

    let error = write_bundle_atomically(&bundle_dir, &bundle, None, |_staging, _final_path| {
        Err(std::io::Error::new(
            std::io::ErrorKind::ReadOnlyFilesystem,
            "Read-only file system",
        ))
    })
    .expect_err("EROFS publish must fail");

    match error {
        OrbitError::Io(message) => {
            assert!(
                message.contains(&bundle_dir.display().to_string()),
                "expected path in `{message}`"
            );
            assert!(
                message.contains("is not writable"),
                "expected writable attribution in `{message}`"
            );
            assert!(
                message.contains("sandbox or environment"),
                "expected sandbox/environment hint in `{message}`"
            );
            assert!(
                message.contains("not an Orbit store defect"),
                "expected store-defect negation in `{message}`"
            );
        }
        other => panic!("expected Io, got {other}"),
    }
    assert!(
        !bundle_dir.exists(),
        "an EROFS writer must not expose the canonical bundle path"
    );
}

/// `write_yaml_durable_with` is bound to durable `atomic_write_text`, so both
/// bundle create and envelope rewrite fsync the `task.yaml` temp file before
/// rename. When `strace` is available this asserts that each temp rename
/// follows a successful fsync of that exact file by the renaming thread; a
/// sandbox denial or missing `strace` is not a product failure.
#[test]
fn task_yaml_temp_is_fsynced_before_rename_on_create_and_rewrite() {
    exercise_task_yaml_create_and_rewrite();

    #[cfg(target_os = "linux")]
    trace_task_yaml_fsync_before_rename();
}

fn exercise_task_yaml_create_and_rewrite() {
    let temp = TempDir::new().expect("tempdir");
    let store = bundle_store(&temp);
    store
        .create_bundle(&sample_bundle("ORB-00000"))
        .expect("create bundle");
    let mut envelope = sample_bundle("ORB-00000").envelope;
    envelope.title = "Rewritten".to_string();
    store
        .rewrite_envelope("ORB-00000", &envelope)
        .expect("rewrite envelope");
    let read = store.read_bundle("ORB-00000").expect("read");
    assert_eq!(read.envelope.title, "Rewritten");
}

#[cfg(target_os = "linux")]
fn trace_task_yaml_fsync_before_rename() {
    use std::process::Command;

    if std::env::var_os("ORB_TASK_YAML_FSYNC_PROBE").is_some() {
        return;
    }
    let Ok(version) = Command::new("strace").arg("-V").output() else {
        return;
    };
    if !version.status.success() {
        return;
    }
    let log = tempfile::NamedTempFile::new().expect("strace log");
    let Some(test_name) = std::thread::current().name().map(ToOwned::to_owned) else {
        return;
    };
    let output = Command::new("strace")
        .args([
            "-f",
            "-y",
            "-e",
            "trace=fsync,fdatasync,rename,renameat,renameat2",
            "-o",
        ])
        .arg(log.path())
        .arg(std::env::current_exe().expect("test exe"))
        .args(["--exact", &test_name])
        .env("ORB_TASK_YAML_FSYNC_PROBE", "1")
        .env("RUST_TEST_THREADS", "1")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return,
        Err(_) => return,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Operation not permitted")
            || stderr.contains("Permission denied")
            || stderr.contains("not permitted")
        {
            return;
        }
        // Probe ran the test body; parse the log even if the harness exit is noisy.
    }
    let trace = fs::read_to_string(log.path()).unwrap_or_default();
    if trace.is_empty() {
        return;
    }
    if let Err(violation) = check_task_yaml_tmp_fsynced_before_rename(&trace) {
        panic!("task.yaml durability trace rejected: {violation:?}\n{trace}");
    }
}

/// Why an `strace -f -y` log fails to prove that every `task.yaml` temp file
/// was durably flushed before it was renamed into place.
#[derive(Debug, PartialEq, Eq)]
enum TaskYamlTraceViolation {
    /// A successful `task.yaml` temp rename (1-based trace line) had no earlier
    /// successful fsync of that exact temp file by the renaming thread.
    UnsyncedRename { line: usize },
    /// Fewer than the create and rewrite renames completed in the trace.
    MissingRenames { found: usize },
}

/// A traced syscall that has started but whose result is on a later
/// `<... resumed>` line, keyed by the thread that issued it.
enum PendingCall {
    Sync {
        path: Option<String>,
    },
    Rename {
        source: String,
        destination: String,
        synced: bool,
        line: usize,
    },
}

/// Returns the number of successful `task.yaml` temp renames when each one
/// follows a successful fsync/fdatasync of the same temp file, as named by the
/// `-y` descriptor path, completed earlier by the same thread.
fn check_task_yaml_tmp_fsynced_before_rename(trace: &str) -> Result<usize, TaskYamlTraceViolation> {
    use std::collections::{HashMap, HashSet};

    let mut synced: HashSet<(Option<&str>, String)> = HashSet::new();
    let mut pending: HashMap<Option<&str>, PendingCall> = HashMap::new();
    let mut renames = 0usize;
    for (index, line) in trace.lines().enumerate() {
        let number = index + 1;
        let (pid, body) = split_trace_pid(line);
        let (call, result) = if let Some(resumed) = body.strip_prefix("<... ") {
            let Some(call) = pending.remove(&pid) else {
                continue;
            };
            let Some(result) = syscall_result(resumed) else {
                continue;
            };
            (call, result)
        } else {
            let Some((name, rest)) = body.split_once('(') else {
                continue;
            };
            let (args, result) = match rest.strip_suffix(" <unfinished ...>") {
                Some(args) => (args, None),
                None => match rest.rsplit_once(") = ") {
                    Some((args, result)) => (args, Some(result)),
                    None => continue,
                },
            };
            let call = match name {
                "fsync" | "fdatasync" => PendingCall::Sync {
                    path: descriptor_path(args),
                },
                "rename" | "renameat" | "renameat2" => {
                    let Some((source, destination)) = rename_paths(args) else {
                        continue;
                    };
                    let synced = synced.contains(&(pid, source.clone()));
                    PendingCall::Rename {
                        source,
                        destination,
                        synced,
                        line: number,
                    }
                }
                _ => continue,
            };
            let Some(result) = result else {
                pending.insert(pid, call);
                continue;
            };
            (call, result)
        };
        if !syscall_succeeded(result) {
            continue;
        }
        match call {
            PendingCall::Sync { path: Some(path) } => {
                synced.insert((pid, path));
            }
            PendingCall::Sync { path: None } => {}
            PendingCall::Rename {
                source,
                destination,
                synced: was_synced,
                line,
            } => {
                if !is_task_yaml_tmp_rename(&source, &destination) {
                    continue;
                }
                if !was_synced {
                    return Err(TaskYamlTraceViolation::UnsyncedRename { line });
                }
                synced.remove(&(pid, source));
                renames += 1;
            }
        }
    }
    if renames < 2 {
        return Err(TaskYamlTraceViolation::MissingRenames { found: renames });
    }
    Ok(renames)
}

/// Splits the `strace -f` thread prefix (`1234 ` in `-o` logs, `[pid 1234] `
/// on stderr) from the syscall text.
fn split_trace_pid(line: &str) -> (Option<&str>, &str) {
    let line = line.trim_start();
    if let Some(rest) = line.strip_prefix("[pid")
        && let Some((pid, body)) = rest.split_once(']')
    {
        return (Some(pid.trim()), body.trim_start());
    }
    match line.split_once(' ') {
        Some((pid, body)) if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) => {
            (Some(pid), body.trim_start())
        }
        _ => (None, line),
    }
}

fn syscall_result(resumed: &str) -> Option<&str> {
    resumed.rsplit_once(") = ").map(|(_, result)| result)
}

fn syscall_succeeded(result: &str) -> bool {
    result.split_whitespace().next() == Some("0")
}

/// The `-y` path of a single descriptor argument such as `3</dir/file>`.
fn descriptor_path(args: &str) -> Option<String> {
    let start = args.find('<')?;
    let end = args.rfind('>')?;
    (end > start).then(|| args[start + 1..end].to_string())
}

/// Source and destination of `rename`, `renameat`, or `renameat2`, resolving a
/// relative path against its `-y` directory-descriptor annotation.
fn rename_paths(args: &str) -> Option<(String, String)> {
    let mut paths = Vec::new();
    let mut directory: Option<String> = None;
    let mut chars = args.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '<' => directory = Some(chars.by_ref().take_while(|&c| c != '>').collect()),
            '"' => {
                let mut path = String::new();
                while let Some(ch) = chars.next() {
                    match ch {
                        '"' => break,
                        '\\' => path.extend(chars.next()),
                        _ => path.push(ch),
                    }
                }
                let path = match directory.take() {
                    Some(dir) if !path.starts_with('/') => format!("{dir}/{path}"),
                    _ => path,
                };
                paths.push(path);
            }
            _ => {}
        }
    }
    let mut paths = paths.into_iter();
    Some((paths.next()?, paths.next()?))
}

fn is_task_yaml_tmp_rename(source: &str, destination: &str) -> bool {
    let file_name = |path: &str| {
        std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned)
    };
    let (Some(source), Some(destination)) = (file_name(source), file_name(destination)) else {
        return false;
    };
    source.starts_with(".task.yaml.") && source.ends_with(".tmp") && destination == "task.yaml"
}

const CREATE_TMP: &str = "/work/tasks/ORB-00000.staging/.task.yaml.11.0.tmp";
const CREATE_DST: &str = "/work/tasks/ORB-00000.staging/task.yaml";
const REWRITE_TMP: &str = "/work/tasks/ORB-00000/.task.yaml.22.1.tmp";
const REWRITE_DST: &str = "/work/tasks/ORB-00000/task.yaml";

fn fsync_line(pid: u32, path: &str, result: &str) -> String {
    format!("{pid} fsync(3<{path}>) = {result}")
}

fn rename_line(pid: u32, source: &str, destination: &str) -> String {
    format!("{pid} rename(\"{source}\", \"{destination}\") = 0")
}

fn trace(lines: &[String]) -> String {
    lines.join("\n")
}

#[test]
fn fsync_trace_accepts_synced_create_and_rewrite_renames() {
    let log = trace(&[
        "4100 fsync(5</work/unrelated.log>) = 0".to_string(),
        fsync_line(4101, CREATE_TMP, "0"),
        rename_line(4101, CREATE_TMP, CREATE_DST),
        "4101 fsync(4</work/tasks/ORB-00000.staging>) = 0".to_string(),
        format!("4101 fdatasync(3<{REWRITE_TMP}>) = 0"),
        format!(
            "4101 renameat2(AT_FDCWD</work>, \"{REWRITE_TMP}\", AT_FDCWD</work>, \"{REWRITE_DST}\", 0) = 0"
        ),
        "4101 +++ exited with 0 +++".to_string(),
    ]);
    assert_eq!(check_task_yaml_tmp_fsynced_before_rename(&log), Ok(2));
}

#[test]
fn fsync_trace_accepts_resumed_sync_and_relative_renameat_amid_other_threads() {
    let log = trace(&[
        format!("4101 fsync(3<{CREATE_TMP}> <unfinished ...>"),
        "4102 fsync(7</work/other.db>) = 0".to_string(),
        "4101 <... fsync resumed>) = 0".to_string(),
        format!("4101 rename(\"{CREATE_TMP}\", \"{CREATE_DST}\" <unfinished ...>"),
        "4102 fsync(7</work/other.db>) = 0".to_string(),
        "4101 <... rename resumed>) = 0".to_string(),
        fsync_line(4101, REWRITE_TMP, "0"),
        "4101 renameat(6</work/tasks/ORB-00000>, \".task.yaml.22.1.tmp\", 6</work/tasks/ORB-00000>, \"task.yaml\") = 0".to_string(),
    ]);
    assert_eq!(check_task_yaml_tmp_fsynced_before_rename(&log), Ok(2));
}

#[test]
fn fsync_trace_rejects_rename_after_only_unrelated_file_fsync() {
    let log = trace(&[
        fsync_line(4101, "/work/tasks/ORB-00000.staging/events.jsonl", "0"),
        rename_line(4101, CREATE_TMP, CREATE_DST),
        fsync_line(4101, REWRITE_TMP, "0"),
        rename_line(4101, REWRITE_TMP, REWRITE_DST),
    ]);
    assert_eq!(
        check_task_yaml_tmp_fsynced_before_rename(&log),
        Err(TaskYamlTraceViolation::UnsyncedRename { line: 2 })
    );
}

#[test]
fn fsync_trace_rejects_rename_after_failed_fsync() {
    let log = trace(&[
        fsync_line(4101, CREATE_TMP, "0"),
        rename_line(4101, CREATE_TMP, CREATE_DST),
        fsync_line(4101, REWRITE_TMP, "-1 EIO (Input/output error)"),
        rename_line(4101, REWRITE_TMP, REWRITE_DST),
    ]);
    assert_eq!(
        check_task_yaml_tmp_fsynced_before_rename(&log),
        Err(TaskYamlTraceViolation::UnsyncedRename { line: 4 })
    );
}

#[test]
fn fsync_trace_rejects_fsync_after_matching_rename() {
    let log = trace(&[
        rename_line(4101, CREATE_TMP, CREATE_DST),
        fsync_line(4101, CREATE_TMP, "0"),
        fsync_line(4101, REWRITE_TMP, "0"),
        rename_line(4101, REWRITE_TMP, REWRITE_DST),
    ]);
    assert_eq!(
        check_task_yaml_tmp_fsynced_before_rename(&log),
        Err(TaskYamlTraceViolation::UnsyncedRename { line: 1 })
    );
}

#[test]
fn fsync_trace_rejects_interleaved_sync_by_another_thread_or_resumed_after_rename() {
    let other_thread = trace(&[
        fsync_line(4102, CREATE_TMP, "0"),
        rename_line(4101, CREATE_TMP, CREATE_DST),
        fsync_line(4101, REWRITE_TMP, "0"),
        rename_line(4101, REWRITE_TMP, REWRITE_DST),
    ]);
    assert_eq!(
        check_task_yaml_tmp_fsynced_before_rename(&other_thread),
        Err(TaskYamlTraceViolation::UnsyncedRename { line: 2 })
    );

    let resumed_late = trace(&[
        fsync_line(4101, CREATE_TMP, "0"),
        rename_line(4101, CREATE_TMP, CREATE_DST),
        format!("4102 fsync(3<{REWRITE_TMP}> <unfinished ...>"),
        rename_line(4101, REWRITE_TMP, REWRITE_DST),
        "4102 <... fsync resumed>) = 0".to_string(),
    ]);
    assert_eq!(
        check_task_yaml_tmp_fsynced_before_rename(&resumed_late),
        Err(TaskYamlTraceViolation::UnsyncedRename { line: 4 })
    );

    let resumed_failed = trace(&[
        format!("4101 fsync(3<{CREATE_TMP}> <unfinished ...>"),
        "4102 fsync(7</work/other.db>) = 0".to_string(),
        "4101 <... fsync resumed>) = -1 EIO (Input/output error)".to_string(),
        rename_line(4101, CREATE_TMP, CREATE_DST),
    ]);
    assert_eq!(
        check_task_yaml_tmp_fsynced_before_rename(&resumed_failed),
        Err(TaskYamlTraceViolation::UnsyncedRename { line: 4 })
    );
}

#[test]
fn fsync_trace_requires_both_create_and_rewrite_renames() {
    let log = trace(&[
        fsync_line(4101, CREATE_TMP, "0"),
        rename_line(4101, CREATE_TMP, CREATE_DST),
    ]);
    assert_eq!(
        check_task_yaml_tmp_fsynced_before_rename(&log),
        Err(TaskYamlTraceViolation::MissingRenames { found: 1 })
    );
}
