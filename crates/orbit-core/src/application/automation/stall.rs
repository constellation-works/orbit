//! Operator-visible reporting for a delivery consumer that stopped making
//! progress.
//!
//! The evaluator decides *that* a consumer is stalled; this module is how a
//! human finds out. One friction record per divergence carries the obligations
//! and the exact way out, and `orbit doctor` reads the same markers so a
//! suspended consumer is visible without knowing which definition to inspect.
//!
//! Deduplication is on the divergence, not the consumer: every consumer
//! observing the branch sees the same rewrite, and one record for it is the
//! useful signal. The record names the consumer that filed it and points at
//! `orbit doctor` for the full set.

use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::delivery::stall::StallReport;
use orbit_common::OrbitError;
use orbit_store::contracts::{FrictionAddParams, FrictionListFilter};
use orbit_types::workflow::automation::recovery::AutomationStall;
use std::fmt::Write as _;

/// Tag every automation stall record carries.
const AUTOMATION_TAG: &str = "automation";
/// Tag identifying a rewritten branch history.
const DIVERGENCE_TAG: &str = "history-diverged";
/// Last-resort tag when a workspace's vocabulary knows none of the others.
const FALLBACK_TAG: &str = "other";
/// Bound on consumer states scanned for a stall report.
const CONSUMER_SCAN_LIMIT: usize = 100;

/// One stalled consumer on this host, as reported by `orbit doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledConsumer {
    pub consumer: String,
    pub stall: AutomationStall,
}

impl StalledConsumer {
    /// The definition name behind the consumer key.
    pub fn definition(&self) -> &str {
        definition_name(&self.consumer)
    }
}

/// File one friction for `report`, or reuse the record an earlier evaluation
/// already filed for the same divergence. Answers with the record's id so the
/// eventual recovery or reset can link it.
pub(super) fn report(
    runtime: &OrbitRuntime,
    report: &StallReport<'_>,
) -> Result<Option<String>, OrbitError> {
    let frictions = crate::runtime::friction::store_for(runtime)?;
    let key = dedupe_key(report);

    let existing = frictions.list(&FrictionListFilter {
        q: Some(key.clone()),
        limit: Some(1),
        ..FrictionListFilter::default()
    })?;
    if let Some(found) = existing.first() {
        return Ok(Some(found.record.id.clone()));
    }

    let stored = frictions.add(FrictionAddParams {
        model: "system".to_string(),
        title: Some(title(report)),
        body: body(report, &key),
        tags: tags(report, &frictions.tags()?),
        during_task: None,
        created_at: report.at,
    })?;

    Ok(Some(stored.record.id))
}

/// Every consumer on this host whose evaluation is suspended by a stall.
pub fn stalled_consumers(runtime: &OrbitRuntime) -> Result<Vec<StalledConsumer>, OrbitError> {
    let Some(machine) = runtime.automation_machine_identity() else {
        return Ok(vec![]);
    };
    let prefix = format!("{machine}/{}/", runtime.workspace_id()?);

    Ok(runtime
        .automation_store()?
        .automation_states(&prefix, CONSUMER_SCAN_LIMIT)?
        .into_iter()
        .filter_map(|state| {
            Some(StalledConsumer {
                consumer: state.consumer,
                stall: state.stall?,
            })
        })
        .collect())
}

/// The stable substring that identifies this divergence across consumers,
/// ticks and hosts. It is written into the body so the ordinary friction
/// search is the dedupe query.
fn dedupe_key(report: &StallReport<'_>) -> String {
    let mut key = format!(
        "automation-stall:{}:{}:{}",
        report.repository, report.branch, report.reason
    );
    // The orphaned revision alone identifies the divergence. Keying on the new
    // head as well would file a fresh record every time the branch grew, which
    // is the spam this record exists to replace.
    if let Some(divergence) = report.divergence {
        let _ = write!(key, ":{}", divergence.observed.commit);
    }

    key
}

