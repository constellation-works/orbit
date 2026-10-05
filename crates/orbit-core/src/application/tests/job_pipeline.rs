use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use crate::application::job::pipeline::configure_pipeline_worker_stdio;
#[cfg(unix)]
use crate::application::job::pipeline::pipeline_worker_log_test_hook::{self, Phase};

#[cfg(unix)]
struct WorkerLogHook;

#[cfg(unix)]
impl WorkerLogHook {
    fn install<F>(phase: Phase, hook: F) -> Self
    where
        F: FnOnce(&Path) + 'static,
    {
        pipeline_worker_log_test_hook::install(phase, hook);
        Self
    }
}

#[cfg(unix)]
impl Drop for WorkerLogHook {
    fn drop(&mut self) {
        pipeline_worker_log_test_hook::clear();
    }
}

#[cfg(unix)]
#[test]
fn configure_pipeline_worker_stdio_rejects_symlinked_log_directory() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &configure_pipeline_worker_stdio_rejects_symlinked_log_directory,
    )) {
        return;
    }
    let root = TempDir::new().expect("tempdir");
    let outside = root.path().join("outside");
    let logs_dir = root.path().join("logs");
    std::fs::create_dir(&outside).expect("create outside directory");
    std::os::unix::fs::symlink(&outside, &logs_dir).expect("create log-directory symlink");
    let mut command = Command::new("true");

    let result = configure_pipeline_worker_stdio(&mut command, &logs_dir, "jrun-child");

    let error = match result {
        Ok(_) => panic!("symlinked log directories must fail closed"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("must not be a symlink"),
        "{error}"
    );
    assert!(
        !outside.join("jrun-child.worker.log").exists(),
        "worker setup must not follow a log-directory symlink"
    );
}

#[cfg(unix)]
#[test]
fn configure_pipeline_worker_stdio_binds_missing_suffix_to_validated_authority() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &configure_pipeline_worker_stdio_binds_missing_suffix_to_validated_authority,
    )) {
        return;
    }
    let root = TempDir::new().expect("tempdir");
    let authority = root.path().join("authority");
    let held_authority = root.path().join("held-authority");
    let outside = root.path().join("outside");
    std::fs::create_dir(&authority).expect("create authority");
    std::fs::create_dir(&outside).expect("create outside directory");

    let authority_for_hook = authority.clone();
    let outside_for_hook = outside.clone();
    let _hook = WorkerLogHook::install(Phase::AuthorityValidated, move |_| {
        std::fs::rename(&authority_for_hook, &held_authority).expect("move validated authority");
        std::os::unix::fs::symlink(&outside_for_hook, &authority_for_hook)
            .expect("replace authority with symlink");
    });
    let logs_dir = authority.join("missing").join("logs");
    let mut command = Command::new("true");

    let _error = match configure_pipeline_worker_stdio(&mut command, &logs_dir, "jrun-child") {
        Ok(_) => panic!("replaced authority must fail closed"),
        Err(error) => error,
    };

    assert!(
        !outside.join("missing").exists(),
        "missing suffix must not be created beneath replacement authority"
    );
}

#[cfg(unix)]
#[test]
fn configure_pipeline_worker_stdio_rejects_final_file_symlink_without_effects() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &configure_pipeline_worker_stdio_rejects_final_file_symlink_without_effects,
    )) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;

    let root = TempDir::new().expect("tempdir");
    let logs_dir = root.path().join("logs");
    let outside_file = root.path().join("outside.log");
    std::fs::create_dir(&logs_dir).expect("create logs directory");
    std::fs::write(&outside_file, "untouched\n").expect("write outside fixture");
    std::fs::set_permissions(&outside_file, std::fs::Permissions::from_mode(0o644))
        .expect("set outside permissions");

    let outside_for_hook = outside_file.clone();
    let _hook = WorkerLogHook::install(Phase::BeforeLogOpen, move |log_path| {
        std::os::unix::fs::symlink(&outside_for_hook, log_path).expect("redirect final log path");
    });
    let mut command = Command::new("true");

    assert!(
        configure_pipeline_worker_stdio(&mut command, &logs_dir, "jrun-child").is_err(),
        "a final-file symlink must fail closed"
    );
    assert_eq!(
        std::fs::read_to_string(&outside_file).expect("read outside fixture"),
        "untouched\n"
    );
    assert_eq!(
        std::fs::metadata(&outside_file)
            .expect("outside metadata")
            .permissions()
            .mode()
            & 0o777,
        0o644,
        "outside file permissions must remain unchanged"
    );
}

#[test]
fn configure_pipeline_worker_stdio_rejects_traversal_in_log_directory() {
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &configure_pipeline_worker_stdio_rejects_traversal_in_log_directory,
    )) {
        return;
    }
    let root = TempDir::new().expect("tempdir");
    let logs_dir = root.path().join("nested").join("..").join("logs");
    let mut command = Command::new("true");

    let error = match configure_pipeline_worker_stdio(&mut command, &logs_dir, "jrun-child") {
        Ok(_) => panic!("traversal components must fail closed"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("must not contain traversal components"),
        "{error}"
    );
    assert!(
        !root
            .path()
            .join("logs")
            .join("jrun-child.worker.log")
            .exists(),
        "worker setup must not resolve traversal into a log directory"
    );
}
