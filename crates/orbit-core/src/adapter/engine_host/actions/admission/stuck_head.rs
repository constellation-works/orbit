//! The friction an `enforce` drain files for a stuck gap-fit head.
//!
//! Gap fit marks a protected head stuck once its miss has not improved over
//! `head_stuck_passes` passes (`gap_fit.rs`). While the guard holds it the
//! drain admits nothing behind it, so a stuck head idles the drain: this
//! module makes that visible once per stall, beside the record `run show`
//! and readiness read. It never changes what is admitted.
//!
//! The `dedupe-key` line in the body names the drain, the head and the
//! stall's start, so a retried pass reuses the record a race or an earlier
//! failure to persist the friction's id already filed.

use std::fmt::Write as _;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::FrictionAddParams;
use orbit_types::record::FrictionStatus;
use orbit_types::workflow::{GapFitHead, GapFitHeadMiss, GapFitHeadStall};

use crate::OrbitRuntime;

/// The friction tag for a stuck head, when the workspace knows it.
const FRICTION_TAG: &str = "automation";
/// Last-resort tag when a workspace's vocabulary lacks [`FRICTION_TAG`].
const FALLBACK_TAG: &str = "other";

/// File the friction for `head` once its stall is stuck and none is
/// recorded, and record its id on the stall. Best effort: a drain that
/// cannot file it still admits as it would and reports the head from its
/// run state; the next pass tries again.
pub(in crate::adapter::engine_host::actions) fn report_stuck_head(
    runtime: &OrbitRuntime,
    run_id: &str,
    head: &mut GapFitHead,
    title: Option<&str>,
) {
    let (Some(miss), Some(stall)) = (head.miss.as_ref(), head.stall.as_mut()) else {
        return;
    };
    if !stall.stuck || stall.friction_id.is_some() {
        return;
    }
    match file(
        runtime,
        run_id,
        &head.task_id,
        head.failing_since,
        title,
        miss,
        stall,
    ) {
        Ok(id) => stall.friction_id = Some(id),
        Err(error) => tracing::warn!(
            target: "orbit.core.job_run",
            run_id,
            task_id = %head.task_id,
            %error,
            "drain could not file the friction for its stuck gap-fit head; run show still reports it"
        ),
    }
}

fn file(
    runtime: &OrbitRuntime,
    run_id: &str,
    task_id: &str,
    failing_since: chrono::DateTime<Utc>,
    title: Option<&str>,
    miss: &GapFitHeadMiss,
    stall: &GapFitHeadStall,
) -> Result<String, OrbitError> {
    let key = format!(
        "gap-fit-stuck-head:{run_id}:{task_id}:{}",
        stall.since.to_rfc3339()
    );
    let named = title.map_or_else(
        || task_id.to_string(),
        |title| format!("{task_id} ({title})"),
    );
    let profile = match &miss.profile_key {
        Some(key) => format!("`{key}` via `{}`", miss.profile_source),
        None => format!("none (`{}`)", miss.profile_source),
    };
    let baseline = miss.baseline_percent.map_or_else(
        || "unmeasured".to_string(),
        |percent| format!("{percent:.0}%"),
    );
    let mut body = String::new();
    let _ = writeln!(
        body,
        "Drain `{run_id}` has held everything behind its gap-fit queue head {named} since \
         {}: the starvation guard protects it, and its miss has not improved over {} \
         consecutive passes since {}. Admission is unchanged; the head is admitted once it \
         fits, or once the host holds no running delivery or reservation.",
        failing_since.format("%Y-%m-%d %H:%MZ"),
        stall.passes,
        stall.since.format("%Y-%m-%d %H:%MZ"),
    );
    let _ = writeln!(body);
    let _ = writeln!(body, "- task: `{task_id}`");
    let _ = writeln!(body, "- profile: {profile}");
    let _ = writeln!(
        body,
        "- miss: {} {:.0}% at +{}m",
        miss.resource, miss.percent, miss.minute
    );
    let _ = writeln!(body, "- {} baseline: {baseline}", miss.resource);
    let _ = writeln!(body);
    let _ = writeln!(
        body,
        "`orbit run show {run_id}` and `orbit run readiness` report the head while it lasts. \
         A stale or oversized profile (`orbit run profiles`) or a high baseline is the usual \
         cause."
    );
    let _ = writeln!(body, "dedupe-key: {key}");

    let frictions = crate::runtime::friction::store_for(runtime)?;
    let accepted = frictions.tags()?;
    let tags = [FRICTION_TAG, FALLBACK_TAG]
        .into_iter()
        .find(|tag| accepted.iter().any(|known| known == tag))
        .map(str::to_string)
        .into_iter()
        .collect();
    let stored = frictions.add_or_reuse(
        &key,
        FrictionAddParams {
            model: "system".to_string(),
            title: Some(format!(
                "Gap-fit queue head {named} is stuck: {} {:.0}% at +{}m has not improved",
                miss.resource, miss.percent, miss.minute
            )),
            body,
            tags,
            status: FrictionStatus::Open,
            during_task: None,
            created_at: Utc::now(),
        },
    )?;
    Ok(stored.record.id)
}
