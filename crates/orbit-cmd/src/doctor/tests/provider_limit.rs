//! [ORB-16371] `provider:off` makes `orbit doctor`'s provider-limits row say
//! gating is off while the exhausted reading is still listed.

use chrono::{Duration, Utc};
use orbit_core::{OrbitRuntime, ShipMode, WorkspaceRuntimeBinding};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};

use crate::doctor::{WorkspaceDoctorStatus, provider_limit_findings};

#[test]
fn an_off_override_reports_that_gating_is_off() {
    let root = tempfile::tempdir().expect("tempdir");
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(global.join("resources/jobs")).expect("global");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow]\ndefault_crew = \"sol\"\nprovider_limit_overrides = [\"codex:off\"]\n\n\
         [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n",
    )
    .expect("config");
    let runtime = OrbitRuntime::from_roots_with_binding(
        &global,
        &workspace,
        WorkspaceRuntimeBinding {
            logical_workspace_id: "ws_provider_limit_off".into(),
            task_partition_id: "ws_provider_limit_off".into(),
            owner_machine_id: None,
            checkout_role: None,
            repo_root: repo,
            ship_mode: ShipMode::Local,
            base_branch: Some("main".into()),
        },
    )
    .expect("runtime");
    let resets_at = Utc::now() + Duration::hours(2);
    runtime
        .record_provider_limit(&ProviderLimitObservation {
            provider: "codex".into(),
            model: None,
            window: Some("primary".into()),
            exhausted: true,
            source: ProviderLimitSource::Event,
            resets_at: Some(resets_at),
            used_percent: Some(100.0),
            window_minutes: Some(300),
            gating: true,
            partial: false,
            observed_at: Utc::now(),
            run_id: None,
            crew: None,
            detail: String::new(),
        })
        .expect("record");

    let findings = provider_limit_findings(&runtime);
    let (_, row) = findings
        .iter()
        .find(|(provider, _)| provider.as_deref() == Some("codex"))
        .expect("codex row");
    assert_eq!(row.check_name, "provider-limits:codex");
    assert_eq!(row.status, WorkspaceDoctorStatus::Ok);
    assert!(
        row.message.contains("gating is off") && row.message.contains("primary"),
        "{}",
        row.message
    );
}
