//! Resource profiles through the public runtime: seeded `run_resource_minutes`
//! and `job_runs` rows become per-key profiles, a lookup walks the fallback
//! chain, and profiles stay cached until a delivery finishes.
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::host_resource::profile::{
    BaseClass, ProfileLookup, ProfileSource, ResourceProfile,
};
use orbit_core::{JobRunState, OrbitRuntime};
use orbit_engine::RuntimeHost;
use orbit_store::Store;
use orbit_store::contracts::{ResourceUsageStoreBackend, RunResourceMinute};
use orbit_types::task::TaskStatus;
use serde_json::json;
use tempfile::TempDir;

const GIB: u64 = 1024 * 1024 * 1024;

/// One run of a delivery: its job, the minute since admission it started,
/// and its cores per minute.
struct RunSeed {
    job: &'static str,
    start_minute: i64,
    cpu: Vec<f64>,
    memory: Vec<u64>,
    finished: bool,
    resumed: bool,
}

impl RunSeed {
    fn finished(job: &'static str, start_minute: i64, cpu: Vec<f64>, memory: Vec<u64>) -> Self {
        Self {
            job,
            start_minute,
            cpu,
            memory,
            finished: true,
            resumed: false,
        }
    }

    /// A one-run delivery of `minutes` minutes at constant load.
    fn flat(minutes: usize, cores: f64, memory: u64) -> Self {
        Self::finished(
            "task_auto_pipeline",
            0,
            vec![cores; minutes],
            vec![memory; minutes],
        )
    }
}

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    store: Store,
    workspace: String,
    /// Admission times are spaced so every delivery is distinct.
    next_admission: std::cell::Cell<i64>,
}

impl Fixture {
    /// A runtime whose global config sets `[machine.auto_concurrency]`.
    fn new(auto_concurrency: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("home/.orbit");
        let workspace = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            global.join("config.toml"),
            format!("[machine.auto_concurrency]\n{auto_concurrency}"),
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        let store = runtime.sqlite_store().unwrap();
        let workspace = runtime.workspace_id().unwrap();
        Self {
            _root: root,
            runtime,
            store,
            workspace,
            next_admission: std::cell::Cell::new(0),
        }
    }

    fn admission(&self) -> DateTime<Utc> {
        let index = self.next_admission.get();
        self.next_admission.set(index + 1);
        Utc::now() - Duration::days(2) + Duration::hours(index)
    }

    /// Seed one delivery of `task` in `workspace` and return its run ids.
    fn delivery(
        &self,
        workspace: &str,
        task: &str,
        class: &str,
        complexity: Option<&str>,
        runs: Vec<RunSeed>,
    ) -> Vec<String> {
        let admitted = self.admission();
        runs.into_iter()
            .map(|seed| self.run(workspace, task, class, complexity, admitted, seed))
            .collect()
    }

    fn run(
        &self,
        workspace: &str,
        task: &str,
        class: &str,
        complexity: Option<&str>,
        admitted: DateTime<Utc>,
        seed: RunSeed,
    ) -> String {
        let started = admitted + Duration::minutes(seed.start_minute);
        let mut run = self
            .runtime
            .insert_job_run(seed.job, 1, started, Some(json!({"task_id": task})), None)
            .unwrap();
        run.created_at = started;
        if seed.finished {
            run.state = JobRunState::Success;
            run.finished_at = Some(started + Duration::minutes(seed.cpu.len() as i64));
        } else {
            run.state = JobRunState::Running;
        }
        self.store
            .upsert_job_run_for_workspace(workspace, &run, None)
            .unwrap();
        for (index, (cpu, memory)) in seed.cpu.iter().zip(&seed.memory).enumerate() {
            self.store
                .record_run_resource_minute(&RunResourceMinute {
                    workspace_id: workspace.to_string(),
                    run_id: run.run_id.clone(),
                    minute_index: index as u32,
                    minute_started_at: started + Duration::minutes(index as i64),
                    task_id: task.to_string(),
                    work_class: class.to_string(),
                    complexity: complexity.map(str::to_string),
                    resumed: seed.resumed,
                    delivery_admitted_at: Some(admitted),
                    cpu_cores: *cpu,
                    memory_bytes: *memory,
                    samples: 12,
                })
                .unwrap();
        }
        run.run_id
    }

