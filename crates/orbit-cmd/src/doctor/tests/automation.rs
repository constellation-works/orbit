use super::*;

/// An enabled delivery definition owned here whose branch does not exist can
/// never baseline, so no stall marker will ever say so. Doctor names the
/// definition, the branch, git's failure text and the corrective action.

#[test]
fn automation_consumer_check_warns_about_an_unresolvable_branch() {
    use orbit_core::application::auto_tasks::AutoTaskAddParams;
    use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
    use orbit_types::workflow::automation::{CoverageClass, DeliveryTrigger};
    use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};

    let temp = tempfile::tempdir().expect("temp dir");
    let runtime =
        workspace_runtime(&temp).with_automation_machine_identity(Some("fixture-machine".into()));
    let repo = temp.path().join("repo");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "--initial-branch=main"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "base",
    ]);

    let delivery = |branch: &str| AutoTaskSchedule::Deliveries {
        deliveries_landed: DeliveryTrigger {
            owner_machine: Some("fixture-machine".into()),
            branch: branch.into(),
            threshold: 1,
            max_wait_minutes: 60,
            coverage: CoverageClass::IntegratedQaV1,
            max_items: 20,
            retries: 0,
        },
    };
    let template = AutoTaskTemplate {
        title: "Exercise batch".into(),
        description: "Inspect captured input".into(),
        acceptance_criteria: vec!["All obligations examined".into()],
        task_type: TaskType::Chore,
        tags: vec![],
        required_tools: vec![],
        priority: TaskPriority::Medium,
        complexity: None,
        crew: None,
        status: TaskStatus::Backlog,
    };
    for (name, branch) in [("delivery-qa", "agent-main"), ("delivery-ok", "main")] {
        runtime
            .auto_task_add(AutoTaskAddParams {
                name: name.into(),
                description: "fixture".into(),
                schedule: delivery(branch),
                template: template.clone(),
                dedupe: DedupePolicy::SkipIfOpen,
            })
            .expect("add delivery definition");
        runtime.auto_task_toggle(name, true).expect("enable");
    }

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "automation-consumers");

    assert_eq!(row.status, WorkspaceDoctorStatus::Warning);
    assert!(
        row.message.contains(
            "1 enabled delivery definition(s) name a branch that does not exist in this \
             repository and can never baseline: delivery-qa targets `agent-main`: \
             evidence_unavailable: git rev-parse --verify --end-of-options \
             refs/heads/agent-main^{commit}: fatal: Needed a single revision"
        ),
        "{}",
        row.message
    );
    assert!(!row.message.contains("delivery-ok"), "{}", row.message);
    let remediation = row.remediation.as_deref().expect("warning carries the fix");
    assert!(
        remediation.contains(
            "set `schedule.deliveries_landed.branch` to the workspace base branch `main`"
        ),
        "{remediation}"
    );
    assert!(
        remediation.contains("orbit auto-task update <name>"),
        "{remediation}"
    );

    // Disabling the definition takes it out of this host's obligations.
    runtime
        .auto_task_toggle("delivery-qa", false)
        .expect("disable");
    let results = runtime.doctor_workspace().expect("doctor");
    assert_eq!(
        status_of(&results, "automation-consumers").status,
        WorkspaceDoctorStatus::Ok
    );
}

