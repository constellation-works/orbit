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
//!
//! An automatic settings adoption is reported the same way, deduplicated on
//! the consumer and its identity change, so a retuned definition is visible
//! without anyone having to act on it.
//!
//! The `dedupe-key` line in the body is the durable identity. A preliminary
//! read can miss when two evaluations race; the store repeats that lookup
//! and the insert in one immediate transaction so both resolve to one id.

use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_automation::delivery::adopt::AdoptionReport;
use orbit_automation::delivery::stall::StallReport;
use orbit_common::OrbitError;
use orbit_store::contracts::{FrictionAddParams, FrictionListFilter};
use orbit_types::record::FrictionStatus;
use orbit_types::workflow::automation::recovery::AutomationStall;
use std::fmt::Write as _;

/// Tag every automation stall record carries.
const AUTOMATION_TAG: &str = "automation";
/// Tag identifying a rewritten branch history.
const DIVERGENCE_TAG: &str = "history-diverged";
/// Last-resort tag when a workspace's vocabulary knows none of the others.
const FALLBACK_TAG: &str = "other";
/// Bound on each page of consumer states read for a complete stall report.
const CONSUMER_PAGE_LIMIT: usize = 100;

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
    let key = dedupe_key(report);
    let mut wanted = vec![AUTOMATION_TAG];
    if report.divergence.is_some() {
        wanted.push(DIVERGENCE_TAG);
    }

    file(
        runtime,
        key.clone(),
        title(report),
        body(report, &key),
        &wanted,
        report.at,
        if report.repaired {
            FrictionStatus::Resolved
        } else {
            FrictionStatus::Open
        },
    )
}

/// File one friction for an automatic settings adoption, or reuse the record
/// an earlier evaluation filed for the same consumer and identity change.
///
/// Nothing is stalled and no operator is asked for anything: the record
/// exists so an edit that changed the consumer's identity is not invisible.
pub(super) fn report_adoption(
    runtime: &OrbitRuntime,
    report: &AdoptionReport<'_>,
) -> Result<Option<String>, OrbitError> {
    let key = format!(
        "automation-settings-adopted:{}:{}->{}",
        report.consumer, report.previous_epoch, report.epoch
    );
    let definition = definition_name(report.consumer);
    let changes = report.changes.join(", ");

    let mut body = String::new();
    let _ = writeln!(
        body,
        "Delivery automation adopted an edited `{definition}` definition on its own: only its \
         settings changed, which cannot alter what the retained coverage debt means. Every \
         covered, pending, unresolved, waived and excluded landing and every receipt was kept, \
         and a recovery record attributed to `system:automation` names the old and new \
         identity. No operator action is required; this record exists so the edit is not \
         invisible."
    );
    let _ = writeln!(body);
    let _ = writeln!(body, "- consumer: `{}`", report.consumer);
    let _ = writeln!(body, "- repository: `{}`", report.repository);
    let _ = writeln!(body, "- branch: `{}`", report.branch);
    let _ = writeln!(body, "- changed: {changes}");
    let _ = writeln!(
        body,
        "- identity: `{}` -> `{}`",
        report.previous_epoch, report.epoch
    );
    let _ = writeln!(body);
    let _ = writeln!(
        body,
        "`orbit auto-task recover {definition}` previews the consumer and lists the record."
    );
    let _ = writeln!(body, "dedupe-key: {key}");

    file(
        runtime,
        key,
        format!("Delivery automation adopted changed settings for {definition}: {changes}"),
        body,
        &[AUTOMATION_TAG],
        report.at,
        FrictionStatus::Resolved,
    )
}

/// Find the friction whose body carries `key`, or insert one.
fn file(
    runtime: &OrbitRuntime,
    key: String,
    title: String,
    body: String,
    wanted: &[&str],
    at: DateTime<Utc>,
    status: FrictionStatus,
) -> Result<Option<String>, OrbitError> {
    let frictions = crate::runtime::friction::store_for(runtime)?;

    let existing = frictions.list(&FrictionListFilter {
        q: Some(key.clone()),
        limit: Some(1),
        ..FrictionListFilter::default()
    })?;
    if let Some(found) = existing
        .iter()
        .find(|row| body_marks_dedupe(&row.record.body, &key))
    {
        return Ok(Some(found.record.id.clone()));
    }

    // Two reporters can both miss the list above. `add_or_reuse` looks again
    // under the write lock before inserting.
    #[cfg(test)]
    dedupe_miss_hook::run();

    let stored = frictions.add_or_reuse(
        &key,
        FrictionAddParams {
            model: "system".to_string(),
            title: Some(title),
            body,
            tags: tags(wanted, &frictions.tags()?),
            status,
            during_task: None,
            created_at: at,
        },
    )?;

    Ok(Some(stored.record.id))
}

/// Every consumer in this workspace on this host whose evaluation is suspended
/// by a stall. Read bounded pages to exhaustion; an incomplete scan is an error,
/// never a healthy or partial result.
pub fn stalled_consumers(runtime: &OrbitRuntime) -> Result<Vec<StalledConsumer>, OrbitError> {
    let Some(machine) = runtime.automation_machine_identity() else {
        return Ok(vec![]);
    };
    let prefix = format!("{machine}/{}/", runtime.workspace_id()?);

    let store = runtime.automation_store()?;
    let mut after: Option<String> = None;
    let mut stalled = Vec::new();
    loop {
        let page = store.automation_states_page(&prefix, after.as_deref(), CONSUMER_PAGE_LIMIT)?;
        if page.is_empty() {
            return Ok(stalled);
        }
        for state in page {
            if !state.consumer.starts_with(&prefix)
                || after.as_ref().is_some_and(|key| state.consumer <= *key)
            {
                return Err(OrbitError::Store(
                    "automation state page is outside its prefix or not strictly ordered".into(),
                ));
            }
            after = Some(state.consumer.clone());
            if let Some(stall) = state.stall {
                stalled.push(StalledConsumer {
                    consumer: state.consumer,
                    stall,
                });
            }
        }
    }
}

/// The stable substring that identifies this divergence across consumers,
/// ticks and hosts. It is written into the body as a `dedupe-key` line, and
/// that exact line is the identity a later report matches.
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
fn tags(wanted: &[&str], accepted: &[String]) -> Vec<String> {
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

fn body_marks_dedupe(body: &str, key: &str) -> bool {
    let marker = format!("dedupe-key: {key}");
    body.lines().any(|line| line == marker)
}

/// The definition name inside a `<machine>/<workspace>/<kind>/<name>` key.
fn definition_name(consumer: &str) -> &str {
    consumer.rsplit('/').next().unwrap_or(consumer)
}

/// A stall's age in whole minutes, for operator-facing reporting.
pub fn stalled_minutes(stall: &AutomationStall, now: DateTime<Utc>) -> i64 {
    now.signed_duration_since(stall.since).num_minutes().max(0)
}

/// Test seam between a missed preliminary lookup and the keyed insert.
///
/// Production has no hook. A test installs one so two reporters can both
/// observe the miss before either insert runs.
#[cfg(test)]
pub(crate) mod dedupe_miss_hook {
    use std::sync::{Arc, Mutex};

    static HOOK: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

    pub(crate) fn install(hook: impl Fn() + Send + Sync + 'static) {
        *HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(hook));
    }

    pub(crate) fn clear() {
        *HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    pub(super) fn run() {
        let hook = HOOK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}