    /// Mark a seeded live run finished `minutes` after it started.
    fn finish(&self, workspace: &str, run_id: &str, minutes: i64) {
        let mut run = self.runtime.get_job_run(run_id).unwrap().unwrap();
        run.state = JobRunState::Success;
        run.finished_at = Some(run.created_at + Duration::minutes(minutes));
        self.store
            .upsert_job_run_for_workspace(workspace, &run, None)
            .unwrap();
    }

    fn profile(&self, class: &str, complexity: Option<&str>) -> Arc<ResourceProfile> {
        self.profile_in(&self.workspace, class, complexity)
    }

    fn profile_in(
        &self,
        workspace: &str,
        class: &str,
        complexity: Option<&str>,
    ) -> Arc<ResourceProfile> {
        let profiles = self.runtime.resource_profiles().unwrap();
        profiles
            .profiles
            .iter()
            .find(|profile| {
                profile.key.workspace_id == workspace
                    && profile.key.work_class == class
                    && profile.key.complexity.as_deref() == complexity
            })
            .cloned()
            .unwrap_or_else(|| panic!("no profile for {workspace} {class} {complexity:?}"))
    }

    fn template_task(&self, template: &str, no_diff: bool) -> String {
        let mut tags = vec![format!("auto-task:{template}")];
        if no_diff {
            tags.push("no-diff-expected".to_string());
        }
        self.runtime
            .add_task(TaskAddParams {
                title: format!("{template} fixture"),
                description: "Profile fixture task.".into(),
                status: Some(TaskStatus::Backlog),
                tags,
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    }
}

/// Ten deliveries lasting one to ten minutes, delivery `i` at `i + 1` cores
/// and GiB throughout: at minute `m` the deliveries `m..10` are alive.
#[test]
fn a_profile_holds_alive_mean_cpu_p90_memory_and_a_capped_horizon() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::a_profile_holds_alive_mean_cpu_p90_memory_and_a_capped_horizon",
    ) {
        return;
    }
    let fixture = Fixture::new("horizon_cap_minutes = 8\n");
    let workspace = fixture.workspace.clone();
    for index in 0..10_u64 {
        let load = (index + 1) as f64;
        fixture.delivery(
            &workspace,
            &format!("ORB-{index}"),
            "change",
            Some("hard"),
            vec![RunSeed::flat(index as usize + 1, load, (index + 1) * GIB)],
        );
    }
    // Eleven deliveries: ten of three minutes and one of five, so alive falls
    // to 1/11, below 0.1, at minute 3.
    for index in 0..11 {
        let minutes = if index == 0 { 5 } else { 3 };
        fixture.delivery(
            &workspace,
            &format!("ORB-short-{index}"),
            "change",
            Some("small"),
            vec![RunSeed::flat(minutes, 1.0, GIB)],
        );
    }

    let profile = fixture.profile("change", Some("hard"));
    assert_eq!(profile.delivery_count, 10);
    assert_eq!(profile.series.len(), 10);
    for (minute, point) in profile.series.iter().enumerate() {
        let alive_count = 10 - minute;
        assert_eq!(point.minute as usize, minute);
        assert!(
            (point.alive - alive_count as f64 / 10.0).abs() < 1e-9,
            "alive at {minute}: {}",
            point.alive
        );
        let mean = (minute + 1 + 10) as f64 / 2.0;
        assert!(
            (point.cpu_cores - mean).abs() < 1e-9,
            "mean cpu at {minute}: {}",
            point.cpu_cores
        );
        // Nearest-rank p90 of the alive deliveries' GiB `minute + 1 ..= 10`.
        let rank = (0.9 * alive_count as f64).ceil() as usize;
        assert_eq!(
            point.memory_bytes,
            (minute + rank) as u64 * GIB,
            "p90 memory at {minute}"
        );
    }
    assert_eq!(
        profile.horizon_minutes, 8,
        "alive stays at or above 0.1 for all ten minutes; the cap of 8 applies"
    );
    assert_eq!(profile.peak_cpu_cores.minute, 7);
    assert!((profile.peak_cpu_cores.value - 9.0).abs() < 1e-9);
    assert!(profile.cpu_spread > 0.0);

