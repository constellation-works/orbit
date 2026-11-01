//! Script-step environment separation [ORB-20661].

use std::path::PathBuf;

use orbit_engine::ScriptStepRequest;
use orbit_types::workflow::activity_job::{ScriptGrant, ScriptGrantLimits};

use crate::application::job::script_step::{script_env, script_launcher_env_from};

fn env_value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// Regression for jrun-20261101-0131-t1: on dk-server-2 every contained
/// script step exited 1 in ~2 ms because the `systemd-run --user` launcher
/// was started from an allowlisted environment carrying neither user-bus
/// locator, while availability had been probed in the Orbit process's own
/// environment. The launcher must receive both locators (when the parent
/// holds them); the script's own environment must receive neither.
#[test]
fn script_launcher_env_carries_user_bus_locators_but_script_env_does_not() {
    let parent: Vec<(String, String)> = [
        ("PATH", "/usr/bin:/bin"),
        ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
        ("DATABASE_URL", "postgres://not-admitted"),
    ]
    .iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect();

    let launcher_env = script_launcher_env_from(&parent);
    assert_eq!(
        env_value(&launcher_env, "XDG_RUNTIME_DIR"),
        Some("/run/user/1000"),
        "jrun-20261101-0131-t1: the systemd-run launcher must receive \
         XDG_RUNTIME_DIR or it cannot reach the user manager"
    );
    assert_eq!(
        env_value(&launcher_env, "DBUS_SESSION_BUS_ADDRESS"),
        Some("unix:path=/run/user/1000/bus"),
        "jrun-20261101-0131-t1: the systemd-run launcher must receive \
         DBUS_SESSION_BUS_ADDRESS or it cannot reach the user manager"
    );
    assert_eq!(
        env_value(&launcher_env, "DATABASE_URL"),
        None,
        "the launcher environment stays allowlisted: unadmitted ambient \
         variables must not cross to the launcher"
    );

    // A locator the parent does not hold is simply absent, never invented.
    let partial: Vec<(String, String)> = [("XDG_RUNTIME_DIR", "/run/user/1000")]
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let launcher_env = script_launcher_env_from(&partial);
    assert_eq!(
        env_value(&launcher_env, "XDG_RUNTIME_DIR"),
        Some("/run/user/1000")
    );
    assert_eq!(env_value(&launcher_env, "DBUS_SESSION_BUS_ADDRESS"), None);

    let request = ScriptStepRequest {
        run_id: "run-1".to_string(),
        step_id: "step-1".to_string(),
        script: "fixture".to_string(),
        input: serde_json::json!({}),
        workspace_path: None,
        timeout_seconds: None,
    };
    let grant = ScriptGrant {
        grant_id: "sg-fixture".to_string(),
        workspace_id: "ws-fixture".to_string(),
        name: "fixture".to_string(),
        digest: "digest".to_string(),
        manifest_digest: "manifest".to_string(),
        entry_digest: "entry".to_string(),
        interpreter_paths: vec![PathBuf::from("/usr/bin/python3")],
        network: Default::default(),
        read_roots: Vec::new(),
        write_roots: Vec::new(),
        env_pass: Vec::new(),
        limits: ScriptGrantLimits::default(),
        host_role: Default::default(),
        grantor: "fixture".to_string(),
        granted_at: "2026-11-01T00:00:00Z".to_string(),
        provenance: "fixture".to_string(),
        expires_at: None,
    };
    let script_env = script_env(&request, "digest", &grant);
    for name in ["XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"] {
        assert_eq!(
            env_value(&script_env, name),
            None,
            "jrun-20261101-0131-t1: {name} is for the systemd-run launcher \
             only and must never reach the script's own environment"
        );
    }
}

