mod agent_invoke;
mod crew_pools;
mod pipeline;

/// Deterministic audit-insert fault plus the existing detached-worker seam.
/// The child and automation admission APIs are crate-private, so this fixture
/// keeps their regression coverage at the owning application boundary.
#[cfg(unix)]
struct SubmissionAuditFault {
    _root: tempfile::TempDir,
    runtime: crate::OrbitRuntime,
    job_path: std::path::PathBuf,
    log_path: std::path::PathBuf,
}

#[cfg(unix)]
impl SubmissionAuditFault {
    fn new(job_name: &str, enabled: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("config.toml"),
            "[workflow]\ndefault_crew = \"system\"\n\n[crews.system]\nprovider = \"codex\"\nmodel = \"fixture-model\"\nbackend = \"cli\"\n",
        )
        .unwrap();
        let runtime = crate::OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        let job_path = jobs_dir.join(format!("{job_name}.yaml"));
        let state = if enabled { "enabled" } else { "disabled" };
        std::fs::write(
            &job_path,
            format!(
                "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {job_name}\nspec:\n  state: {state}\n  kind: workflow\n  steps:\n    - id: noop\n      spec:\n        type: deterministic\n        action: sleep\n        config: {{}}\n"
            ),
        )
        .unwrap();
        runtime.sqlite_store().unwrap();
        let db = orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::new(
            &global, &workspace,
        ))
        .unwrap();
        rusqlite::Connection::open(db)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_submission_audit BEFORE INSERT ON audit_events \
                 WHEN NEW.tool_name IN ('pipeline.invoke', 'pipeline.resume', 'agent.invoke') \
                 BEGIN SELECT RAISE(ABORT, 'injected submission audit failure'); END;",
            )
            .unwrap();
        // Never re-execute libtest as a real worker or invoke a provider. A
        // successful spawn still exercises durable admission and supervision.
        crate::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
        let log_path = root.path().join("submission-warnings.jsonl");
        Self {
            _root: root,
            runtime,
            job_path,
            log_path,
        }
    }

    fn capture<T>(&self, submit: impl FnOnce() -> T) -> T {
        let log = std::fs::File::create(&self.log_path).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || log.try_clone().unwrap())
            .finish();
        tracing::subscriber::with_default(subscriber, submit)
    }

    fn assert_warning(&self, run_id: Option<&str>) {
        let events: Vec<serde_json::Value> = std::fs::read_to_string(&self.log_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(
            events.iter().any(|event| {
                event["level"] == "WARN"
                    && event["target"] == "orbit.core.job_run"
                    && event["fields"]["run_id"] == run_id.unwrap_or_default()
                    && event["fields"]["operation"]
                        .as_str()
                        .is_some_and(|op| !op.is_empty())
                    && event["fields"]["error"]
                        .as_str()
                        .is_some_and(|error| error.contains("injected submission audit failure"))
            }),
            "an audit insert failure must emit a warning correlated with the submission: {events:?}"
        );
    }
}
