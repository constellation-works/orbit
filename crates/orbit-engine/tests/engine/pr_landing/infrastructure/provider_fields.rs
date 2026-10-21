//! Provider field refusals through the public VCS operation boundary.
use super::*;

#[test]
fn provider_fields_keep_prerequisite_refusals_across_read_operations() {
    isolated(
        "infrastructure::provider_fields::provider_fields_keep_prerequisite_refusals_across_read_operations",
        |sandbox| {
            in_fresh_row(sandbox, |row| {
                let fx = fixture_with_workflow(row);
                setup(&fx, "positive");
                let original: Value =
                    serde_json::from_slice(&fs::read(fx.forge.join("infra-repo")).unwrap())
                        .unwrap();
                for (key, value) in [
                    ("id", Some(json!(0))),
                    ("id", Some(json!(-1))),
                    ("id", Some(json!(1.5))),
                    ("id", Some(json!("500"))),
                    ("id", Some(Value::Null)),
                    ("id", None),
                    ("full_name", Some(json!(""))),
                    ("full_name", Some(json!(500))),
                    ("full_name", Some(Value::Null)),
                    ("full_name", None),
                    // Both fields parse, but the PR still names the original repo.
                    ("id", Some(json!(501))),
                ] {
                    let mut repo = original.clone();
                    if let Some(value) = &value {
                        repo[key] = value.clone();
                    } else {
                        repo.as_object_mut().unwrap().remove(key);
                    }
                    put(&fx, "repo", repo);
                    for operation in [
                        "ci.prerequisite_evidence",
                        "ci.emission_evidence",
                        "ci.current_gates",
                    ] {
                        let error = orbit_engine::run_private_vcs_operation(
                            operation,
                            &json!({
                                "workspace_path": fx.repo, "pr": PR_NUMBER,
                                "head_sha": fx.candidate, "head_branch": BRANCH,
                                "base_branch": BASE,
                                "deadline_ms": std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH).unwrap()
                                    .as_millis() as u64 + 60_000,
                            }),
                        )
                        .expect_err("malformed or changed provider fields must refuse");
                        let orbit_common::OrbitError::Execution(message) = error else {
                            panic!("unexpected refusal for {operation}: {error}");
                        };
                        let reason = if value == Some(json!(501)) {
                            "foreign or changed repository"
                        } else {
                            key
                        };
                        assert_eq!(
                            message,
                            format!("workflow prerequisite evidence is unavailable ({reason})"),
                            "{operation}: {key}={value:?}",
                        );
                    }
                }
                assert!(fx.merge_requests().is_empty());
            });
        },
    );
}
