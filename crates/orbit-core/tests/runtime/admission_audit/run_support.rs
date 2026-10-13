use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_types::workflow::JobRun;
use tempfile::tempdir;
pub(crate) fn test_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

pub(crate) fn insert_pending_run(runtime: &OrbitRuntime, job_id: &str) -> JobRun {
    crate::admission_audit::store_support::jobs(runtime)
        .insert_job_run(
            job_id,
            1,
            Utc::now() - chrono::Duration::seconds(5),
            None,
            None,
        )
        .expect("insert run")
}
