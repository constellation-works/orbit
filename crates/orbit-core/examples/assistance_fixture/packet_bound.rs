//! Packet-bound choice admission on the public assistance service.
//! Synthetic labels only. This is part of the manual fixture, not a unit test.
use std::collections::BTreeMap;

use orbit_store::contracts::assistance::AssistanceAdmission;
use orbit_types::assistance::{
    self, AssistanceContract, AssistanceDisposition, AssistanceEvidencePacket,
    AssistancePacketBoundChoice, AssistanceQuestion, AssistanceReason, AssistanceRequest,
};
use serde_json::{Value, json};

use crate::fixture::{AssistanceFixture, FixtureResult, SUPPLIED};

const ABSTAIN: &str = "insufficient_precedent";

pub(super) fn fixture_packet_bound_choice(root: &std::path::Path, schema: &Value) -> FixtureResult {
    let f = AssistanceFixture::open_fixture(root, schema, "task_quality")?;
    legacy_expanded_template_is_unchanged(&f)?;
    fixed_choice_ignores_packet_ids(&f)?;
    packet_bound_choice_uses_copied_ids(&f)?;
    Ok(())
}

fn legacy_expanded_template_is_unchanged(f: &AssistanceFixture) -> FixtureResult {
    let template = f.contract.clone();
    let encoded = serde_json::to_value(&template)?;
    assert_absent(&encoded, "packet_bound")?;
    let digest = assistance::assistance_digest(&serde_json::to_vec(&template)?);
    assert_eq!(
        digest,
        f.settings.lock().map_err(|_| "settings lock")?.policies[0].contract_digest,
        "existing expanded template digest must stay the serialized contract"
    );
    let id = enqueue(f, f.request("legacy-same")?, &template)?;
    assert_eq!(
        f.record(&id)?.work.request.contract,
        template,
        "expanding an existing template must not rewrite its questions"
    );
    let mut drifted = f.request("legacy-drifted")?;
    let Some(question) = drifted.contract.questions.values_mut().next() else {
        return Err("schema template has no question".into());
    };
    match question {
        AssistanceQuestion::Predicate { rubric, .. }
        | AssistanceQuestion::Choice { rubric, .. } => rubric.push(' '),
    }
    assert!(
        f.service
            .enqueue_expanded_assistance(drifted, &[SUPPLIED.into()], &template)
            .is_err(),
        "an expanded question that changes the template is still refused"
    );
    Ok(())
}

fn fixed_choice_ignores_packet_ids(f: &AssistanceFixture) -> FixtureResult {
    let template = fixed_choice_template();
    let question = serde_json::to_value(&template.questions["grade"])?;
    assert_eq!(
        question,
        json!({
            "type": "choice",
            "rubric": "Pick the recorded grade.",
            "allowed": ["adequate", "deficient"],
            "abstain_values": ["deficient"],
            "min_confidence": null,
            "criterion_refs": []
        }),
        "a choice without a packet binding must serialize exactly as before"
    );
    grant(f, "legacy_choice", &template, &["option_ids"])?;
    let mut expanded = template.clone();
    expanded.questions.clear();
    expanded
        .questions
        .insert("grade/1".into(), template.questions["grade"].clone());
    let id = enqueue(
        f,
        request(
            f,
            "legacy-grade",
            "legacy_choice",
            &expanded,
            json!(["not-a-grade"]),
        )?,
        &template,
    )?;
    assert_eq!(
        choice_allowed(&f.record(&id)?.work.request.contract, "grade/1")?,
        vec!["adequate".to_owned(), "deficient".to_owned()],
        "a fixed choice keeps its allowed values when the packet names other ids"
    );
    expanded
        .questions
        .get_mut("grade/1")
        .ok_or("missing grade/1")?
        .clone_from(&AssistanceQuestion::Choice {
            rubric: "Pick the recorded grade.".into(),
            allowed: vec!["other".into(), "deficient".into()],
            abstain_values: vec!["deficient".into()],
            min_confidence: None,
            criterion_refs: vec![],
            packet_bound: None,
        });
    assert!(
        f.service
            .enqueue_expanded_assistance(
                request(
                    f,
                    "legacy-other",
                    "legacy_choice",
                    &expanded,
                    json!(["adequate"])
                )?,
                &[SUPPLIED.into()],
                &template,
            )
            .is_err(),
        "a fixed expanded choice still refuses a different allowed set"
    );
    Ok(())
}