/// The record's tags, narrowed to the vocabulary this workspace validates.
///
/// The taxonomy is per-workspace data seeded once, so a workspace created
/// before these tags existed does not know them. Losing a tag is acceptable;
/// losing the record because of a tag is not.
fn tags(report: &StallReport<'_>, accepted: &[String]) -> Vec<String> {
    let mut wanted = vec![AUTOMATION_TAG];
    if report.divergence.is_some() {
        wanted.push(DIVERGENCE_TAG);
    }

    let tags = wanted
        .iter()
        .filter(|tag| accepted.iter().any(|known| known == *tag))
        .map(|tag| (*tag).to_string())
        .collect::<Vec<_>>();

    if !tags.is_empty() {
        return tags;
    }

    accepted
        .iter()
        .find(|known| *known == FALLBACK_TAG)
        .cloned()
        .into_iter()
        .collect()
}

fn title(report: &StallReport<'_>) -> String {
    let definition = definition_name(report.consumer);
    if report.repaired {
        return format!(
            "Delivery automation replayed a rewritten `{}` history for {definition}",
            report.branch
        );
    }

    format!(
        "Delivery automation is stalled on {definition}: {}",
        report.reason
    )
}

fn body(report: &StallReport<'_>, key: &str) -> String {
    let definition = definition_name(report.consumer);
    let mut body = String::new();

    if report.repaired {
        let _ = writeln!(
            body,
            "The `{}` branch history was rewritten in a way the replay proof showed preserved \
             every observed commit's content, so the evaluator reconciled the consumer itself \
             and kept all coverage debt. No operator action is required; this record exists so \
             the rewrite is not invisible.",
            report.branch
        );
    } else {
        let _ = writeln!(
            body,
            "Delivery automation stopped evaluating `{definition}` and will not resume on its \
             own. Every obligation it holds is retained, and nothing new is observed until an \
             operator decides."
        );
    }

    let _ = writeln!(body);
    let _ = writeln!(body, "- consumer: `{}`", report.consumer);
    let _ = writeln!(body, "- repository: `{}`", report.repository);
    let _ = writeln!(body, "- branch: `{}`", report.branch);
    let _ = writeln!(body, "- reason: `{}`", report.reason);

    if let Some(divergence) = report.divergence {
        let _ = writeln!(
            body,
            "- observed `{}` is no longer reachable from head `{}`",
            divergence.observed.commit, divergence.head.commit
        );
        if !divergence.refusal.is_empty() {
            let _ = writeln!(body, "- replay proof refused: `{}`", divergence.refusal);
        }
        if !divergence.obligations.is_empty() {
            let _ = writeln!(body);
            let _ = writeln!(
                body,
                "Obligations the proof could not map onto the new head:"
            );
            for obligation in &divergence.obligations {
                let _ = writeln!(body, "- {obligation}");
            }
        }
    }

    if !report.repaired {
        let _ = writeln!(body);
        let _ = writeln!(
            body,
            "Resolve it with one of:\n\
             - `orbit auto-task recover {definition} --replay-history --reason \"<why>\"` — \
             reconciles a provable rewrite and retains every obligation.\n\
             - `orbit auto-task reset {definition} --reason \"<why>\"` — forgets the debt above \
             and re-baselines at the current branch head.\n\
             \n\
             Run `orbit auto-task reset {definition}` with no reason first: that previews \
             exactly what would be forgotten."
        );
    }

    let _ = writeln!(body);
    let _ = writeln!(
        body,
        "Consumers sharing this branch share this record; `orbit doctor` lists every stalled \
         consumer on this host."
    );
    let _ = writeln!(body, "dedupe-key: {key}");

    body
}

/// The definition name inside a `<machine>/<workspace>/<kind>/<name>` key.
fn definition_name(consumer: &str) -> &str {
    consumer.rsplit('/').next().unwrap_or(consumer)
}

/// A stall's age in whole minutes, for operator-facing reporting.
pub fn stalled_minutes(stall: &AutomationStall, now: DateTime<Utc>) -> i64 {
    now.signed_duration_since(stall.since).num_minutes().max(0)
}
