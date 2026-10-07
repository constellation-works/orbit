//! Required-check records keep a stable id across an attempt's report
//! revisions [ORB-14370].
//!
//! Command-text matching falsely blocked correct candidates: a reviewer that
//! renames, rewraps or concretises a command between revisions dropped the
//! obligation as far as the gate could tell. Reports reach the gate through
//! the public `orbit.task.artifact.put` tool, which refuses a revision that
//! drops an earlier record id while the reviewer can still correct it, and
//! the deterministic settlement then compares by id.

use chrono::{Duration, Utc};
use orbit_automation::review::certificate_acceptable;
use orbit_common::security::release::sha256_hex;
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::{
    ArtifactManifestFileV2, ArtifactManifestV2, TASK_ARTIFACT_MANIFEST_FILE_NAME,
    TASK_ARTIFACTS_DIR_NAME,
};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT, RetiredValidation, ReviewCertificate, ReviewReport,
    ReviewReportHistory, ReviewReportRevision, ReviewVerdict, ValidationOutcome,
};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

fn report(fixture: &Fixture, verdict: &str, validation: Value, retired: Value) -> Value {
    json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": verdict,
        "summary": "Checked the candidate.",
        "findings": [],
        "validation": validation,
        "retired_validation": retired,
        "escalation": (verdict == "incomplete").then_some("validation still running"),
    })
}

fn certificate(fixture: &Fixture) -> ReviewCertificate {
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_GATE_ARTIFACT)
        .unwrap()
        .expect("settlement issues a certificate");
    serde_json::from_slice(&artifact.content).unwrap()
}

fn history_len(fixture: &Fixture) -> usize {
    let history = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_REPORT_HISTORY_ARTIFACT)
        .unwrap()
        .expect("report history");
    ReviewReportHistory::parse(&history.content)
        .unwrap()
        .revisions
        .len()
}

/// Seed the artifacts a pre-id build could already have stored, bypassing
/// today's submit-time validation while exercising normal runtime settlement.
fn seed_legacy_reports(fixture: &Fixture, earlier: &Value, current: &Value) {
    let earlier_bytes = earlier.to_string().into_bytes();
    let current_bytes = current.to_string().into_bytes();
    let earlier_report = ReviewReport::parse(&earlier_bytes).unwrap();
    let current_report = ReviewReport::parse(&current_bytes).unwrap();
    let mut history = ReviewReportHistory::default();
    history
        .record(ReviewReportRevision {
            attempt_id: earlier_report.attempt_id,
            sha256: sha256_hex(&earlier_bytes),
            observed_at: Utc::now() - Duration::seconds(1),
            recorded_by: "legacy-reviewer".into(),
            verdict: earlier_report.verdict,
            validation: earlier_report.validation,
            record_id_contract_checked: None,
        })
        .unwrap();
    store_report_artifacts(
        fixture,
        current_bytes,
        serde_json::to_vec_pretty(&history).unwrap(),
    );

    assert_eq!(current_report.validation[0].id, None);
}

/// Seed a report delivered only after the claimed reviewer stopped, which
/// the store retains without an in-session refusal [ORB-14370].
fn seed_post_session_report(fixture: &Fixture, current: &Value) {
    let current_bytes = current.to_string().into_bytes();
    let current_report = ReviewReport::parse(&current_bytes).unwrap();
    let mut history = ReviewReportHistory::default();
    history
        .record(ReviewReportRevision {
            attempt_id: current_report.attempt_id,
            sha256: sha256_hex(&current_bytes),
            observed_at: Utc::now(),
            recorded_by: "claimed-reviewer".into(),
            verdict: current_report.verdict,
            validation: current_report.validation,
            record_id_contract_checked: Some(false),
        })
        .unwrap();
    store_report_artifacts(
        fixture,
        current_bytes,
        serde_json::to_vec_pretty(&history).unwrap(),
    );
}