    let short = fixture.profile("change", Some("small"));
    assert_eq!(short.horizon_minutes, 3, "alive is 1/11 at minute 3");
    assert!(
        short.cpu_spread.abs() < 1e-9,
        "every delivery used one core"
    );
}

/// A delivery is every run of its task sharing one admission time, summed per
/// minute on the admission clock; resumed and live deliveries are left out.
#[test]
fn a_delivery_sums_its_run_chain_from_admission_and_skips_resumed_and_live_ones() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::a_delivery_sums_its_run_chain_from_admission_and_skips_resumed_and_live_ones",
    ) {
        return;
    }
    let fixture = Fixture::new("");
    let workspace = fixture.workspace.clone();
    let runs = fixture.delivery(
        &workspace,
        "ORB-chain",
        "change",
        Some("medium"),
        vec![
            RunSeed::finished("task_auto_pipeline", 0, vec![0.5; 6], vec![GIB; 6]),
            RunSeed::finished("task_gate_pipeline", 1, vec![1.0; 5], vec![GIB; 5]),
            RunSeed::finished("task_pr_pipeline", 3, vec![2.0, 2.0], vec![2 * GIB; 2]),
        ],
    );
    fixture.delivery(
        &workspace,
        "ORB-resumed",
        "change",
        Some("medium"),
        vec![RunSeed {
            resumed: true,
            ..RunSeed::flat(3, 8.0, 8 * GIB)
        }],
    );
    fixture.delivery(
        &workspace,
        "ORB-live",
        "change",
        Some("medium"),
        vec![RunSeed {
            finished: false,
            ..RunSeed::flat(3, 8.0, 8 * GIB)
        }],
    );

    let profile = fixture.profile("change", Some("medium"));
    assert_eq!(profile.delivery_count, 1, "{:?}", profile.deliveries);
    let delivery = &profile.deliveries[0];
    assert_eq!(delivery.task_id, "ORB-chain");
    let mut expected_runs = runs.clone();
    expected_runs.sort();
    assert_eq!(delivery.run_ids, expected_runs);
    assert_eq!(delivery.minutes, 6);
    let cpu: Vec<f64> = profile.series.iter().map(|m| m.cpu_cores).collect();
    assert_eq!(cpu, vec![0.5, 1.5, 1.5, 3.5, 3.5, 1.5]);
    let memory: Vec<u64> = profile.series.iter().map(|m| m.memory_bytes).collect();
    assert_eq!(
        memory,
        vec![GIB, 2 * GIB, 2 * GIB, 4 * GIB, 4 * GIB, 2 * GIB]
    );
}