/// An enabled delivery definition whose resolved owner is not this host is
/// refused at every tick and accumulates debt nothing here can discharge
/// [ORB-12867]. It carries no stall marker and its branch resolves fine, so
/// before this check `orbit doctor` reported `ok` while the definition was
/// silently dead. A definition this host owns, and one the operator disabled,
/// stay quiet.
#[test]
fn automation_consumer_check_warns_about_a_definition_this_host_cannot_admit() {
    use orbit_core::application::auto_tasks::AutoTaskAddParams;
    use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
    use orbit_types::workflow::automation::{CoverageClass, DeliveryTrigger};
    use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};

    let temp = tempfile::tempdir().expect("temp dir");
    let runtime =
        workspace_runtime(&temp).with_automation_machine_identity(Some("fixture-machine".into()));

    // Every definition here names a branch that resolves, so the only debt
    // this check can report is the ownership one.
    let repo = temp.path().join("repo");
    for args in [
        vec!["init", "--initial-branch=main"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(&args)
            .current_dir(&repo)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let template = AutoTaskTemplate {
        title: "Review a frozen delivery batch".into(),
        description: "Inspect captured input".into(),
        acceptance_criteria: vec!["All obligations examined".into()],
        task_type: TaskType::Chore,
        tags: vec![],
        required_tools: vec![],
        priority: TaskPriority::Medium,
        complexity: None,
        crew: None,
        status: TaskStatus::Backlog,
    };
    let add = |name: &str, owner: Option<&str>| {
        runtime
            .auto_task_add(AutoTaskAddParams {
                name: name.into(),
                description: "fixture".into(),
                schedule: AutoTaskSchedule::Deliveries {
                    deliveries_landed: DeliveryTrigger {
                        owner_machine: owner.map(ToOwned::to_owned),
                        branch: "main".into(),
                        threshold: 1,
                        max_wait_minutes: 60,
                        coverage: CoverageClass::LandedCodeReviewV1,
                        max_items: 20,
                        retries: 0,
                    },
                },
                template: template.clone(),
                dedupe: DedupePolicy::SkipIfOpen,
            })
            .expect("add delivery definition");
        runtime.auto_task_toggle(name, true).expect("enable");
    };
    add("delivery-here", Some("fixture-machine"));
    add("delivery-elsewhere", Some("another-machine"));
    add("delivery-unresolved", None);

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "automation-consumers");

    assert_eq!(row.status, WorkspaceDoctorStatus::Warning);
    assert!(
        row.message.contains(
            "2 enabled delivery definition(s) this host can never admit work for, so their \
             coverage debt only grows: delivery-elsewhere (owned_elsewhere: the definition's \
             `owner_machine` names machine `another-machine`, and this host is \
             `fixture-machine`)"
        ),
        "{}",
        row.message
    );
    assert!(
        row.message.contains(
            "delivery-unresolved (ownership_unresolved: no owner machine \
             resolves: the definition sets no `owner_machine` and this workspace has no \
             registered owner machine, and this host is `fixture-machine`)"
        ),
        "unresolved ownership is reported as distinctly as owned_elsewhere: {}",
        row.message
    );
    assert!(
        !row.message.contains("delivery-here"),
        "a definition this host owns is not a debt: {}",
        row.message
    );

    let remediation = row.remediation.as_deref().expect("warning carries the fix");
    assert!(
        remediation.contains("names machine `another-machine`, and this host is `fixture-machine`"),
        "the remediation names the ownership mismatch: {remediation}"
    );
    assert!(
        remediation.contains("orbit auto-task show delivery-elsewhere --preview"),
        "the remediation names the surface that shows the debt: {remediation}"
    );
    assert!(
        remediation.contains("orbit auto-task toggle delivery-elsewhere off"),
        "{remediation}"
    );

    // Disabling them takes them out of this host's obligations: `disabled`
    // already says why nothing fires.
    for name in ["delivery-elsewhere", "delivery-unresolved"] {
        runtime.auto_task_toggle(name, false).expect("disable");
    }
    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "automation-consumers");
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok);
    assert_eq!(row.remediation, None);
}

/// A workspace with no delivery automation state is healthy, not skipped: the
/// check reads persisted stall markers, and having none is the answer.
#[test]
fn automation_consumer_check_passes_without_any_stalled_consumer() {
    let temp = tempfile::tempdir().expect("temp dir");
    let runtime = workspace_runtime(&temp);

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "automation-consumers");

    assert_eq!(row.status, WorkspaceDoctorStatus::Ok);
    assert!(
        row.message
            .contains("no stalled delivery automation consumer"),
        "{}",
        row.message
    );
    assert_eq!(row.remediation, None);
}

