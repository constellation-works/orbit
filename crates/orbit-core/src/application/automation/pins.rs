//! Per-attempt source pins, `refs/orbit/automation/<attempt id>` [ORB-14164].
//!
//! Admitting a state-member attempt pins the commit it froze so its run can
//! still reach it. Settling, exhausting or retiring the attempt releases the
//! pin; a pin an earlier release left behind is released by
//! `orbit doctor --fix-automation-pins` once an inventory of this host proves
//! nothing still names it. Delivery batch pins live one level deeper, under
//! the consumer digest, and are never touched here.

use super::source::Source;
use crate::OrbitRuntime;
use orbit_automation::AutomationError;
use orbit_common::OrbitError;
use orbit_types::workflow::automation::{AutomationState, members::MemberAttempt};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

const NAMESPACE: &str = "refs/orbit/automation/";

/// Store pages read while inventorying consumers.
const CONSUMER_PAGE_LIMIT: usize = 100;

/// Unreferenced pins deleted per `update-ref --stdin` transaction: about
/// 64 KiB of instructions, well inside one command's budget.
const RELEASE_CHUNK: usize = 500;

fn name(attempt_id: &str) -> String {
    format!("{NAMESPACE}{attempt_id}")
}

/// Attempt ids are lowercase SHA-256 digests; nothing else under the
/// namespace's top level is provably an attempt pin.
fn is_attempt_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Pin the source `attempt` froze.
pub(super) fn pin(source: &Source, attempt: &MemberAttempt) -> Result<(), AutomationError> {
    source.git(&[
        "update-ref",
        &name(&attempt.id),
        &attempt.member.source.commit,
    ])?;
    Ok(())
}

/// The commit `attempt_id` pinned, resolved by its one ref name: the cost is
/// independent of how many pins the namespace holds.
pub(super) fn pinned(source: &Source, attempt_id: &str) -> Result<Option<String>, AutomationError> {
    if !is_attempt_id(attempt_id) {
        return Ok(None);
    }
    let name = name(attempt_id);
    // A literal pattern also matches refs below `<name>/`; only the exact
    // name is this attempt's pin.
    Ok(source
        .git(&["for-each-ref", "--format=%(refname) %(objectname)", &name])?
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find(|(refname, _)| *refname == name)
        .map(|(_, object)| object.to_string()))
}

/// Release `attempt`'s pin. Git deletes it only while it still names the
/// commit the attempt froze, so a ref someone else repointed is never lost.
/// Returns whether a pin was deleted.
pub(super) fn release(source: &Source, attempt: &MemberAttempt) -> Result<bool, AutomationError> {
    let Some(pinned) = pinned(source, &attempt.id)? else {
        return Ok(false);
    };
    if pinned != attempt.member.source.commit {
        return Err(AutomationError::Deferred(format!(
            "automation_pin_moved: {} names {pinned}, not the attempt's {}",
            name(&attempt.id),
            attempt.member.source.commit
        )));
    }
    delete(source, &name(&attempt.id), &pinned)?;
    Ok(true)
}

fn delete(source: &Source, refname: &str, expected: &str) -> Result<(), AutomationError> {
    source.git(&["update-ref", "-d", refname, expected])?;
    Ok(())
}

/// What `orbit doctor --fix-automation-pins` did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AttemptPinCleanup {
    /// Attempt pins nothing on this host named; each was deleted.
    pub released: Vec<String>,
    /// Pins of attempts a consumer still has in flight.
    pub retained_active: usize,
    /// Pins an accepted assessment names, which an assessment certified under
    /// the `material_v1` contract needs to be carried forward.
    pub retained_assessed: usize,
    /// Pins of attempts a pending or running pipeline run still carries.
    pub retained_live_run: usize,
    /// Top-level refs that are not attempt pins; nothing proves whose they
    /// are, so they are left untouched.
    pub unrecognized: Vec<String>,
    /// Unreferenced pins that moved or could not be deleted; left in place.
    pub kept: Vec<String>,
}