// Public engine/filer regression with a real Node adapter and the real
// declared-output publisher. Only the fixture script launcher is replaced;
// ORBIT_UX_EVIDENCE_CONTAINED=1 selects the unmodified sandbox launcher for
// parent-owned QA. No production runner has a bypass for this test seam.
// Unit admission: fail-closed provenance and path safety at the public engine
// and runtime boundary, using the crate-private output publisher only as a
// fixture. A crate-root replay would need the unavailable nested kernel sandbox.
// Regression: the adapter removed screenshots before the findings filer read them.
#[cfg(target_os = "linux")]
struct EvidencePipeline<'a> {
    runtime: &'a crate::OrbitRuntime,
    repo: &'a std::path::Path,
    grant: &'a ScriptGrant,
}

#[cfg(target_os = "linux")]
impl orbit_engine::test_support::NullRuntimeHost for EvidencePipeline<'_> {
    fn run_script_step(&self, request: &ScriptStepRequest) -> orbit_engine::ScriptStepRun {
        use orbit_engine::{ScriptStepEnd, ScriptStepOutputs, ScriptStepRan, ScriptStepRun};
        use std::io::Write;
        use std::process::{Command, Stdio};
        if std::env::var_os("ORBIT_UX_EVIDENCE_CONTAINED").is_some() {
            return orbit_engine::RuntimeHost::run_script_step(self.runtime, request);
        }
        let scratch = tempfile::tempdir_in(self.repo.join(".orbit/tmp")).unwrap();
        let out = scratch.path().join("out");
        let tmp = scratch.path().join("tmp");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        let mut child = Command::new(&self.grant.interpreter_paths[0])
            .arg(self.repo.join("scripts/ux-rules/step.mjs"))
            .current_dir(self.repo)
            .env_clear()
            .env("HOME", scratch.path())
            .env("TMPDIR", &tmp)
            .env("ORBIT_SCRIPT_OUT", &out)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&request.input).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_dir(&tmp).unwrap().count(),
            0,
            "adapter removed surface scratch"
        );
        let mut ran = ScriptStepRan {
            end: ScriptStepEnd::Exited(0),
            stdout: output.stdout,
            stdout_over_cap: false,
            output_schema: serde_json::Value::Null,
            log: output.stderr,
            record: orbit_types::workflow::activity_job::ScriptExecutionRecord::default(),
            outputs: ScriptStepOutputs::None,
        };
        crate::application::job::script_outputs::keep_declared_outputs(
            self.runtime,
            &orbit_types::workflow::activity_job::ScriptFileOutputs {
                max_files: Some(64),
                max_file_bytes: Some(1048576),
            },
            self.grant,
            request,
            &out,
            &mut ran,
        );
        assert!(
            matches!(ran.outputs, ScriptStepOutputs::Kept(_)),
            "{:?}",
            ran.outputs
        );
        scratch.close().unwrap();
        ScriptStepRun::Ran(Box::new(ran))
    }

    fn run_deterministic(
        &self,
        action: &str,
        config: &serde_json::Value,
        input: &serde_json::Value,
        context: orbit_tools::ToolContext,
    ) -> Result<serde_json::Value, orbit_engine::DispatchError> {
        orbit_engine::RuntimeHost::run_deterministic(self.runtime, action, config, input, context)
    }

    fn tool_context_for_activity(
        &self,
        run: Option<&str>,
        profile: Option<&str>,
        audit: Option<std::sync::Arc<dyn orbit_tools::FsAuditLogger>>,
        programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_engine::RuntimeHost::tool_context_for_activity(
            self.runtime,
            run,
            profile,
            audit,
            programs,
        )
    }

    fn checkpoint_step(
        &self,
        run: &str,
        index: u32,
        step: &str,
        output: &serde_json::Value,
        compound: &std::collections::BTreeMap<String, serde_json::Value>,
        sets: &std::collections::BTreeMap<
            String,
            orbit_types::workflow::activity_job::ScriptOutputSet,
        >,
    ) -> Result<(), orbit_engine::DispatchError> {
        orbit_engine::RuntimeHost::checkpoint_step(
            self.runtime,
            run,
            index,
            step,
            output,
            compound,
            sets,
        )
    }

    fn verify_script_output_set(
        &self,
        run: &str,
        set: &orbit_types::workflow::activity_job::ScriptOutputSet,
    ) -> Result<(), String> {
        orbit_engine::RuntimeHost::verify_script_output_set(self.runtime, run, set)
    }
}