/// Each lookup names the step that answered. A sparse `no-diff` key borrows
/// no-diff profiles before any heavier `change` profile; a template ignores
/// complexity and falls back to its base class.
#[test]
fn lookups_walk_the_fallback_chain_and_keep_no_diff_work_off_change_profiles() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::lookups_walk_the_fallback_chain_and_keep_no_diff_work_off_change_profiles",
    ) {
        return;
    }
    let fixture = Fixture::new("min_profile_deliveries = 2\n");
    let empty = fixture
        .runtime
        .resource_profile_for(&ProfileLookup::new(
            "ws_new",
            "no-diff",
            Some("hard".into()),
            false,
        ))
        .unwrap();
    assert_eq!(empty.source, ProfileSource::None);
    assert!(empty.profile.is_none());

    let here = fixture.workspace.clone();
    let docs_audit = fixture.template_task("docs-audit", true);
    let curation = fixture.template_task("friction-curation", true);
    for (workspace, task, class, complexity, deliveries, cores) in [
        (
            here.as_str(),
            "ORB-nd-hard",
            "no-diff",
            Some("hard"),
            1,
            0.2,
        ),
        (
            here.as_str(),
            "ORB-nd-medium",
            "no-diff",
            Some("medium"),
            2,
            0.3,
        ),
        (here.as_str(), "ORB-change", "change", Some("hard"), 2, 4.0),
        ("ws_other", "ORB-x-nd", "no-diff", Some("small"), 2, 0.9),
        (
            here.as_str(),
            docs_audit.as_str(),
            "auto-task:docs-audit",
            Some("hard"),
            1,
            0.4,
        ),
        (
            here.as_str(),
            docs_audit.as_str(),
            "auto-task:docs-audit",
            Some("small"),
            1,
            0.4,
        ),
        (
            here.as_str(),
            curation.as_str(),
            "auto-task:friction-curation",
            None,
            1,
            0.1,
        ),
    ] {
        for _ in 0..deliveries {
            fixture.delivery(
                workspace,
                task,
                class,
                complexity,
                vec![RunSeed::flat(3, cores, GIB)],
            );
        }
    }
    let lookup = |workspace: &str, class: &str, complexity: Option<&str>, no_diff: bool| {
        fixture
            .runtime
            .resource_profile_for(&ProfileLookup::new(
                workspace,
                class,
                complexity.map(str::to_string),
                no_diff,
            ))
            .unwrap()
    };

    let exact = lookup(&here, "change", Some("hard"), false);
    assert_eq!(exact.source, ProfileSource::Exact);
    assert_eq!(exact.profile.unwrap().key.work_class, "change");

    let template = lookup(&here, "auto-task:docs-audit", Some("large"), true);
    assert_eq!(
        template.source,
        ProfileSource::Exact,
        "a template key ignores complexity"
    );
    let template = template.profile.unwrap();
    assert_eq!(template.key.complexity, None);
    assert_eq!(template.delivery_count, 2);
    assert_eq!(
        template.base_class,
        BaseClass::NoDiff,
        "its tasks are no-diff-expected"
    );

    let dropped = lookup(&here, "no-diff", Some("hard"), false);
    assert_eq!(dropped.source, ProfileSource::WithoutComplexity);
    let dropped = dropped.profile.unwrap();
    assert_eq!(
        (dropped.key.work_class.as_str(), dropped.delivery_count),
        ("no-diff", 6)
    );

    let base = lookup(&here, "auto-task:friction-curation", None, true);
    assert_eq!(base.source, ProfileSource::BaseClass);
    let base = base.profile.unwrap();
    assert_eq!(
        (base.key.workspace_id.as_str(), base.key.work_class.as_str()),
        (here.as_str(), "no-diff")
    );

    let borrowed = lookup("ws_new", "no-diff", Some("hard"), false);
    assert_eq!(borrowed.source, ProfileSource::HeaviestOfClass);
    let borrowed = borrowed.profile.unwrap();
    assert_eq!(
        (
            borrowed.key.workspace_id.as_str(),
            borrowed.key.work_class.as_str()
        ),
        ("ws_other", "no-diff"),
        "the heaviest no-diff profile, not the far heavier change one"
    );

    let new_template = lookup("ws_new", "auto-task:new-template", None, false);
    assert_eq!(new_template.source, ProfileSource::HeaviestOfClass);
    assert_eq!(new_template.profile.unwrap().key.work_class, "change");
}

