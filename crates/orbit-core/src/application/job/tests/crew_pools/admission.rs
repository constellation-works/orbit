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

/// [ORB-17773] A claimed leaf whose snapshot crew came from an automatic
/// pool is re-drawn on the follower with the follower's own weekly
/// readings: Claude at weekly pressure about 25 (10% used, six hours to
/// reset) decisively outweighs the snapshot's Codex crew. The owner's
/// opposite readings, recorded in the owner's own store, never reach the
/// follower's draw. Under `static`, with an explicit source, and on a
/// legacy snapshot carrying no source, the owner's crew is kept.
#[test]
fn a_claimed_leaf_redraws_an_automatic_crew_on_the_followers_own_pressure() {
    use chrono::Duration;
    use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};

    use crate::runtime::engine::crew::BundleCrews;

    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &a_claimed_leaf_redraws_an_automatic_crew_on_the_followers_own_pressure,
    )) {
        return;
    }
    const TASK: &str = "ORB-17773";
    let week = |provider: &str, used_percent: f64, resets_in: Duration| ProviderLimitObservation {
        provider: provider.into(),
        account: None,
        model: None,
        window: Some("seven_day".into()),
        exhausted: false,
        source: ProviderLimitSource::Event,
        resets_at: Some(Utc::now() + resets_in),
        used_percent: Some(used_percent),
        window_minutes: Some(10080),
        gating: true,
        partial: false,
        observed_at: Utc::now(),
        run_id: None,
        crew: None,
        detail: String::new(),
    };
    // One host: its own roots and provider-limit store, seeded with the
    // weekly readings the case names.
    let host = |extra: &str, readings: &[ProviderLimitObservation]| {
        let root = tempdir().expect("create tempdir");
        let global = root.path().join("global");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).expect("create global root");
        std::fs::create_dir_all(&workspace).expect("create workspace root");
        std::fs::write(
            workspace.join("config.toml"),
            format!(
                "[workflow]\ndefault_crew = \"sol\"\n\
                 hard_complexity_crews = [\"opus\", \"sol\"]\n{extra}\n\
                 [crews.opus]\nprovider = \"claude\"\nmodel = \"opus-model\"\n\n\
                 [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n"
            ),
        )
        .expect("write crew config");
        let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
        for reading in readings {
            runtime
                .record_provider_limit(reading)
                .expect("seed reading");
        }
        (root, runtime)
    };
    let leaf = |source: Option<&str>| {
        let mut claimed = json!({"id": TASK, "crew": "sol", "complexity": "hard"});
        if let Some(source) = source {
            claimed["crew_source"] = json!(source);
        }
        json!({"task_ids": [TASK], "claimed_task": claimed})
    };
    let resolved = |runtime: &OrbitRuntime, source: Option<&str>| {
        runtime
            .resolve_crew_for_run_input(&leaf(source), BundleCrews::Unanimous)
            .expect("resolve the claimed leaf's crew")
            .name
    };

    // The owner's readings favour Codex just as decisively; they live in
    // the owner's store and are never the follower's to read.
    let decisive = "crew_selection = \"usage_pressure\"\n\
                    usage_pressure_cap = 1000000\nusage_pressure_strength = 10";
    let (_owner_root, owner) = host(
        decisive,
        &[
            week("claude", 89.0, Duration::days(7)),
            week("codex", 0.0, Duration::hours(6)),
        ],
    );
    assert_eq!(resolved(&owner, Some("pool:hard")), "sol", "owner fixture");

    let (_follower_root, follower) = host(
        decisive,
        &[
            week("claude", 10.0, Duration::hours(6)),
            week("codex", 89.0, Duration::days(7)),
        ],
    );
    assert_eq!(
        resolved(&follower, Some("pool:hard")),
        "opus",
        "the follower re-draws on its own readings"
    );
    assert_eq!(
        resolved(&follower, Some("pool:hard")),
        "opus",
        "every resolution of the leaf draws the same member"
    );
    assert_eq!(
        resolved(&follower, Some("default")),
        "opus",
        "a default assignment re-draws from its complexity pool, as at owner admission"
    );
    assert_eq!(
        resolved(&follower, Some("explicit")),
        "sol",
        "an explicit crew is never re-drawn"
    );
    assert_eq!(
        resolved(&follower, None),
        "sol",
        "a legacy snapshot without a source keeps the owner's crew"
    );

    let (_static_root, statik) = host(
        "crew_selection = \"static\"",
        &[week("claude", 10.0, Duration::hours(6))],
    );
    assert_eq!(
        resolved(&statik, Some("pool:hard")),
        "sol",
        "under static the claimed crew is unchanged"
    );
}
