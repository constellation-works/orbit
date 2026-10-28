//! Analytics › Resources: `GET /api/resource-profiles` serves the profiles
//! admission prices candidates with, scoped to the selected workspace, with
//! the host's size and admit thresholds.

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

const GIB: i64 = 1024 * 1024 * 1024;

#[test]
fn resource_profiles_disclose_the_all_selection_limit_with_more_retained_deliveries() {
    isolated(
        "resource_profiles::resource_profiles_disclose_the_all_selection_limit_with_more_retained_deliveries",
        || {
            let fixture = Fixture::new();
            let config = fixture.global.join("config.toml");
            let mut text = std::fs::read_to_string(&config).unwrap();
            text.push_str("\n[machine.auto_concurrency]\nprofile_window = 1500\n");
            std::fs::write(&config, text).unwrap();
            let runtime = admitting_runtime(&fixture);
            let ws = runtime.workspace_id().unwrap();
            let admitted = Utc::now() - chrono::Duration::days(1);
            // All deliveries share one crew model and are inside ordinary
            // age retention. Seed them in one transaction, without live workers.
            runtime.sqlite_store().unwrap().with_transaction(|tx| {
                let db = tx.connection();
                let store = |error: rusqlite::Error| orbit_core::OrbitError::Store(error.to_string());
                let mut run = db.prepare(
                    "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, \
                         scheduled_at, created_at, finished_at) \
                     VALUES (?1, ?2, 'task_auto_pipeline', 1, 'success', ?3, ?3, ?3)",
                ).map_err(store)?;
                let mut minute = db.prepare(
                    "INSERT INTO run_resource_minutes (workspace_id, run_id, minute_index, \
                         minute_started_at, task_id, work_class, complexity, resumed, \
                         delivery_admitted_at, cpu_cores, memory_bytes, samples, cpu_accounting, crew_model, orbit_version) \
                     VALUES (?1, ?2, 0, ?3, ?2, 'change', 'medium', 0, ?3, ?4, ?5, 12, 'linux-cgroup-v1', 'fixture-model', 'fixture-version')",
                ).map_err(store)?;
                for index in 0..1500 {
                    let id = format!("jrun-history-{index}");
                    let stamp = (admitted + chrono::Duration::seconds(index))
                        .to_rfc3339_opts(SecondsFormat::Micros, true);
                    run.execute(rusqlite::params![id, ws, stamp]).map_err(store)?;
                    // The older 500 would visibly change the served mean.
                    let cpu = if index < 500 { 9.0 } else { 2.0 };
                    minute.execute(rusqlite::params![ws, id, stamp, cpu, GIB]).map_err(store)?;
                }
                Ok(())
            }).unwrap();

            let server = fixture.server(false);
            let default = json_ok(server.get("/api/resource-profiles?workspace=ws_http_fixture"));
            assert_eq!(default["profiles"][0]["delivery_count"], json!(1500));

            let all =
                json_ok(server.get("/api/resource-profiles?workspace=ws_http_fixture&last=all"));
            assert_eq!(all["profiles"][0]["delivery_count"], json!(1000));
            assert_eq!(
                all["profiles"][0]["deliveries"].as_array().unwrap().len(),
                1000
            );
            assert_eq!(all["profiles"][0]["series"][0]["cpu_cores"], json!(2.0));
            assert_eq!(all["profiles"][0]["crew_model"], Value::Null);
            assert_eq!(
                all["profiles"][0]["crews"],
                json!([{
                    "crew_model": "fixture-model",
                    "deliveries": 1000,
                    "profile_deliveries": 1000,
                }]),
                "a union reports each crew model's share of its window and own profile"
            );
            assert_eq!(all["last"], json!("all"));
            assert_eq!(all["profile_window"], json!(1500));
            assert_eq!(
                all["delivery_limit"],
                json!(1000),
                "the legacy all selection must disclose its actual per-key cap"
            );
            assert_eq!(all["max_last"], json!(1000));
            assert_eq!(default["last"], Value::Null);
            assert_eq!(default["delivery_limit"], json!(1500));

            for count in [200, 1000] {
                let body = json_ok(server.get(&format!(
                    "/api/resource-profiles?workspace=ws_http_fixture&last={count}"
                )));
                assert_eq!(body["last"], json!(count.to_string()));
                assert_eq!(body["delivery_limit"], json!(count));
                assert_eq!(body["profiles"][0]["delivery_count"], json!(count));
            }
            assert_eq!(
                server
                    .get("/api/resource-profiles?workspace=ws_http_fixture&last=1001")
                    .status(),
                400
            );
            let after = json_ok(server.get("/api/resource-profiles?workspace=ws_http_fixture"));
            assert_eq!(
                after["profiles"], default["profiles"],
                "history selection must leave admission's larger window intact"
            );
        },
    );
}

/// One finished single-run delivery admitted `hours_ago`, one row per minute
/// at `cores` and 1 GiB.
fn delivery(
    runtime: &OrbitRuntime,
    workspace: &str,
    run_id: &str,
    class: &str,
    complexity: &str,
    hours_ago: i64,
    cores: &[f64],
) {
    let admitted: DateTime<Utc> = Utc::now() - chrono::Duration::hours(hours_ago);
    let stamp = |minutes: i64| {
        (admitted + chrono::Duration::minutes(minutes)).to_rfc3339_opts(SecondsFormat::Micros, true)
    };
    let task = format!("task-{run_id}");
    runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            let db = tx.connection();
            let store = |error: rusqlite::Error| orbit_core::OrbitError::Store(error.to_string());
            db.execute(
                "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, \
                     scheduled_at, created_at, finished_at) \
                 VALUES (?1, ?2, 'task_auto_pipeline', 1, 'success', ?3, ?3, ?4)",
                (run_id, workspace, stamp(0), stamp(cores.len() as i64)),
            )
            .map_err(store)?;
            for (index, cpu) in cores.iter().enumerate() {
                db.execute(
                    "INSERT INTO run_resource_minutes (workspace_id, run_id, minute_index, \
                         minute_started_at, task_id, work_class, complexity, resumed, \
                         delivery_admitted_at, cpu_cores, memory_bytes, samples, cpu_accounting) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, 12, 'linux-cgroup-v1')",
                    rusqlite::params![
                        workspace,
                        run_id,
                        index as i64,
                        stamp(index as i64),
                        task,
                        class,
                        complexity,
                        stamp(0),
                        cpu,
                        GIB
                    ],
                )
                .map_err(store)?;
            }
            Ok(())
        })
        .unwrap();
}