/// Template profiles on another workspace answer step 3, even when opposite
/// class history is much heavier. The lighter template keeps the base-class
/// rollup below the heavier template, so this also exercises template ranking.
#[test]
fn host_fallback_uses_template_history_with_or_without_opposite_class_history() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::host_fallback_uses_template_history_with_or_without_opposite_class_history",
    ) {
        return;
    }
    for with_change in [false, true] {
        let fixture = Fixture::new("min_profile_deliveries = 2\n");
        for (template, cores) in [("delivery-code-review", 1.0), ("docs-audit", 0.1)] {
            let task = fixture.template_task(template, true);
            for _ in 0..2 {
                fixture.delivery(
                    "ws_other",
                    &task,
                    &format!("auto-task:{template}"),
                    None,
                    vec![RunSeed::flat(1, cores, GIB)],
                );
            }
        }
        if with_change {
            let task = fixture.template_task("building-template", false);
            for class in ["change", "auto-task:building-template"] {
                for _ in 0..2 {
                    fixture.delivery(
                        "ws_change",
                        &task,
                        class,
                        None,
                        vec![RunSeed::flat(1, 100.0, 10 * GIB)],
                    );
                }
            }
        }
        if with_change {
            // A sparse local key must not prevent borrowing usable host history.
            fixture.delivery(
                &fixture.workspace,
                "ORB-sparse-local",
                "no-diff",
                Some("hard"),
                vec![RunSeed::flat(1, 200.0, GIB)],
            );
        }
        for class in ["no-diff", "auto-task:delivery-code-review"] {
            let answer = fixture
                .runtime
                .resource_profile_for(&ProfileLookup::new(
                    &fixture.workspace,
                    class,
                    Some("hard".into()),
                    true,
                ))
                .unwrap();
            assert_eq!(answer.source, ProfileSource::HeaviestOfClass);
            let profile = answer.profile.unwrap();
            assert_eq!(profile.key.workspace_id, "ws_other");
            assert_eq!(profile.key.work_class, "auto-task:delivery-code-review");
            assert_eq!(profile.base_class, BaseClass::NoDiff);
            assert!(!profile.sparse);
        }
    }
}

/// Sparse same-class templates still fall through to step 4; a populated
/// Change-based template also cannot answer a NoDiff same-class fallback.
#[test]
fn sparse_or_opposite_class_templates_do_not_answer_the_same_class_fallback() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::sparse_or_opposite_class_templates_do_not_answer_the_same_class_fallback",
    ) {
        return;
    }
    for no_diff in [true, false] {
        let fixture = Fixture::new("min_profile_deliveries = 2\n");
        let task = fixture.template_task("docs-audit", no_diff);
        let deliveries = if no_diff { 1 } else { 2 };
        for _ in 0..deliveries {
            fixture.delivery(
                "ws_other",
                &task,
                "auto-task:docs-audit",
                None,
                vec![RunSeed::flat(1, 1.0, GIB)],
            );
        }
        let answer = fixture
            .runtime
            .resource_profile_for(&ProfileLookup::new(
                &fixture.workspace,
                "no-diff",
                None,
                false,
            ))
            .unwrap();
        assert_eq!(answer.source, ProfileSource::HeaviestOnHost);
        let profile = answer.profile.unwrap();
        assert_eq!(profile.sparse, no_diff);
        assert_eq!(
            profile.base_class,
            if no_diff {
                BaseClass::NoDiff
            } else {
                BaseClass::Change
            }
        );
    }
}

/// Templates compete with class profiles using core-minutes first, then
/// memory peak when CPU ties.
#[test]
fn host_fallback_ranks_templates_and_class_profiles_by_cpu_then_memory() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::host_fallback_ranks_templates_and_class_profiles_by_cpu_then_memory",
    ) {
        return;
    }
    for (cores, memory, template_wins) in [
        (1.0, 8 * GIB, false),
        (2.0, 3 * GIB, false),
        (2.0, 5 * GIB, true),
        (3.0, GIB, true),
    ] {
        let fixture = Fixture::new("min_profile_deliveries = 2\n");
        let template = fixture.template_task("docs-audit", true);
        let lighter = fixture.template_task("friction-curation", true);
        for _ in 0..2 {
            for (workspace, task, class, cpu, mem) in [
                ("ws_class", "ORB-class", "no-diff", 2.0, 4 * GIB),
                (
                    "ws_template",
                    template.as_str(),
                    "auto-task:docs-audit",
                    cores,
                    memory,
                ),
                (
                    "ws_template",
                    lighter.as_str(),
                    "auto-task:friction-curation",
                    0.1,
                    GIB,
                ),
            ] {
                fixture.delivery(
                    workspace,
                    task,
                    class,
                    None,
                    vec![RunSeed::flat(1, cpu, mem)],
                );
            }
        }
        let answer = fixture
            .runtime
            .resource_profile_for(&ProfileLookup::new("ws_new", "no-diff", None, false))
            .unwrap();
        assert_eq!(answer.source, ProfileSource::HeaviestOfClass);
        let profile = answer.profile.unwrap();
        assert_eq!(
            profile.key.work_class,
            if template_wins {
                "auto-task:docs-audit"
            } else {
                "no-diff"
            }
        );
        assert_eq!(
            profile.expected_core_minutes,
            if template_wins { cores } else { 2.0 }
        );
        assert_eq!(
            profile.peak_memory_bytes.value,
            if template_wins { memory } else { 4 * GIB }
        );
    }
}