fn store_report_artifacts(fixture: &Fixture, report_bytes: Vec<u8>, history_bytes: Vec<u8>) {
    let registry =
        TaskRegistryStore::open(&task_registry_path(&fixture.runtime.global_root())).unwrap();
    let bundle = registry
        .canonical_task_bundle_path(&fixture.runtime.workspace_id().unwrap(), &fixture.task_id)
        .unwrap();
    let artifacts = bundle.join(TASK_ARTIFACTS_DIR_NAME);
    let manifest_path = artifacts.join(TASK_ARTIFACT_MANIFEST_FILE_NAME);
    let mut manifest: ArtifactManifestV2 =
        serde_yaml::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();

    for (path, bytes) in [
        (REVIEW_REPORT_ARTIFACT, report_bytes),
        (REVIEW_REPORT_HISTORY_ARTIFACT, history_bytes),
    ] {
        let digest = sha256_hex(&bytes);
        let blob = format!("files/.blob-{}-{digest}", sha256_hex(path.as_bytes()));
        std::fs::write(artifacts.join(&blob), &bytes).unwrap();
        manifest.files.retain(|file| file.path != path);
        manifest.files.push(ArtifactManifestFileV2 {
            origin: None,
            path: path.into(),
            blob,
            sha256: digest,
            media_type: "application/json".into(),
            size_bytes: bytes.len() as u64,
            created_by: "reviewer-fixture".into(),
            created_at: Utc::now(),
        });
    }
    std::fs::write(manifest_path, serde_yaml::to_string(&manifest).unwrap()).unwrap();
}

/// ORB-14360 recorded a prose-named check `not_run` and later the real
/// command `passed`; ORB-14260 recorded a `<workspace>` placeholder and
/// later the absolute path. Carrying the record id forward settles both.
#[test]
fn a_carried_record_id_settles_when_the_command_text_changes() {
    if !super::dispatch_admission::isolated(
        "review_record_ids::a_carried_record_id_settles_when_the_command_text_changes",
    ) {
        return;
    }
    // `None`: the absolute path of the fixture's own checkout.
    for (incident, earlier, current) in [
        (
            "ORB-14360",
            "focused CLI reference invocation verification",
            Some("python3 .orbit/tmp/verify-reference-examples.py"),
        ),
        (
            "ORB-14260",
            "cargo test --manifest-path <workspace>/Cargo.toml -p orbit-core",
            None,
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.admit();
        let current = current.map_or_else(
            || {
                format!(
                    "cargo test --manifest-path {}/Cargo.toml -p orbit-core",
                    fixture.repo.display()
                )
            },
            str::to_string,
        );
        fixture.put_report(&report(
            &fixture,
            "incomplete",
            json!([{"id": "V1", "command": earlier, "outcome": "not_run", "role": "required"}]),
            json!([]),
        ));
        fixture.put_report(&report(
            &fixture,
            "accept",
            json!([{"id": "V1", "command": current, "outcome": "passed", "role": "required"}]),
            json!([]),
        ));

        let settled = fixture
            .settle()
            .unwrap_or_else(|error| panic!("{incident}: the carried id settles: {error}"));
        assert_eq!(settled["gate"], "passed", "{incident}");
        let certificate = certificate(&fixture);
        assert!(certificate.validation_complete, "{incident}");
        assert_eq!(
            certificate
                .retained_obligations
                .iter()
                .map(|obligation| obligation.validation.command.as_str())
                .collect::<Vec<_>>(),
            vec![earlier],
            "{incident}: the earlier record stays visible on the certificate"
        );
        assert_eq!(certificate_acceptable(&certificate), Ok(()), "{incident}");
    }
}

/// A revision that drops an earlier record id is refused at attach with
/// the record named, and so is retiring a record that failed; the reviewer
/// then corrects the report in the same attempt and it settles.
#[test]
fn a_revision_dropping_a_record_id_is_refused_at_attach_and_resubmitted() {
    if !super::dispatch_admission::isolated(
        "review_record_ids::a_revision_dropping_a_record_id_is_refused_at_attach_and_resubmitted",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    let missing_id = fixture
        .try_put_report(&report(
            &fixture,
            "incomplete",
            json!([{"command": "make ci-fast", "outcome": "not_run", "role": "required"}]),
            json!([]),
        ))
        .expect_err("a newly submitted required record needs an id");
    assert!(
        missing_id
            .to_string()
            .contains("these have none: `make ci-fast` -> `\"id\": \"V1\"`"),
        "the first-submission refusal names the record and an id to give it: {missing_id}"
    );
    let duplicate_id = fixture
        .try_put_report(&report(
            &fixture,
            "incomplete",
            json!([
                {"id": "V1", "command": "make ci-fast", "outcome": "not_run", "role": "required"},
                {"id": "V1", "command": "cargo test -p orbit-core", "outcome": "not_run", "role": "required"}
            ]),
            json!([]),
        ))
        .expect_err("distinct required records cannot share an id");
    assert!(
        duplicate_id
            .to_string()
            .contains("validation record id `V1` is used by multiple records"),
        "the duplicate-id refusal identifies the ambiguous id: {duplicate_id}"
    );
    let first = report(
        &fixture,
        "incomplete",
        json!([
            {"id": "V1", "command": "make ci-fast", "outcome": "failed", "role": "required"},
            {"id": "V2", "command": "make docs-check", "outcome": "not_run", "role": "required"},
        ]),
        json!([]),
    );
    fixture.put_report(&first);

    let dropped = fixture
        .try_put_report(&report(
            &fixture,
            "accept",
            json!([{"id": "V2", "command": "make docs-check", "outcome": "passed"}]),
            json!([]),
        ))
        .expect_err("a revision that omits V1 is refused");
    let message = dropped.to_string();
    assert!(
        message.contains("required validation record `V1` (`make ci-fast`)")
            && message.contains("this report omits it"),
        "the refusal names the dropped record: {message}"
    );

    let retired_failure = fixture
        .try_put_report(&report(
            &fixture,
            "accept",
            json!([{"id": "V2", "command": "make docs-check", "outcome": "passed"}]),
            json!([{"id": "V1", "reason": "not needed"}]),
        ))
        .expect_err("a failed record cannot be retired");
    assert!(
        retired_failure
            .to_string()
            .contains("record `V1` (`make ci-fast`) was recorded failed")
            && retired_failure
                .to_string()
                .contains("retires it although it failed"),
        "{retired_failure}"
    );

    // Neither refusal replaced the report or entered the history.
    let current = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, REVIEW_REPORT_ARTIFACT)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&current.content).unwrap(),
        first
    );
    assert_eq!(history_len(&fixture), 1);

    // The corrected revision carries V1 as a superseded failed attempt and
    // its required passing rerun, both under V1, then retires V2, which never
    // ran.
    fixture.put_report(&report(
        &fixture,
        "accept",
        json!([
            {
                "id": "V1",
                "command": "make ci-fast",
                "outcome": "failed",
                "role": "superseded",
                "note": "rerun after the failed first attempt",
            },
            {
                "id": "V1",
                "command": "set -o pipefail; make ci-fast 2>&1 | tee .orbit/tmp/ci-fast.log",
                "outcome": "passed",
                "role": "required",
            }
        ]),
        json!([{"id": "V2", "reason": "the docs target was removed from this repository"}]),
    ));
    assert_eq!(history_len(&fixture), 2);

    let settled = fixture.settle().expect("the corrected revision settles");
    assert_eq!(settled["gate"], "passed");
    let certificate = certificate(&fixture);
    assert!(certificate.validation_complete);
    assert_eq!(
        certificate.retired_validation,
        vec![RetiredValidation {
            id: "V2".into(),
            reason: "the docs target was removed from this repository".into(),
        }]
    );
    assert_eq!(certificate_acceptable(&certificate), Ok(()));
    let comment = fixture
        .runtime
        .get_task_comments(&fixture.task_id)
        .unwrap()
        .into_iter()
        .map(|comment| comment.message)
        .rfind(|message| message.starts_with("before-PR review settled attempt"))
        .expect("settlement comments its verdict");
    assert!(
        comment.contains(
            "V2 `make docs-check` not_run (retired: the docs target was removed from this \
             repository)"
        ),
        "the retirement is disclosed: {comment}"
    );
}

