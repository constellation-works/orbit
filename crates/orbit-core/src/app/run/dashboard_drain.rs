use super::exec::require;
use super::{Command, Operation};
use crate::application::job::{
    AutoDrainSubmissionProfile, AutoDrainSubmissionReceipt, DrainAdmissionsStopResult,
};
use crate::{
    AutoDrainRequest, CompletionPolicy, DrainAdmissionsStopRequest, DrainEntryPoint, OrbitRuntime,
};
use orbit_common::governance::authorization::{
    DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED, DASHBOARD_AUTO_DRAIN_AUTO_FIX,
    DASHBOARD_AUTO_DRAIN_COMPLETE, DASHBOARD_AUTO_DRAIN_MODE, DASHBOARD_AUTO_DRAIN_STOP,
};
use serde_json::{Value, json};

impl<'a> Command<'a, AutoDrainSubmissionReceipt> {
    /// Submit the local drain. Each control the request asks for is a
    /// requirement, in this order, so the first refusal is the one reported.
    pub fn dashboard_auto(request: AutoDrainRequest<'a>) -> Self {
        let operation = Operation::command("run", "auto")
            .target(Some("workflow"), Some("auto".into()))
            .requires_if(
                request.completion == CompletionPolicy::Done,
                &DASHBOARD_AUTO_DRAIN_COMPLETE,
            )
            .requires_if(
                request.approve_proposed,
                &DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED,
            )
            .requires_if(request.auto_fix, &DASHBOARD_AUTO_DRAIN_AUTO_FIX)
            .requires_if(
                request.auto_concurrency.is_some(),
                &DASHBOARD_AUTO_DRAIN_MODE,
            );
        Self::new(operation, move |core| {
            super::submission::submit_auto(
                core.runtime(),
                request,
                AutoDrainSubmissionProfile::Http,
            )
        })
    }
}

impl<'a, E: 'a> Command<'a, Result<DrainAdmissionsStopResult, E>> {
    /// Stop the drain's admissions. A malformed body, the inner error, is
    /// reported only to a session admitted to stop the drain.
    pub fn dashboard_stop(input: Result<(Option<String>, Option<String>), E>) -> Self {
        Self::new(
            Operation::command("run", "auto").requires(&DASHBOARD_AUTO_DRAIN_STOP),
            move |core| {
                let (reason, claim) = match input {
                    Ok(fields) => fields,
                    Err(error) => return Ok(Err(error)),
                };
                let result =
                    core.runtime()
                        .stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
                            actor: "dashboard",
                            source: "dashboard",
                            reason: reason.as_deref(),
                            claim_token: claim.as_deref(),
                            force: false,
                        })?;
                Ok(Ok(result))
            },
        )
    }
}

impl<'a> Command<'a, Value> {
    /// Read dashboard readiness and available start controls.
    pub fn dashboard_readiness(
        ids: &'a [String],
        detail: Option<&'a str>,
        concurrency: Option<u32>,
        limit: usize,
        crews: &'a [String],
        hosts: Option<&'a orbit_registry::hosts::HostRegistry>,
    ) -> Self {
        Self::new(
            Operation::command("run", "readiness")
                .target(Some("workspace"), Some("auto_readiness".into())),
            move |core| {
                let runtime = core.runtime();
                let mut payload = runtime.workspace_auto_readiness_with_detail(
                    ids,
                    detail,
                    concurrency,
                    limit,
                    crews,
                )?;
                payload["start_options"] = drain_start_options(runtime, hosts)?;
                // Whether the caller holds every start control; a read, so
                // nothing is refused.
                let controls_authorized = [
                    &DASHBOARD_AUTO_DRAIN_COMPLETE,
                    &DASHBOARD_AUTO_DRAIN_APPROVE_PROPOSED,
                    &DASHBOARD_AUTO_DRAIN_AUTO_FIX,
                    &DASHBOARD_AUTO_DRAIN_STOP,
                    &DASHBOARD_AUTO_DRAIN_MODE,
                ]
                .into_iter()
                .all(|control| require(core, control).is_ok());
                if let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "controls_authorized".into(),
                        Value::Bool(controls_authorized),
                    );
                }
                Ok(payload)
            },
        )
    }
}

/// What the Start a window composer offers on this checkout: whether it is
/// the workspace's owner (a local drain) or a replica (a pull drain from its
/// owner, with the owner's host name and the selector `--pull` resolves to),
/// the runtime's own refusal for the source this checkout cannot start, the
/// configured crews for `--allow-crew`, the workspace pools each
/// `--<tier>-complexity-crews` override replaces, and the review layer
/// before landing workspace policy applies (`review`, as a run would capture
/// it: `{"timing", "source"}`) [ORB-20510]. A pool configuration the
/// runtime cannot read leaves `complexity_pools` null; submission reports why.
fn drain_start_options(
    runtime: &OrbitRuntime,
    hosts: Option<&orbit_registry::hosts::HostRegistry>,
) -> Result<Value, crate::OrbitError> {
    let workspace_id = runtime
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone());
    let owner = runtime.replica_owner_machine().map(str::to_string);
    let owner_host = owner.as_deref().and_then(|machine_id| {
        hosts?
            .hosts()
            .entries()
            .iter()
            .find(|entry| entry.machine_id == machine_id)
            .map(|entry| entry.name.clone())
    });
    let local_refusal = match owner {
        Some(_) => runtime
            .drain_entry_admission(DrainEntryPoint::OwnerDrain, &[])?
            .into_result()
            .err()
            .map(|error| error.to_string()),
        None => None,
    };
    let crews = runtime
        .configured_crew_registry_projection()
        .crews
        .into_iter()
        .map(|crew| json!({"name": crew.name, "enabled": crew.enabled}))
        .collect::<Vec<_>>();
    let complexity_pools = runtime
        .workspace_complexity_crew_pools()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "workspace complexity pools unreadable; the composer shows none");
            Value::Null
        });
    let policy = runtime.operation_policy();
    let review = if policy.review_before_pr.value {
        json!({"timing": "before-pr", "source": policy.review_before_pr.source.label()})
    } else if policy.review_before_landing.value {
        json!({"timing": "before-landing", "source": policy.review_before_landing.source.label()})
    } else {
        json!({"timing": "none", "source": policy.review_before_landing.source.label()})
    };
    Ok(json!({
        "checkout": if owner.is_some() { "replica" } else { "owner" },
        "workspace_id": workspace_id,
        "this_host": hosts.and_then(|hosts| hosts.local()).map(|local| local.name.clone()),
        "owner_machine_id": owner,
        "owner_host": owner_host,
        "pull_selector": owner
            .as_deref()
            .zip(workspace_id.as_deref())
            .map(|(machine, workspace)| format!("{machine}/{workspace}")),
        "local_refusal": local_refusal,
        "pull_refusal": runtime
            .pull_drain_checkout_refusal()
            .map(|error| error.to_string()),
        "crews": crews,
        "complexity_pools": complexity_pools,
        "review": review,
    }))
}