/// Different sparse templates of one base class can together populate the
/// local rollup. The opposite class never contributes deliveries to it.
#[test]
fn local_base_class_rollups_include_template_only_history() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::local_base_class_rollups_include_template_only_history",
    ) {
        return;
    }
    let fixture = Fixture::new("min_profile_deliveries = 2\n");
    for (template, no_diff, cores) in [
        ("docs-audit", true, 1.0),
        ("friction-curation", true, 3.0),
        ("building-template", false, 100.0),
    ] {
        let task = fixture.template_task(template, no_diff);
        fixture.delivery(
            &fixture.workspace,
            &task,
            &format!("auto-task:{template}"),
            None,
            vec![RunSeed::flat(1, cores, GIB)],
        );
        assert!(
            fixture
                .profile(&format!("auto-task:{template}"), None)
                .sparse
        );
    }
    for (class, source) in [
        ("auto-task:docs-audit", ProfileSource::BaseClass),
        ("auto-task:new-template", ProfileSource::BaseClass),
        ("no-diff", ProfileSource::WithoutComplexity),
    ] {
        let answer = fixture
            .runtime
            .resource_profile_for(&ProfileLookup::new(&fixture.workspace, class, None, true))
            .unwrap();
        assert_eq!(answer.source, source);
        let profile = answer.profile.unwrap();
        assert_eq!(profile.key.workspace_id, fixture.workspace);
        assert_eq!(profile.key.work_class, "no-diff");
        assert_eq!(profile.base_class, BaseClass::NoDiff);
        assert_eq!(profile.delivery_count, 2);
        assert_eq!(profile.expected_core_minutes, 2.0);
        assert!(!profile.sparse);
    }
}

/// Step 4 answers only when the base class has no profile anywhere, and takes
/// the heaviest even when every profile is sparse.
#[test]
fn a_class_with_no_profile_borrows_the_heaviest_on_the_host() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::a_class_with_no_profile_borrows_the_heaviest_on_the_host",
    ) {
        return;
    }
    let fixture = Fixture::new("");
    let workspace = fixture.workspace.clone();
    fixture.delivery(
        &workspace,
        "ORB-one",
        "change",
        Some("hard"),
        vec![RunSeed::flat(4, 2.0, GIB)],
    );
    let answer = fixture
        .runtime
        .resource_profile_for(&ProfileLookup::new(&workspace, "no-diff", None, false))
        .unwrap();
    assert_eq!(answer.source, ProfileSource::HeaviestOnHost);
    let profile = answer.profile.unwrap();
    assert!(profile.sparse);
    assert_eq!(profile.key.work_class, "change");
}

/// Profiles are reused across lookups and runtime clones while nothing
/// finishes; new minutes of a live delivery change nothing until it does.
#[test]
fn profiles_stay_cached_until_a_delivery_finishes() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::profiles_stay_cached_until_a_delivery_finishes",
    ) {
        return;
    }
    let fixture = Fixture::new("");
    let workspace = fixture.workspace.clone();
    fixture.delivery(
        &workspace,
        "ORB-done",
        "change",
        Some("hard"),
        vec![RunSeed::flat(2, 1.0, GIB)],
    );
    let first = fixture.runtime.resource_profiles().unwrap();
    assert!(Arc::ptr_eq(
        &first,
        &fixture.runtime.resource_profiles().unwrap()
    ));
    assert!(Arc::ptr_eq(
        &first,
        &fixture.runtime.clone().resource_profiles().unwrap()
    ));

    let live = fixture.delivery(
        &workspace,
        "ORB-live",
        "change",
        Some("hard"),
        vec![RunSeed {
            finished: false,
            ..RunSeed::flat(3, 1.0, GIB)
        }],
    );
    assert!(
        Arc::ptr_eq(&first, &fixture.runtime.resource_profiles().unwrap()),
        "a live delivery's minutes do not rebuild profiles"
    );

    fixture.finish(&workspace, &live[0], 3);
    let rebuilt = fixture.runtime.resource_profiles().unwrap();
    assert!(!Arc::ptr_eq(&first, &rebuilt));
    assert_eq!(fixture.profile("change", Some("hard")).delivery_count, 2);
}