#[test]
#[cfg(target_os = "linux")]
fn ux_evidence_survives_script_job_filer_cleanup_and_checkpoint_reuse() {
    use orbit_engine::{RuntimeHost, V2AuditWriter, execute_job_with_resume};
    use orbit_types::workflow::{JobRunState, PipelineState};
    use serde_json::{Value, json};
    use std::sync::Arc;
    if crate::application::tests::run_isolated_test(std::any::type_name_of_val(
        &ux_evidence_survives_script_job_filer_cleanup_and_checkpoint_reuse,
    )) {
        return;
    }
    let source_root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../..")
        .canonicalize()
        .unwrap();
    let scratch_root = source_root.join(".orbit/tmp");
    std::fs::create_dir_all(&scratch_root).unwrap();
    let root = tempfile::tempdir_in(&scratch_root).unwrap();
    let repo = root.path().join("repo");
    let global = root.path().join("home/.orbit");
    std::fs::create_dir_all(repo.join(".orbit/tmp")).unwrap();
    std::fs::create_dir_all(&global).unwrap();
    let runtime = crate::OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("node"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let entry = repo.join("scripts/ux-rules/step.mjs");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::copy(source_root.join("scripts/ux-rules/step.mjs"), &entry).unwrap();
    // A real decodable PNG, not text with an image extension.
    let png: Vec<u8> = vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 48, 152, 240, 224,
        63, 0, 5, 52, 2, 160, 226, 230, 199, 152, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    std::fs::write(repo.join("runner.mjs"), format!(r#"import {{writeFileSync}} from 'node:fs'; import path from 'node:path';
const out=process.argv[process.argv.indexOf('--out')+1];
writeFileSync(path.join(out,'capture.png'),Buffer.from({}));
writeFileSync(path.join(out,'ux-rules.json'),JSON.stringify({{schema_version:1,surface:'ui',results:[{{rule:'ui.example',target:{{id:'fixture'}},status:'fail',severity:'high',fingerprint:'retained-png',measurements:{{gap:1}},evidence:['capture.png']}}]}}));"#, serde_json::to_string(&png).unwrap())).unwrap();
    let scripts = repo.join(".orbit/resources/scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(scripts.join("ux_rules_runner.yaml"), format!("schemaVersion: 1\nkind: Script\nmetadata:\n  name: ux_rules_runner\nspec:\n  description: Isolated evidence fixture.\n  entry: scripts/ux-rules/step.mjs\n  interpreter: [{}]\n  files:\n    outputs: {{max_files: 64, max_file_bytes: 1048576}}\n", node.display())).unwrap();
    let grant = runtime
        .grant_script(
            "ux_rules_runner",
            &crate::runtime::scripts::ScriptGrantOptions {
                output_max_files: Some(64),
                output_max_file_bytes: Some(1048576),
                ..Default::default()
            },
        )
        .unwrap();
    let yaml = include_str!("../../../../assets/jobs/ux_rules_pipeline.yaml");
    let mut job = orbit_engine::activity_job::load_job_asset(yaml)
        .unwrap()
        .spec;
    // Resolve only the catalog activity; the real script target stays a script.
    let activity = orbit_engine::activity_job::load_activity_asset(include_str!(
        "../../../../assets/activities/file_rule_findings.yaml"
    ))
    .unwrap();
    let orbit_types::workflow::JobV2StepBody::TargetRef(reference) = job.steps[1].body.clone()
    else {
        panic!("filer ref")
    };
    job.steps[1].body =
        orbit_types::workflow::JobV2StepBody::Target(orbit_types::workflow::TargetStep {
            spec: activity.spec.spec,
            activity_name: Some("file_rule_findings".to_string()),
            input_schema_json: Some(activity.spec.input_schema_json),
            fs_profile: None,
            default_input: reference.default_input,
            timeout_seconds: reference.timeout_seconds,
            session: None,
        });
    let input = json!({"surfaces":["ui"],"runner":"runner.mjs","orbit":node});
    let host = EvidencePipeline {
        runtime: &runtime,
        repo: &repo,
        grant: &grant,
    };
    let run = |resume: Option<&PipelineState>, ancestor: Option<String>| {
        let row = runtime
            .stores()
            .jobs()
            .insert_job_run(
                "ux_rules_pipeline",
                1,
                chrono::Utc::now(),
                Some(input.clone()),
                ancestor,
            )
            .unwrap();
        let mut state = resume.cloned().unwrap_or_else(|| {
            PipelineState::new(
                row.run_id.clone(),
                "ux_rules_pipeline".to_string(),
                input.clone(),
            )
        });
        state.run_id = row.run_id.clone();
        runtime.write_run_state(&row.run_id, &state).unwrap();
        runtime
            .write_run_definition_snapshot(&row.run_id, yaml)
            .unwrap();
        let audit = V2AuditWriter::with_disk_sinks(
            &repo.join("audit"),
            Arc::new(orbit_store::Store::open_in_memory().unwrap()),
            runtime.workspace_id().unwrap(),
            &row.run_id,
            "fixture",
            Some(&repo),
        )
        .unwrap();
        let outcome =
            execute_job_with_resume(&job, input.clone(), &row.run_id, audit, &host, resume)
                .unwrap();
        assert!(outcome.success, "{outcome:?}");
        runtime.read_run_state(&row.run_id).unwrap().unwrap()
    };
    let first = run(None, None);
    let filed = &first.pipeline["file"];
    assert_eq!(filed["filed_count"], 1, "{filed}");
    let task_id = filed["filed"][0]["task_id"].as_str().unwrap();
    let artifacts = runtime.get_task_artifacts(task_id).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].content, png);
    let set = &first.script_output_sets[&0]["run"];
    if let Some(report_dir) = std::env::var_os("ORBIT_UX_EVIDENCE_REPORT_DIR") {
        let report_dir = std::path::PathBuf::from(report_dir);
        std::fs::create_dir_all(&report_dir).unwrap();
        assert!(
            report_dir
                .canonicalize()
                .unwrap()
                .starts_with(scratch_root.canonicalize().unwrap())
        );
        std::fs::write(report_dir.join("attached.png"), &artifacts[0].content).unwrap();
        std::fs::write(report_dir.join("receipt.json"), serde_json::to_vec_pretty(&json!({
            "schema_version":1,"launcher":if std::env::var_os("ORBIT_UX_EVIDENCE_CONTAINED").is_some(){"contained"}else{"fixture"},
            "run_id":first.run_id,"task_id":task_id,"file":set.files[0],"task_artifact":artifacts[0].path,
            "attachment_sha256":orbit_types::workflow::activity_job::script_sha256_hex(&artifacts[0].content),
            "scratch_removed":true
        })).unwrap()).unwrap();
    }

    assert_eq!(
        orbit_types::workflow::activity_job::script_sha256_hex(&artifacts[0].content),
        set.files[0].sha256
    );
    assert_eq!(
        std::fs::read_dir(repo.join(".orbit/tmp")).unwrap().count(),
        0,
        "script scratch gone before reading task evidence"
    );
    let second = run(None, None);
    assert_eq!(
        second.pipeline["file"]["filed_count"], 0,
        "identical replay deduplicates"
    );
    let mut checkpoint = first.clone();
    checkpoint.step_states.remove(&1);
    checkpoint.step_outputs.remove(&1);
    checkpoint.pipeline.as_object_mut().unwrap().remove("file");
    checkpoint.next_step_index = 1;
    let resumed = run(Some(&checkpoint), Some(first.run_id.clone()));
    assert_eq!(resumed.pipeline["file"]["filed_count"], 0);
    assert_eq!(resumed.script_output_sets[&0], first.script_output_sets[&0]);
    // Genuine files from an unrelated run cannot be substituted, even if
    // the document bytes and evidence names are identical.
    let mut wrong = checkpoint.clone();
    wrong.script_output_sets = second.script_output_sets.clone();
    let bad = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "ux_rules_pipeline",
            1,
            chrono::Utc::now(),
            Some(input),
            Some(first.run_id.clone()),
        )
        .unwrap();
    wrong.run_id = bad.run_id.clone();
    runtime.write_run_state(&bad.run_id, &wrong).unwrap();
    runtime
        .write_run_definition_snapshot(&bad.run_id, yaml)
        .unwrap();
    let context = orbit_tools::ToolContext {
        reservation_owner: Some(orbit_tools::ReservationOwnerContext {
            owner_run_id: bad.run_id.clone(),
            owner_metadata_json: None,
        }),
        ..Default::default()
    };
    let call = |document: Value, files: Value| {
        RuntimeHost::run_deterministic(
            &runtime,
            "file_rule_findings",
            &json!({}),
            &json!({"results":document,"evidence_step":"run","evidence_files":files}),
            context.clone(),
        )
    };
    let original = first.step_output(0).unwrap().clone();
    let refused = call(
        original.clone(),
        serde_json::to_value(&second.script_output_sets[&0]["run"].files).unwrap(),
    )
    .unwrap_err();
    assert!(refused.to_string().contains("not run"), "{refused}");
    wrong.script_output_sets = first.script_output_sets.clone();
    runtime.write_run_state(&bad.run_id, &wrong).unwrap();
    let mut forged = set.files.clone();
    forged[0].path = repo.join("capture.png").display().to_string();
    assert!(
        call(original.clone(), serde_json::to_value(forged).unwrap())
            .unwrap_err()
            .to_string()
            .contains("do not match")
    );
    let mut unpersisted = wrong.clone();
    unpersisted.script_output_sets.remove(&0);
    unpersisted
        .script_output_sets
        .insert(99, first.script_output_sets[&0].clone());
    runtime.write_run_state(&bad.run_id, &unpersisted).unwrap();
    assert!(
        call(original.clone(), serde_json::to_value(&set.files).unwrap())
            .unwrap_err()
            .to_string()
            .contains("not persisted"),
        "historical maps cannot replace the bound producer checkpoint"
    );
    let mut over_cap = wrong.clone();
    over_cap
        .script_output_sets
        .get_mut(&0)
        .unwrap()
        .get_mut("run")
        .unwrap()
        .files[0]
        .bytes = 1048577;
    runtime.write_run_state(&bad.run_id, &over_cap).unwrap();
    assert!(
        call(original.clone(), serde_json::to_value(&set.files).unwrap())
            .unwrap_err()
            .to_string()
            .contains("budget exceeds")
    );
    runtime.write_run_state(&bad.run_id, &wrong).unwrap();
    for evidence in [json!(["../capture.png"]), json!([42]), json!("capture.png")] {
        let mut malformed = original.clone();
        malformed["results"][0]["evidence"] = evidence;
        let mut state = wrong.clone();
        state.record_step(0, JobRunState::Success, Some(malformed.clone()), None);
        state.next_step_index = 1;
        runtime.write_run_state(&bad.run_id, &state).unwrap();
        assert!(call(malformed, serde_json::to_value(&set.files).unwrap()).is_err());
    }
    let mut missing = original.clone();
    missing["results"][0]["evidence"] = json!(["missing.png"]);
    // Matching document checkpoint cannot grant access to an undeclared name.
    wrong.record_step(0, JobRunState::Success, Some(missing.clone()), None);
    wrong.next_step_index = 1;
    runtime.write_run_state(&bad.run_id, &wrong).unwrap();
    assert!(
        call(missing, serde_json::to_value(&set.files).unwrap())
            .unwrap_err()
            .to_string()
            .contains("not in the producer")
    );
    std::fs::write(&set.files[0].path, b"changed").unwrap();
    wrong.record_step(0, JobRunState::Success, Some(original.clone()), None);
    wrong.next_step_index = 1;
    runtime.write_run_state(&bad.run_id, &wrong).unwrap();
    assert!(
        call(original, serde_json::to_value(&set.files).unwrap())
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    assert_eq!(
        runtime.get_task_artifacts(task_id).unwrap()[0].content,
        png,
        "attached evidence remains independent of producer storage"
    );
}