/// Release every attempt pin that no persisted consumer state and no live
/// run on this host still names.
///
/// Serialized with admission and settlement by the routine sweep lock: the
/// repair refuses while a sweep holds it. Pins are listed before the
/// inventory is read, and admission commits an attempt before pinning it, so
/// a listed pin whose attempt is still needed is always in the inventory. Any
/// inventory failure deletes nothing.
pub fn release_unreferenced_attempt_pins(
    runtime: &OrbitRuntime,
) -> Result<AttemptPinCleanup, OrbitError> {
    let Some(_lock) =
        orbit_store::try_acquire_routine_sweep_lock(&runtime.global_root().join("state"))?
    else {
        return Err(OrbitError::InvalidInput(
            "a routine sweep is evaluating consumers; retry --fix-automation-pins after it \
             finishes"
                .into(),
        ));
    };

    let root = &runtime.paths().repo_root;
    let mut cleanup = AttemptPinCleanup::default();
    let mut candidates = Vec::new();
    for (refname, object) in top_level_refs(root)? {
        match refname.strip_prefix(NAMESPACE) {
            Some(id) if is_attempt_id(id) => candidates.push((id.to_string(), refname, object)),
            _ => cleanup.unrecognized.push(refname),
        }
    }

    let inventory = Inventory::read(runtime)?;
    let mut unreferenced = Vec::new();
    for (id, refname, object) in candidates {
        if inventory.active.contains(&id) {
            cleanup.retained_active += 1;
        } else if inventory.live_runs.contains(&id) {
            cleanup.retained_live_run += 1;
        } else if inventory.assessed.contains(&id) {
            cleanup.retained_assessed += 1;
        } else {
            unreferenced.push((refname, object));
        }
    }

    // A fresh source per command: the source deadline bounds one evaluation
    // pass, not a repair over every leaked pin.
    for chunk in unreferenced.chunks(RELEASE_CHUNK) {
        let instructions = chunk
            .iter()
            .map(|(refname, object)| format!("delete {refname} {object}\n"))
            .collect::<String>();
        // One transaction: either every pin in the chunk still named its
        // listed commit and is gone, or none was deleted and each is retried
        // alone so one moved ref cannot keep the rest.
        if Source::new(root)
            .git_with_input(&["update-ref", "--stdin"], instructions.as_bytes())
            .is_ok()
        {
            cleanup
                .released
                .extend(chunk.iter().map(|(refname, _)| refname.clone()));
            continue;
        }
        for (refname, object) in chunk {
            match delete(&Source::new(root), refname, object) {
                Ok(()) => cleanup.released.push(refname.clone()),
                Err(error) => {
                    tracing::warn!(
                        refname,
                        error = %error,
                        "an unreferenced automation attempt pin could not be deleted"
                    );
                    cleanup.kept.push(refname.clone());
                }
            }
        }
    }
    Ok(cleanup)
}

/// Every top-level ref under the namespace with the object it names. Listed
/// one leading hex digit at a time so each listing stays far inside the
/// source budget; `*` does not cross `/`, so batch pins are never listed.
fn top_level_refs(root: &std::path::Path) -> Result<Vec<(String, String)>, OrbitError> {
    let mut refs = Vec::new();
    for digit in "0123456789abcdef".chars() {
        let listing = Source::new(root)
            .git(&[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                &format!("{NAMESPACE}{digit}*"),
            ])
            .map_err(orbit_automation::automation_error_to_orbit)?;
        refs.extend(
            listing
                .lines()
                .filter_map(|line| line.split_once(' '))
                .map(|(name, object)| (name.to_string(), object.to_string())),
        );
    }
    Ok(refs)
}

/// Attempt ids this host still names, by why they are needed.
#[derive(Default)]
struct Inventory {
    active: BTreeSet<String>,
    assessed: BTreeSet<String>,
    live_runs: BTreeSet<String>,
}

impl Inventory {
    /// Every consumer in the host store, whichever workspace it belongs to:
    /// workspaces may share one repository's refs, and a wider inventory only
    /// keeps more. Read in bounded pages to exhaustion.
    fn read(runtime: &OrbitRuntime) -> Result<Self, OrbitError> {
        let mut inventory = Self::default();
        let store = runtime.automation_store()?;
        let mut after: Option<String> = None;
        loop {
            let page = store.automation_states_page("", after.as_deref(), CONSUMER_PAGE_LIMIT)?;
            if page.is_empty() {
                break;
            }
            for state in page {
                if after.as_ref().is_some_and(|key| state.consumer <= *key) {
                    return Err(OrbitError::Store(
                        "automation state page is not strictly ordered".into(),
                    ));
                }
                after = Some(state.consumer.clone());
                inventory.add(&state);
            }
        }

        for job in ["task_pilot_pipeline", "task_triage_pipeline"] {
            for run in runtime
                .stores()
                .jobs()
                .list_pending_or_running_job_runs(job)?
            {
                if let Some(id) = run
                    .input
                    .as_ref()
                    .and_then(|input| input.get("state_automation"))
                    .and_then(|claim| claim.get("id"))
                    .and_then(Value::as_str)
                {
                    inventory.live_runs.insert(id.to_string());
                }
            }
        }
        Ok(inventory)
    }

    fn add(&mut self, state: &AutomationState) {
        let Some(members) = &state.members else {
            return;
        };
        if let Some(active) = &members.active {
            self.active.insert(active.id.clone());
        }
        self.assessed.extend(
            members
                .assessed
                .values()
                .map(|assessment| assessment.receipt_id.clone()),
        );
    }
}