/// The fixture workspace reopened, so it reads the config written after the
/// fixture opened its runtime: an admitting drain's view of the host.
fn admitting_runtime(fixture: &Fixture) -> OrbitRuntime {
    let registry = orbit_registry::workspace_registry::load_registry_from(
        &orbit_registry::workspace_registry::registry_path_for(&fixture.global),
    )
    .unwrap();
    orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
        &fixture.global,
        &registry.workspaces[0],
        &registry.checkouts[0],
    )
    .unwrap()
}

#[test]
fn resource_profiles_serve_admissions_profiles_for_the_selected_workspace_with_host_capacity() {
    isolated(
        "resource_profiles::resource_profiles_serve_admissions_profiles_for_the_selected_workspace_with_host_capacity",
        || {
            let fixture = Fixture::new();
            // The machine identity lives in this file too: append, do not replace.
            let config = fixture.global.join("config.toml");
            let mut text = std::fs::read_to_string(&config).unwrap_or_default();
            text.push_str(
                "\n[machine.auto_concurrency]\nadmit_cpu_percent = 80\nadmit_memory_percent = 60\n\
                 min_profile_deliveries = 2\nprofile_window = 5\n",
            );
            std::fs::write(&config, text).unwrap();
            let runtime = admitting_runtime(&fixture);
            let ws = runtime.workspace_id().unwrap();
            delivery(
                &runtime,
                &ws,
                "jrun-m1",
                "change",
                "medium",
                5,
                &[1.0, 3.0, 2.0],
            );
            delivery(&runtime, &ws, "jrun-m2", "change", "medium", 4, &[2.0, 1.0]);
            delivery(
                &runtime,
                &ws,
                "jrun-h1",
                "change",
                "hard",
                3,
                &[4.0, 4.0, 4.0, 4.0],
            );
            delivery(
                &runtime,
                &ws,
                "jrun-q1",
                "auto-task:qa-sweep",
                "low",
                2,
                &[0.5],
            );
            delivery(
                &runtime,
                &ws,
                "jrun-q2",
                "auto-task:qa-sweep",
                "medium",
                1,
                &[0.5, 0.5],
            );
            // Another workspace on the same host keeps its own profiles.
            delivery(
                &runtime,
                "ws_other",
                "jrun-o1",
                "change",
                "medium",
                2,
                &[8.0],
            );
            delivery(
                &runtime,
                "ws_other",
                "jrun-o2",
                "change",
                "medium",
                1,
                &[8.0],
            );

            let server = fixture.server(false);
            let body = json_ok(server.get("/api/resource-profiles?workspace=ws_http_fixture"));

            assert_eq!(body["workspace_id"], json!(ws));
            assert_eq!(body["admit_cpu_percent"], json!(80));
            assert_eq!(body["admit_memory_percent"], json!(60));
            assert_eq!(body["min_profile_deliveries"], json!(2));
            assert_eq!(body["profile_window"], json!(5));
            let host = &body["host"];
            assert!(host["cpu_count"].as_u64().unwrap() > 0, "{host}");
            assert!(host["memory_total_bytes"].as_u64().unwrap() > 0, "{host}");

            // Exactly the profiles admission builds for this workspace, as
            // `orbit run profiles --json` encodes them.
            let admission: Vec<Value> = runtime
                .resource_profiles()
                .unwrap()
                .profiles
                .iter()
                .filter(|profile| profile.key.workspace_id == ws)
                .map(|profile| serde_json::to_value(profile.as_ref()).unwrap())
                .collect();
            let served = body["profiles"].as_array().unwrap();
            assert_eq!(
                served, &admission,
                "the view must chart admission's own profiles"
            );

            let keys: Vec<(String, Value, u64, bool)> = served
                .iter()
                .map(|profile| {
                    (
                        profile["key"]["work_class"].as_str().unwrap().to_string(),
                        profile["key"]["complexity"].clone(),
                        profile["delivery_count"].as_u64().unwrap(),
                        profile["sparse"].as_bool().unwrap(),
                    )
                })
                .collect();
            assert_eq!(
                keys,
                vec![
                    ("auto-task:qa-sweep".into(), Value::Null, 2, false),
                    ("change".into(), json!("hard"), 1, true),
                    ("change".into(), json!("medium"), 2, false),
                ],
                "no profile from another workspace, templates keyed without complexity"
            );
            let medium = &served[2];
            assert!(medium["horizon_minutes"].as_u64().is_some());
            let first = &medium["series"][0];
            for field in [
                "alive",
                "cpu_cores",
                "cpu_stddev",
                "memory_bytes",
                "memory_mean_bytes",
            ] {
                assert!(
                    first[field].is_number(),
                    "series minute lacks {field}: {first}"
                );
            }
            assert_eq!(first["cpu_cores"], json!(1.5));
            assert_eq!(first["cpu_stddev"], json!(0.5));
        },
    );
}