/// Reports written without record ids attach and settle as before: a
/// command-identity match resolves the earlier record, and a renamed command
/// is still a dropped obligation.
#[test]
fn reports_without_record_ids_keep_the_command_identity_rules() {
    if !super::dispatch_admission::isolated(
        "review_record_ids::reports_without_record_ids_keep_the_command_identity_rules",
    ) {
        return;
    }
    for (earlier, current, passes) in [
        (
            "make ci-fast",
            "TMPDIR=\"$PWD/.orbit/tmp\" make  ci-fast",
            true,
        ),
        (
            "focused CLI reference invocation verification",
            "python3 .orbit/tmp/verify-reference-examples.py",
            false,
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.admit();
        let earlier_report = report(
            &fixture,
            "incomplete",
            json!([{"command": earlier, "outcome": "not_run"}]),
            json!([]),
        );
        let current_report = report(
            &fixture,
            "accept",
            json!([{"command": current, "outcome": "passed"}]),
            json!([]),
        );
        seed_legacy_reports(&fixture, &earlier_report, &current_report);
        let settled = fixture.settle();
        let certificate = certificate(&fixture);
        assert_eq!(settled.is_ok(), passes, "{earlier} -> {current}");
        assert_eq!(certificate.validation_complete, passes);
        assert_eq!(
            certificate.retained_obligations[0].validation.outcome,
            ValidationOutcome::NotRun
        );
        if !passes {
            assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
            let escalation = certificate.escalation.unwrap_or_default();
            assert!(
                escalation.contains(&format!(
                    "required check `{earlier}` was recorded not_run by an earlier report \
                     revision of this attempt and the final report omits it"
                )),
                "{escalation}"
            );
        }
    }
}

/// A claimed worker's Evidence/Fail commit happens after the reviewer stops,
/// so it must retain the report instead of refusing the write. Settlement
/// still applies the id contract to a newly received report; pre-change
/// reports without the contract marker keep their legacy behavior above.
#[test]
fn new_post_session_claim_reports_cannot_settle_without_unique_record_ids() {
    if !super::dispatch_admission::isolated(
        "review_record_ids::new_post_session_claim_reports_cannot_settle_without_unique_record_ids",
    ) {
        return;
    }
    let cases = [
        (
            "missing id",
            json!([{"command":"make ci-fast","outcome":"passed","role":"required"}]),
            false,
            "needs a stable, non-empty `id`",
        ),
        (
            "duplicate id",
            json!([
                {"id":"V1","command":"make ci-fast","outcome":"passed","role":"required"},
                {"id":"V1","command":"cargo test -p orbit-core","outcome":"passed","role":"required"}
            ]),
            false,
            "validation record id `V1` is used by multiple records",
        ),
        (
            "valid ids",
            json!([{"id":"V1","command":"make ci-fast","outcome":"passed","role":"required"}]),
            true,
            "",
        ),
    ];
    for (name, validation, passes, expected_message) in cases {
        let mut fixture = Fixture::new();
        fixture.admit();
        seed_post_session_report(&fixture, &report(&fixture, "accept", validation, json!([])));

        let settled = fixture.settle();
        assert_eq!(settled.is_ok(), passes, "{name}: {settled:?}");
        let certificate = certificate(&fixture);
        assert_eq!(certificate.validation_complete, passes, "{name}");
        if passes {
            assert_eq!(certificate.verdict, ReviewVerdict::Accept, "{name}");
        } else {
            assert_eq!(certificate.verdict, ReviewVerdict::Incomplete, "{name}");
            let escalation = certificate.escalation.unwrap_or_default();
            assert!(
                escalation.contains("report_record_ids_invalid")
                    && escalation.contains(expected_message),
                "{name}: settlement identifies the bad post-session report: {escalation}"
            );
        }
    }
}

/// Deploy and land are not atomic: a reviewer prompted with the instructions
/// before record ids filed an id-less revision under the old build, then
/// puts another under this one. The put is refused naming every record and
/// a free id for each; the reviewer resubmits in the same attempt, and the
/// earlier id-less record is still matched by command identity.
#[test]
fn an_in_flight_review_without_ids_resubmits_with_the_named_ids() {
    if !super::dispatch_admission::isolated(
        "review_record_ids::an_in_flight_review_without_ids_resubmits_with_the_named_ids",
    ) {
        return;
    }
    let mut fixture = Fixture::new();
    fixture.admit();
    let earlier = report(
        &fixture,
        "incomplete",
        json!([
            {"command": "make ci-fast", "outcome": "not_run", "role": "required"},
            {"command": "make ci-lint", "outcome": "not_run", "role": "required"},
        ]),
        json!([]),
    );
    seed_legacy_reports(&fixture, &earlier, &earlier);

    let mut current = report(
        &fixture,
        "accept",
        json!([
            {"command": "make ci-fast", "outcome": "passed", "role": "required"},
            {"command": "make ci-lint", "outcome": "passed", "role": "required"},
        ]),
        json!([]),
    );
    let refused = fixture
        .try_put_report(&current)
        .expect_err("a new id-less revision is refused")
        .to_string();
    assert!(
        refused.contains("`make ci-fast` -> `\"id\": \"V1\"`, `make ci-lint` -> `\"id\": \"V2\"`"),
        "the refusal names every record and a distinct id for each: {refused}"
    );
    assert_eq!(history_len(&fixture), 1, "a refused put adds no revision");

    current["validation"][0]["id"] = json!("V1");
    current["validation"][1]["id"] = json!("V2");
    fixture.put_report(&current);
    assert_eq!(history_len(&fixture), 2);
    let settled = fixture
        .settle()
        .expect("the resubmitted report settles against the id-less revision");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(certificate_acceptable(&certificate(&fixture)), Ok(()));
}
