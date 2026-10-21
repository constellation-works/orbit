use chrono::Utc;
use serde_json::json;
use tempfile::tempdir;

use crate::OrbitRuntime;

/// [ORB-14697] A pool member whose provider reads at or above its usage
/// threshold holds no ticket, and draws again once the reading's reset has
/// passed. The tickets walk every residue of the 100-ticket pool, so a draw
/// that could land on the limited member would.
#[test]
fn a_limited_pool_member_is_never_drawn_until_its_reading_resets() {
    use std::collections::BTreeMap;

    use chrono::{Duration, SubsecRound};
    use orbit_types::task::{TaskComplexity, TaskStatus};
    use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};

    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &a_limited_pool_member_is_never_drawn_until_its_reading_resets,
    )) {
        return;
    }
    let root = tempdir().expect("create tempdir");
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("create global root");
    std::fs::create_dir_all(&workspace).expect("create workspace root");
    let crews = "[crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n\n\
                 [crews.sonnet]\nprovider = \"claude\"\nmodel = \"sonnet-model\"\n";
    let write = |pool: &str| {
        std::fs::write(
            workspace.join("config.toml"),
            format!("[workflow]\ndefault_crew = \"sol\"\n{pool}\n{crews}"),
        )
        .expect("write crew config");
    };
    // Created before the pool exists, the task's crew is the default's, so
    // admission draws it from the whole pool.
    write("");
    let task = OrbitRuntime::from_roots(&global, &workspace)
        .expect("build runtime")
        .add_task(crate::application::task::TaskAddParams {
            title: "Provider limit draw fixture".into(),
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("add task");
    assert_eq!(task.crew_source.as_deref(), Some("default"));
    write("medium_complexity_crews = [\"sol:50\", \"sonnet:50\"]");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");

    let reading = |used_percent: f64, resets_at| ProviderLimitObservation {
        provider: "claude".into(),
        account: None,
        model: None,
        window: Some("five_hour".into()),
        exhausted: false,
        source: ProviderLimitSource::Event,
        resets_at: Some(resets_at),
        used_percent: Some(used_percent),
        window_minutes: Some(300),
        gating: true,
        partial: false,
        observed_at: Utc::now(),
        run_id: None,
        crew: None,
        detail: String::new(),
    };
    let draws = |runtime: &OrbitRuntime| {
        let mut ticket = 16_u64;
        let mut drawn = BTreeMap::<String, usize>::new();
        let mut sources = Vec::new();
        for _ in 0..1000 {
            let mut input = json!({ "task_ids": [task.id] });
            runtime
                .install_auto_crew_admission(
                    "task_local_pipeline",
                    &mut input,
                    None,
                    false,
                    &mut || {
                        ticket += 1;
                        Ok(ticket)
                    },
                )
                .expect("admit");
            *drawn
                .entry(input["crew"].as_str().unwrap_or_default().to_string())
                .or_default() += 1;
            sources.push(input["crew_selection"]["source"].clone());
        }
        (drawn, sources)
    };

    // The store keeps microseconds, so the expected reset must too.
    let resets_at = (Utc::now() + Duration::hours(1)).trunc_subsecs(6);
    runtime
        .record_provider_limit(&reading(93.0, resets_at))
        .expect("seed the reading");
    let (drawn, sources) = draws(&runtime);
    assert_eq!(drawn.keys().collect::<Vec<_>>(), ["sol"], "{drawn:?}");
    let expected = format!(
        "workflow.medium_complexity_crews; provider limit: claude five_hour at 93% (limit 90%) \
         until {}; crews sonnet skipped",
        resets_at.to_rfc3339()
    );
    assert!(
        sources.iter().all(|source| source == &json!(expected)),
        "the selection names the limit: {:?}",
        sources.first()
    );

    runtime
        .record_provider_limit(&reading(93.0, Utc::now() - Duration::minutes(1)))
        .expect("record the reset reading");
    let (drawn, _) = draws(&runtime);
    assert_eq!(
        drawn.keys().collect::<Vec<_>>(),
        ["sol", "sonnet"],
        "{drawn:?}"
    );
    assert_eq!(drawn["sol"], 500, "an even split over the walked tickets");
}