/// No-diff profiles, and templates whose tasks are no-diff, flag deliveries
/// over one core for two minutes in a row; a template that builds by design
/// and change work are never flagged.
#[test]
fn no_diff_profiles_flag_build_like_deliveries_except_templates_that_build_by_design() {
    if !super::dispatch_admission::isolated(
        "resource_profiles::no_diff_profiles_flag_build_like_deliveries_except_templates_that_build_by_design",
    ) {
        return;
    }
    let fixture = Fixture::new("");
    let workspace = fixture.workspace.clone();
    let building = vec![0.2, 2.5, 3.0, 0.2];
    let spiky = vec![0.2, 2.5, 0.2, 2.5];
    let flagged = fixture.delivery(
        &workspace,
        "ORB-review",
        "no-diff",
        Some("medium"),
        vec![RunSeed::finished(
            "task_claimed_pr_pipeline",
            0,
            building.clone(),
            vec![GIB; 4],
        )],
    );
    fixture.delivery(
        &workspace,
        "ORB-spike",
        "no-diff",
        Some("medium"),
        vec![RunSeed::finished(
            "task_claimed_pr_pipeline",
            0,
            spiky,
            vec![GIB; 4],
        )],
    );
    let audit = fixture.template_task("docs-audit", true);
    fixture.delivery(
        &workspace,
        &audit,
        "auto-task:docs-audit",
        None,
        vec![RunSeed::finished(
            "task_auto_pipeline",
            0,
            building.clone(),
            vec![GIB; 4],
        )],
    );
    let qa = fixture.template_task("qa-sweep", true);
    fixture.delivery(
        &workspace,
        &qa,
        "auto-task:qa-sweep",
        None,
        vec![RunSeed::finished(
            "task_auto_pipeline",
            0,
            building.clone(),
            vec![GIB; 4],
        )],
    );
    fixture.delivery(
        &workspace,
        "ORB-change",
        "change",
        Some("medium"),
        vec![RunSeed::finished(
            "task_auto_pipeline",
            0,
            building,
            vec![GIB; 4],
        )],
    );

    let review = fixture.profile("no-diff", Some("medium"));
    assert_eq!(
        review.build_like.len(),
        1,
        "a one-minute spike is not a build"
    );
    let flag = &review.build_like[0];
    assert_eq!(flag.task_id, "ORB-review");
    assert_eq!(flag.run_ids, flagged);
    assert_eq!(flag.from_minute, 1);
    assert!((flag.peak_cpu_cores - 3.0).abs() < 1e-9);

    let audit_profile = fixture.profile("auto-task:docs-audit", None);
    assert_eq!(audit_profile.base_class, BaseClass::NoDiff);
    assert_eq!(audit_profile.build_like.len(), 1);
    assert_eq!(audit_profile.build_like[0].task_id, audit);

    let profiles = fixture.runtime.resource_profiles().unwrap();
    let rollup = profiles
        .class_profiles
        .iter()
        .find(|profile| profile.key.work_class == "no-diff")
        .unwrap();
    assert_eq!(rollup.build_like.len(), 2);
    assert!(
        rollup
            .build_like
            .iter()
            .all(|delivery| delivery.task_id != qa)
    );

    let qa_profile = fixture.profile("auto-task:qa-sweep", None);
    assert_eq!(qa_profile.base_class, BaseClass::NoDiff);
    assert!(
        qa_profile.build_like.is_empty(),
        "qa-sweep is declared to build by design"
    );
    assert!(
        fixture
            .profile("change", Some("medium"))
            .build_like
            .is_empty()
    );
}