fn packet_bound_choice_uses_copied_ids(f: &AssistanceFixture) -> FixtureResult {
    let template = packet_template(8, 64);
    assert_template_binds_field_not_ids(&template)?;
    grant(f, "packet_choice", &template, &["option_ids"])?;
    let allowed = copied_allowed();
    accept_copied_choice(f, &template, &allowed)?;
    refuse_inconsistent_answers(f, &template, &allowed)?;
    accept_abstention(f, &template, &allowed)?;
    refuse_id_bounds(f, &template)?;
    refuse_packet_shapes(f, &template)?;
    refuse_ungranted_field_and_ceiling(f, &template)?;
    Ok(())
}

fn copied_allowed() -> Vec<String> {
    vec![
        "approve".to_owned(),
        "reject".to_owned(),
        ABSTAIN.to_owned(),
    ]
}

fn assert_template_binds_field_not_ids(template: &AssistanceContract) -> FixtureResult {
    let template_json = String::from_utf8(serde_json::to_vec(template)?)?;
    assert!(
        !template_json.contains("approve"),
        "the template digest input must not contain per-request ids"
    );
    assert!(
        template_json.contains("option_ids") && template_json.contains(ABSTAIN),
        "the template digest input must bind the field and the abstention"
    );
    Ok(())
}