#[test]
fn tracked_orbit_files_skips_without_git() {
    let temp = tempfile::tempdir().expect("temp dir");
    let runtime = workspace_runtime(&temp);
    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "tracked-orbit-files");
    assert_eq!(row.status, WorkspaceDoctorStatus::Skipped);
    assert_eq!(row.remediation, None);
}

#[test]
fn tracked_orbit_files_warn_until_untracked() {
    let temp = tempfile::tempdir().expect("temp dir");
    let runtime = workspace_runtime(&temp);
    let repo_root = temp.path().join("repo");
    fs::write(repo_root.join(".orbit").join("config.toml"), "# test\n")
        .expect("write tracked config");

    let git_init = std::process::Command::new("git")
        .args([
            "-C",
            repo_root.to_str().expect("utf8 repo"),
            "init",
            "--quiet",
        ])
        .status()
        .expect("git init");
    assert!(git_init.success(), "initialize git repo");
    let add = std::process::Command::new("git")
        .args([
            "-C",
            repo_root.to_str().expect("utf8 repo"),
            "add",
            ".orbit/config.toml",
        ])
        .status()
        .expect("git add");
    assert!(add.success(), "track .orbit/config.toml");

    let results = runtime.doctor_workspace().expect("doctor");
    let row = status_of(&results, "tracked-orbit-files");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning);
    assert!(row.message.contains("tracked file"), "{}", row.message);
    assert_eq!(
        row.remediation.as_deref(),
        Some("git rm -r --cached .orbit")
    );

    let untrack = std::process::Command::new("git")
        .args([
            "-C",
            repo_root.to_str().expect("utf8 repo"),
            "rm",
            "-r",
            "--cached",
            "--quiet",
            ".orbit",
        ])
        .status()
        .expect("git rm --cached");
    assert!(untrack.success(), "untrack .orbit");

    let after = runtime.doctor_workspace().expect("doctor after untrack");
    let row = status_of(&after, "tracked-orbit-files");
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok);
    assert_eq!(row.message, "no tracked files under .orbit/");
    assert_eq!(row.remediation, None);
}

/// [ORB-12968] A scheduled reboot is a warning naming its mode and time; with
/// none pending the row is ok. The probe is injected, never the real `/run`.
#[test]
fn host_shutdown_check_names_a_scheduled_reboot() {
    use orbit_core::runtime::host_signal::{FixedHostSignals, ScheduledShutdown};

    let temp = tempfile::tempdir().expect("temp dir");
    let quiet = workspace_runtime(&temp)
        .with_host_signal_probe(std::sync::Arc::new(FixedHostSignals::none()));
    let results = quiet.doctor_workspace().expect("doctor");
    assert_eq!(
        status_of(&results, "host-shutdown").status,
        WorkspaceDoctorStatus::Ok
    );

    let scheduled_at = chrono::DateTime::parse_from_rfc3339("2026-09-25T04:00:00Z")
        .expect("time")
        .with_timezone(&Utc);
    let held = quiet.with_host_signal_probe(std::sync::Arc::new(FixedHostSignals::scheduled(
        ScheduledShutdown {
            mode: "reboot".to_string(),
            scheduled_at,
            source: "fixture".to_string(),
        },
    )));
    let results = held.doctor_workspace().expect("doctor");
    let row = status_of(&results, "host-shutdown");
    assert_eq!(row.status, WorkspaceDoctorStatus::Warning);
    assert!(
        row.message
            .contains("host reboot scheduled for 2026-09-25T04:00:00Z"),
        "{}",
        row.message
    );
    assert!(
        row.remediation
            .as_deref()
            .is_some_and(|remediation| remediation.contains("shutdown -c")),
        "{:?}",
        row.remediation
    );
}
