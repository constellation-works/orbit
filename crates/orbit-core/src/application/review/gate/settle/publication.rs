use orbit_common::OrbitError;
use orbit_store::contracts::{ClaimEvidence, ClaimWorkerUpdate};
use orbit_types::workflow::{REVIEW_GATE_ARTIFACT, ReviewCertificate};
use serde_json::json;

use crate::OrbitRuntime;
use crate::application::review::gate::context::GateContext;
use crate::application::review::gate::judgement::{
    Delivery, comment_lead, verdict_comment, write_artifact,
};
use crate::application::task::TaskUpdateParams;

/// Give every task the certificate artifact and verdict comment, writing
/// only what is missing so a replay after a partial publish neither loses
/// nor duplicates evidence. A task already carrying a later attempt's
/// certificate keeps it.
pub(super) fn publish_certificate(
    runtime: &OrbitRuntime,
    context: &GateContext,
    certificate: &ReviewCertificate,
) -> Result<(), OrbitError> {
    let certificate_bytes = serde_json::to_vec_pretty(certificate)
        .map_err(|error| OrbitError::Execution(format!("serialize review certificate: {error}")))?;
    let landing = context
        .admission
        .as_ref()
        .is_some_and(|admission| admission.gates_landing());
    for task in &context.tasks {
        // [ORB-15130] A before-landing comment states the PR and the status
        // the store holds when it is written, not the before-PR template.
        let delivery = if landing {
            Delivery::before_landing(&runtime.get_task(&task.id)?)
        } else {
            Delivery::BeforePr
        };
        let comment = verdict_comment(certificate, &delivery);
        let lead = comment_lead(certificate, &delivery);
        let current = runtime.get_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)?;
        let current_matches = current
            .as_ref()
            .is_some_and(|artifact| artifact.content == certificate_bytes);
        let superseded = current
            .as_ref()
            .and_then(|artifact| {
                serde_json::from_slice::<ReviewCertificate>(&artifact.content).ok()
            })
            .is_some_and(|held| {
                held.attempt_id != certificate.attempt_id && held.issued_at > certificate.issued_at
            });
        if superseded {
            continue;
        }
        if !current_matches {
            write_artifact(
                runtime,
                &task.id,
                &context.run_id,
                REVIEW_GATE_ARTIFACT,
                &certificate_bytes,
            )?;
        }
        // The status line may differ on a replay, so a comment already
        // posted for this attempt and verdict counts as disclosed.
        let disclosed = runtime
            .get_task_comments(&task.id)?
            .iter()
            .any(|existing| existing.message.trim_start().starts_with(&lead));
        if !disclosed {
            post_comment(runtime, context, &task.id, &comment)?;
        }
    }
    Ok(())
}

/// Post the verdict comment. A claimed leaf's comment crosses its binding to
/// the owner as claim evidence [ORB-13908].
fn post_comment(
    runtime: &OrbitRuntime,
    context: &GateContext,
    task_id: &str,
    comment: &str,
) -> Result<(), OrbitError> {
    if context.claimed {
        runtime.route_worker_host_tool(
            "orbit.task.update",
            json!({
                "id": task_id,
                "_worker_update": ClaimWorkerUpdate {
                    evidence: ClaimEvidence {
                        comment: Some(comment.to_string()),
                        ..ClaimEvidence::default()
                    },
                    ..ClaimWorkerUpdate::default()
                },
            }),
        )?;
        return Ok(());
    }
    runtime.update_task_as_system(
        task_id,
        TaskUpdateParams {
            comment: Some(comment.to_string()),
            ..TaskUpdateParams::default()
        },
        None,
    )?;
    Ok(())
}