fn accept_copied_choice(
    f: &AssistanceFixture,
    template: &AssistanceContract,
    allowed: &[String],
) -> FixtureResult {
    let id = enqueue(
        f,
        request(
            f,
            "packet-approve",
            "packet_choice",
            template,
            json!(["approve", "reject"]),
        )?,
        template,
    )?;
    let stored = f.record(&id)?;
    assert_eq!(
        choice_allowed(&stored.work.request.contract, "decision_choice")?,
        allowed,
        "admission copies packet ids and appends the abstention"
    );
    assert_absent(
        &serde_json::to_value(&stored.work.request.contract)?,
        "packet_bound",
    )?;
    assert_ne!(
        assistance::assistance_digest(&serde_json::to_vec(&stored.work.request.contract)?),
        assistance::assistance_digest(&serde_json::to_vec(template)?),
        "the stored contract carries the copied ids, which the template digest excludes"
    );
    f.respond(choice_reply(allowed, "approve", (0.01, 0.98, 0.98)))?;
    assert!(
        f.service
            .process_next_assistance("ws_fixture", "packet_choice")?
            .is_some(),
        "the copied choice must be claimed"
    );
    let body = f
        .transport
        .bodies
        .lock()
        .map_err(|_| "body lock")?
        .last()
        .cloned()
        .ok_or("missing decisions body")?;
    let sent: Vec<String> = body["questions"][0]["choices"]
        .as_array()
        .ok_or("choices missing")?
        .iter()
        .filter_map(|choice| choice["value"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(sent, allowed, "the model is offered the copied ids");
    assert_eq!(
        (
            f.record(&id)?.result.as_ref().map(|result| result.reason),
            f.record(&id)?
                .result
                .as_ref()
                .map(|result| result.disposition)
        ),
        (
            Some(AssistanceReason::Complete),
            Some(AssistanceDisposition::Suggested)
        ),
        "a copied id with a consistent distribution is accepted"
    );
    Ok(())
}

fn refuse_inconsistent_answers(
    f: &AssistanceFixture,
    template: &AssistanceContract,
    allowed: &[String],
) -> FixtureResult {
    refuse_answer(
        f,
        template,
        "packet-outside",
        allowed,
        "defer",
        (0.01, 0.98, 0.98),
    )?;
    let mut missing_abstention = allowed.to_vec();
    missing_abstention.pop();
    refuse_answer(
        f,
        template,
        "packet-partial-distribution",
        &missing_abstention,
        "approve",
        (0.01, 0.99, 0.99),
    )?;
    refuse_answer(
        f,
        template,
        "packet-sum",
        allowed,
        "approve",
        (0.5, 0.5, 0.5),
    )?;
    // 0.40 + 0.20 + 0.40 sums to one, and reject is not the most probable value.
    refuse_answer(
        f,
        template,
        "packet-not-modal",
        allowed,
        "reject",
        (0.40, 0.20, 0.20),
    )?;
    Ok(())
}

fn accept_abstention(
    f: &AssistanceFixture,
    template: &AssistanceContract,
    allowed: &[String],
) -> FixtureResult {
    let abstained = enqueue(
        f,
        request(
            f,
            "packet-abstain",
            "packet_choice",
            template,
            json!(["approve", "reject"]),
        )?,
        template,
    )?;
    f.respond(choice_reply(allowed, ABSTAIN, (0.01, 0.98, 0.98)))?;
    assert!(
        f.service
            .process_next_assistance("ws_fixture", "packet_choice")?
            .is_some()
    );
    assert_eq!(
        (
            f.record(&abstained)?
                .result
                .as_ref()
                .map(|result| result.reason),
            f.record(&abstained)?
                .result
                .as_ref()
                .map(|result| result.disposition)
        ),
        (
            Some(AssistanceReason::Complete),
            Some(AssistanceDisposition::Uncertain)
        ),
        "the template abstention remains a valid uncertain answer"
    );
    Ok(())
}

fn refuse_id_bounds(f: &AssistanceFixture, template: &AssistanceContract) -> FixtureResult {
    let nine: Vec<String> = (0..9).map(|i| format!("id-{i}")).collect();
    assert!(
        enqueue_result(
            f,
            request(f, "packet-nine", "packet_choice", template, json!(nine))?,
            template,
        )
        .is_err(),
        "more than the declared id count refuses enqueue"
    );
    assert!(
        enqueue_result(
            f,
            request(
                f,
                "packet-long",
                "packet_choice",
                template,
                json!(["a".repeat(65)])
            )?,
            template,
        )
        .is_err(),
        "an id over the byte bound refuses enqueue"
    );
    let wide: Vec<String> = (0..8).map(|i| format!("{i:064}")).collect();
    let wide_id = enqueue(
        f,
        request(
            f,
            "packet-wide",
            "packet_choice",
            template,
            json!(wide.clone()),
        )?,
        template,
    )?;
    let mut expected = wide;
    expected.push(ABSTAIN.to_owned());
    assert_eq!(
        choice_allowed(
            &f.record(&wide_id)?.work.request.contract,
            "decision_choice"
        )?,
        expected,
        "eight ids of 64 bytes are inside the bound"
    );
    let tight = packet_template(2, 64);
    grant(f, "packet_choice_tight", &tight, &["option_ids"])?;
    assert!(
        enqueue_result(
            f,
            request(
                f,
                "packet-three",
                "packet_choice_tight",
                &tight,
                json!(["a", "b", "c"])
            )?,
            &tight,
        )
        .is_err(),
        "a tighter declared id count refuses the extra id"
    );
    let tight_id = enqueue(
        f,
        request(
            f,
            "packet-two",
            "packet_choice_tight",
            &tight,
            json!(["a", "b"]),
        )?,
        &tight,
    )?;
    assert_eq!(
        choice_allowed(
            &f.record(&tight_id)?.work.request.contract,
            "decision_choice"
        )?,
        vec!["a".to_owned(), "b".to_owned(), ABSTAIN.to_owned()]
    );
    Ok(())
}

fn refuse_packet_shapes(f: &AssistanceFixture, template: &AssistanceContract) -> FixtureResult {
    for (subject, ids) in [
        ("packet-empty", json!([])),
        ("packet-duplicate", json!(["approve", "approve"])),
        ("packet-abstain-id", json!([ABSTAIN])),
        ("packet-missing", Value::Null),
        ("packet-text", json!("approve")),
        ("packet-number", json!([1])),
    ] {
        let mut packet_request = request(f, subject, "packet_choice", template, ids.clone())?;
        if ids.is_null() {
            packet_request.evidence_packet.fields.clear();
        }
        assert!(
            enqueue_result(f, packet_request, template).is_err(),
            "{subject} must refuse enqueue"
        );
    }
    Ok(())
}

fn refuse_ungranted_field_and_ceiling(
    f: &AssistanceFixture,
    template: &AssistanceContract,
) -> FixtureResult {
    grant(f, "packet_choice_ungranted", template, &["excerpts"])?;
    assert!(
        enqueue_result(
            f,
            request(
                f,
                "packet-ungranted",
                "packet_choice_ungranted",
                template,
                json!(["approve"]),
            )?,
            template,
        )
        .is_err(),
        "an ungranted packet field cannot supply option ids"
    );
    for (subject, max_ids, max_bytes) in [
        ("packet-ceiling-count", 9_u8, 64_u8),
        ("packet-ceiling-bytes", 8, 65),
    ] {
        let invalid = packet_template(max_ids, max_bytes);
        assert!(
            enqueue_result(
                f,
                request(f, subject, "packet_choice", &invalid, json!(["approve"]))?,
                &invalid,
            )
            .is_err(),
            "{subject} exceeds the service ceiling and must refuse enqueue"
        );
    }
    assert!(
        f.service
            .enqueue_assistance(
                request(
                    f,
                    "packet-no-template",
                    "packet_choice",
                    template,
                    json!(["approve"])
                )?,
                &[SUPPLIED.into()],
            )
            .is_err(),
        "a packet-bound question without a granted template is refused"
    );
    Ok(())
}

fn refuse_answer(
    f: &AssistanceFixture,
    template: &AssistanceContract,
    subject: &str,
    distribution: &[String],
    chosen: &str,
    probabilities: (f64, f64, f64),
) -> FixtureResult {
    let id = enqueue(
        f,
        request(
            f,
            subject,
            "packet_choice",
            template,
            json!(["approve", "reject"]),
        )?,
        template,
    )?;
    f.respond(choice_reply(distribution, chosen, probabilities))?;
    assert!(
        f.service
            .process_next_assistance("ws_fixture", "packet_choice")?
            .is_some(),
        "{subject} must be processed"
    );
    assert_eq!(
        f.record(&id)?.result.as_ref().map(|result| result.reason),
        Some(AssistanceReason::Malformed),
        "{subject} must be a malformed answer"
    );
    Ok(())
}

fn fixed_choice_template() -> AssistanceContract {
    AssistanceContract {
        format_version: assistance::ASSISTANCE_FORMAT.into(),
        question_set_version: "legacy-choice/1".into(),
        rubric_version: "legacy-rubric/1".into(),
        evidence_builder_version: "legacy-evidence/1".into(),
        result_schema_version: assistance::ASSISTANCE_RESULT_SCHEMA.into(),
        questions: BTreeMap::from([(
            "grade".into(),
            AssistanceQuestion::Choice {
                rubric: "Pick the recorded grade.".into(),
                allowed: vec!["adequate".into(), "deficient".into()],
                abstain_values: vec!["deficient".into()],
                min_confidence: None,
                criterion_refs: vec![],
                packet_bound: None,
            },
        )]),
    }
}

fn packet_template(max_ids: u8, max_id_bytes: u8) -> AssistanceContract {
    AssistanceContract {
        format_version: assistance::ASSISTANCE_FORMAT.into(),
        question_set_version: "packet-choice/1".into(),
        rubric_version: "packet-rubric/1".into(),
        evidence_builder_version: "packet-evidence/1".into(),
        result_schema_version: assistance::ASSISTANCE_RESULT_SCHEMA.into(),
        questions: BTreeMap::from([(
            "decision_choice".into(),
            AssistanceQuestion::Choice {
                rubric: "Choose one supplied option, or abstain.".into(),
                allowed: vec![ABSTAIN.into()],
                abstain_values: vec![ABSTAIN.into()],
                min_confidence: Some(0.9),
                criterion_refs: vec![],
                packet_bound: Some(AssistancePacketBoundChoice {
                    field: "option_ids".into(),
                    max_ids,
                    max_id_bytes,
                }),
            },
        )]),
    }
}

fn grant(
    f: &AssistanceFixture,
    use_case: &str,
    template: &AssistanceContract,
    fields: &[&str],
) -> FixtureResult {
    let digest = assistance::assistance_digest(&serde_json::to_vec(template)?);
    let mut settings = f.settings.lock().map_err(|_| "settings lock")?;
    let mut policy = settings.policies[0].clone();
    policy.use_case = use_case.into();
    policy.contract_digest = digest;
    policy.evidence_fields = fields.iter().map(|field| (*field).to_owned()).collect();
    settings
        .policies
        .retain(|policy| policy.use_case != use_case);
    settings.policies.push(policy);
    Ok(())
}

fn request(
    f: &AssistanceFixture,
    subject: &str,
    use_case: &str,
    contract: &AssistanceContract,
    option_ids: Value,
) -> FixtureResult<AssistanceRequest> {
    Ok(AssistanceRequest {
        workspace_id: "ws_fixture".into(),
        owner_id: "fixture-host".into(),
        use_case: use_case.into(),
        source_digest: assistance::assistance_digest(subject.as_bytes()),
        contract: contract.clone(),
        evidence_packet: AssistanceEvidencePacket {
            packet_id: subject.into(),
            subject_id: subject.into(),
            collected_at: f.now()?,
            evidence_refs: vec!["fixture:permitted".into()],
            omissions: vec![],
            truncated: false,
            fields: BTreeMap::from([("option_ids".into(), option_ids)]),
        },
    })
}

fn enqueue(
    f: &AssistanceFixture,
    request: AssistanceRequest,
    template: &AssistanceContract,
) -> FixtureResult<String> {
    match enqueue_result(f, request, template)? {
        AssistanceAdmission::Enqueued(id) | AssistanceAdmission::Existing(id) => Ok(id),
        other => Err(format!("packet-bound fixture was not admitted: {other:?}").into()),
    }
}

fn enqueue_result(
    f: &AssistanceFixture,
    request: AssistanceRequest,
    template: &AssistanceContract,
) -> FixtureResult<AssistanceAdmission> {
    Ok(f.service
        .enqueue_expanded_assistance(request, &[SUPPLIED.into()], template)?)
}

fn choice_allowed(contract: &AssistanceContract, name: &str) -> FixtureResult<Vec<String>> {
    match contract.questions.get(name) {
        Some(AssistanceQuestion::Choice { allowed, .. }) => Ok(allowed.clone()),
        _ => Err(format!("missing choice {name}").into()),
    }
}

fn choice_reply(allowed: &[String], chosen: &str, probabilities: (f64, f64, f64)) -> Value {
    let (other_p, chosen_p, confidence) = probabilities;
    json!({
        "model": "gpt-6-luna",
        "answers": [{
            "type": "choice",
            "name": "decision_choice",
            "choice": chosen,
            "confidence": confidence,
            "probabilities": allowed.iter().map(|value| json!({
                "value": value,
                "probability": if value == chosen { chosen_p } else { other_p }
            })).collect::<Vec<_>>()
        }],
        "usage": {"input_tokens": 10, "output_tokens": 0}
    })
}

fn assert_absent(value: &Value, key: &str) -> FixtureResult {
    match value {
        Value::Object(map) => {
            if map.contains_key(key) {
                return Err(format!("{key} leaked into serialization").into());
            }
            for child in map.values() {
                assert_absent(child, key)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                assert_absent(child, key)?;
            }
        }
        _ => {}
    }
    Ok(())
}
