//! Per-attempt source pins [ORB-14164].
//!
//! Admitting a state-member attempt pins the commit it froze so its run can
//! still reach it. Several Orbit roots and workspaces can share one Git common
//! directory while each keeps its consumers and runs in its own store, so only
//! the owner's own state can prove a pin unused. A pin therefore lives in the
//! namespace of exactly one owner — the canonical Orbit root, the workspace
//! partition, the machine and the canonical Git common directory —
//! `refs/orbit/pins/v1/<owner digest>/<attempt id>`, and the owner's record
//! under `refs/orbit/pin-owners/v1/<owner digest>` must match before anything
//! in it is deleted. A moved or copied root or repository computes a different
//! owner, so it never adopts the pins it left behind.
//!
//! Settling, exhausting or retiring an attempt releases its owned pin;
//! `orbit doctor --fix-automation-pins` reclaims an owned pin a crash left
//! behind. Neither touches another owner's namespace, the delivery batch pins,
//! or the legacy `refs/orbit/automation/<attempt id>` pins earlier releases
//! wrote into one shared namespace, which no owner can prove are its own.

use super::source::Source;
use crate::OrbitRuntime;
use orbit_automation::{AutomationError, automation_error_to_orbit, delivery::digest};
use orbit_common::OrbitError;
use orbit_types::workflow::automation::{AutomationState, members::MemberAttempt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// The shared namespace releases before owner scoping pinned attempts in.
/// Read as a bounded exact fallback; never written or deleted here.
const LEGACY_NAMESPACE: &str = "refs/orbit/automation/";
const OWNED_NAMESPACE: &str = "refs/orbit/pins/v1/";
const OWNER_RECORDS: &str = "refs/orbit/pin-owners/v1/";

/// The deferral every ownership refusal carries.
const OWNER_UNPROVEN: &str = "automation_pin_owner_unproven";

/// Store pages read while inventorying consumers.
const CONSUMER_PAGE_LIMIT: usize = 100;

/// Unreferenced pins deleted per `update-ref --stdin` transaction: about
/// 90 KiB of instructions, well inside one command's budget.
const RELEASE_CHUNK: usize = 500;

const HEX_DIGITS: &str = "0123456789abcdef";

/// Attempt ids are lowercase SHA-256 digests; nothing else is provably an
/// attempt pin.
fn is_attempt_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn unproven(detail: impl std::fmt::Display) -> AutomationError {
    AutomationError::Deferred(format!("{OWNER_UNPROVEN}: {detail}"))
}

/// Who may create, release and reclaim the pins of one namespace: the
/// identities whose stores hold every consumer and run that can name them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Owner {
    schema: u32,
    /// Canonical Orbit root; its host store holds the consumers and runs.
    root: String,
    /// Workspace partition of that store's consumers and runs.
    workspace: String,
    machine: String,
    /// Canonical Git common directory the refs are written to.
    git_common_dir: String,
}

impl Owner {
    /// This runtime's owner. Any identity that cannot be resolved refuses
    /// rather than falling back to a namespace another owner could share.
    pub(super) fn of(runtime: &OrbitRuntime, source: &Source) -> Result<Self, AutomationError> {
        let machine = runtime
            .automation_machine_identity()
            .ok_or_else(|| unproven("no registered machine identity"))?;
        let workspace = runtime.workspace_id().map_err(unproven)?;
        let root = runtime
            .global_root()
            .canonicalize()
            .map_err(|error| unproven(format!("the Orbit root does not resolve: {error}")))?;
        let common = source.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let common = Path::new(common.trim()).canonicalize().map_err(|error| {
            unproven(format!(
                "the Git common directory does not resolve: {error}"
            ))
        })?;
        Ok(Self {
            schema: 1,
            root: root.to_string_lossy().into_owned(),
            workspace,
            machine: machine.to_string(),
            git_common_dir: common.to_string_lossy().into_owned(),
        })
    }

    fn record(&self) -> Result<Vec<u8>, AutomationError> {
        serde_json::to_vec(self).map_err(|error| AutomationError::Evidence(error.to_string()))
    }

    fn digest(&self) -> Result<String, AutomationError> {
        Ok(digest(&self.record()?))
    }

    fn namespace(&self) -> Result<String, AutomationError> {
        Ok(format!("{OWNED_NAMESPACE}{}/", self.digest()?))
    }

    fn pin(&self, attempt_id: &str) -> Result<String, AutomationError> {
        Ok(format!("{}{attempt_id}", self.namespace()?))
    }

    fn record_ref(&self) -> Result<String, AutomationError> {
        Ok(format!("{OWNER_RECORDS}{}", self.digest()?))
    }

    /// Prefix of every consumer key this owner's evaluations write.
    fn consumers(&self) -> String {
        format!("{}/{}/", self.machine, self.workspace)
    }
}

/// What the owner record says about this owner's namespace.
enum Record {
    Missing,
    Matches,
    Conflicting(String),
}

fn record(source: &Source, owner: &Owner) -> Result<Record, AutomationError> {
    let refname = owner.record_ref()?;
    let Some(object) = exact(source, &refname)? else {
        return Ok(Record::Missing);
    };
    let recorded = match source.git(&["cat-file", "blob", &object]) {
        Ok(recorded) => recorded,
        Err(error) => {
            return Ok(Record::Conflicting(format!(
                "{refname} names no readable owner record: {error}"
            )));
        }
    };
    Ok(match serde_json::from_str::<Owner>(&recorded) {
        Ok(recorded) if recorded == *owner => Record::Matches,
        Ok(recorded) => Record::Conflicting(format!(
            "{refname} is bound to root {}, workspace {}, machine {}, Git directory {}",
            recorded.root, recorded.workspace, recorded.machine, recorded.git_common_dir
        )),
        Err(error) => {
            Record::Conflicting(format!("{refname} holds no readable owner record: {error}"))
        }
    })
}

/// Require a matching owner record, writing it the first time this owner
/// pins anything.
fn bind(source: &Source, owner: &Owner) -> Result<(), AutomationError> {
    match record(source, owner)? {
        Record::Matches => return Ok(()),
        Record::Conflicting(detail) => return Err(unproven(detail)),
        Record::Missing => {}
    }
    let blob = source.git_with_input(&["hash-object", "-w", "--stdin"], &owner.record()?)?;
    if create(source, &owner.record_ref()?, blob.trim()).is_ok() {
        return Ok(());
    }
    // Another admission by this owner may have recorded it first.
    match record(source, owner)? {
        Record::Matches => Ok(()),
        Record::Missing => Err(unproven("the owner record could not be written")),
        Record::Conflicting(detail) => Err(unproven(detail)),
    }
}

/// Require the owner record before deleting anything in its namespace.
fn proved(source: &Source, owner: &Owner) -> Result<(), AutomationError> {
    match record(source, owner)? {
        Record::Matches => Ok(()),
        Record::Missing => Err(unproven(format!(
            "{} has no owner record",
            owner.namespace()?
        ))),
        Record::Conflicting(detail) => Err(unproven(detail)),
    }
}

/// The object `refname` names, read by that one name: a literal pattern also
/// matches refs below `<refname>/`, but a ref sorts before its descendants,
/// so one row bounds the output however many there are.
fn exact(source: &Source, refname: &str) -> Result<Option<String>, AutomationError> {
    Ok(source
        .git(&[
            "for-each-ref",
            "--count=1",
            "--format=%(refname) %(objectname)",
            refname,
        ])?
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find(|(name, _)| *name == refname)
        .map(|(_, object)| object.to_string()))
}

/// Create `refname` only if it does not exist yet.
fn create(source: &Source, refname: &str, object: &str) -> Result<(), AutomationError> {
    source.git_with_input(
        &["update-ref", "--stdin"],
        format!("create {refname} {object}\n").as_bytes(),
    )?;
    Ok(())
}

/// Delete `refname` only while it still names `expected`.
fn delete(source: &Source, refname: &str, expected: &str) -> Result<(), AutomationError> {
    source.git(&["update-ref", "-d", refname, expected])?;
    Ok(())
}

fn moved(refname: &str, pinned: &str, commit: &str) -> AutomationError {
    AutomationError::Deferred(format!(
        "automation_pin_moved: {refname} names {pinned}, not the attempt's {commit}"
    ))
}

/// Pin the source `attempt` froze in `owner`'s namespace and return the ref.
/// An existing pin is kept only when it names the same commit, so an
/// idempotent retry succeeds and a ref someone repointed is never rebound.
pub(super) fn pin(
    source: &Source,
    owner: &Owner,
    attempt: &MemberAttempt,
) -> Result<String, AutomationError> {
    if !is_attempt_id(&attempt.id) {
        return Err(AutomationError::Evidence(format!(
            "attempt id {} is not a digest",
            attempt.id
        )));
    }
    bind(source, owner)?;
    let refname = owner.pin(&attempt.id)?;
    let commit = &attempt.member.source.commit;
    if exact(source, &refname)?.is_none() && create(source, &refname, commit).is_ok() {
        return Ok(refname);
    }
    match exact(source, &refname)? {
        Some(pinned) if pinned == *commit => Ok(refname),
        Some(pinned) => Err(moved(&refname, &pinned, commit)),
        None => Err(AutomationError::Deferred(format!(
            "automation_pin_unwritten: {refname}"
        ))),
    }
}

/// The commit `attempt_id` pinned: its owned pin, else the legacy shared
/// pin an attempt admitted before owner scoping took. Two exact reads,
/// whatever either namespace holds.
pub(super) fn pinned(
    source: &Source,
    owner: &Owner,
    attempt_id: &str,
) -> Result<Option<String>, AutomationError> {
    if !is_attempt_id(attempt_id) {
        return Ok(None);
    }
    if let Some(pinned) = exact(source, &owner.pin(attempt_id)?)? {
        return Ok(Some(pinned));
    }
    exact(source, &format!("{LEGACY_NAMESPACE}{attempt_id}"))
}

/// Release `attempt`'s owned pin once the owner record proves the namespace
/// is this owner's. Git deletes it only while it still names the commit the
/// attempt froze. A legacy shared pin is never deleted. Returns whether a pin
/// was deleted.
pub(super) fn release(
    source: &Source,
    owner: &Owner,
    attempt: &MemberAttempt,
) -> Result<bool, AutomationError> {
    if !is_attempt_id(&attempt.id) {
        return Ok(false);
    }
    let refname = owner.pin(&attempt.id)?;
    let Some(pinned) = exact(source, &refname)? else {
        return Ok(false);
    };
    let commit = &attempt.member.source.commit;
    if pinned != *commit {
        return Err(moved(&refname, &pinned, commit));
    }
    proved(source, owner)?;
    delete(source, &refname, &pinned)?;
    Ok(true)
}

/// Pin `attempt`'s frozen source in this runtime's own namespace, exactly as
/// admission does, and return the ref.
pub fn pin_attempt_source(
    runtime: &OrbitRuntime,
    attempt: &MemberAttempt,
) -> Result<String, OrbitError> {
    let source = Source::new(&runtime.paths().repo_root);
    Owner::of(runtime, &source)
        .and_then(|owner| pin(&source, &owner, attempt))
        .map_err(automation_error_to_orbit)
}

/// What `orbit doctor --fix-automation-pins` did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AttemptPinCleanup {
    /// The pin namespace this Orbit root and workspace own.
    pub namespace: String,
    /// Why nothing in the namespace was reclaimed: its owner record is
    /// missing or names another owner.
    pub refused: Option<String>,
    /// Owned pins nothing names; each was deleted.
    pub released: Vec<String>,
    /// Owned pins of attempts a consumer still has in flight or will retry.
    pub retained_active: usize,
    /// Owned pins an accepted assessment names, which an assessment certified
    /// under the `material_v1` contract needs to be carried forward.
    pub retained_assessed: usize,
    /// Owned pins of attempts a pending or running pipeline run still carries.
    pub retained_live_run: usize,
    /// Owned pins kept because ownership was refused.
    pub retained_unproven: usize,
    /// Owned pins that moved or could not be deleted; left in place.
    pub kept: Vec<String>,
    /// Legacy shared `refs/orbit/automation/<attempt id>` pins. No owner is
    /// recorded for them, and another root or an earlier client may still
    /// need one, so all are retained.
    pub retained_legacy: usize,
    /// Pin namespaces recorded for another Orbit root, workspace, machine or
    /// Git directory; never listed or touched.
    pub foreign_owners: usize,
    /// Refs that are not attempt pins in this namespace or at the legacy top
    /// level; nothing proves whose they are, so they are left untouched.
    pub unrecognized: Vec<String>,
}

/// Reclaim every owned attempt pin that no persisted consumer state and no
/// live run of this owner still names, and report what every other pin
/// namespace holds without touching it.
///
/// Only this owner's consumers and runs can name a pin in its namespace, and
/// all of them live in this runtime's stores. Pins are listed before the
/// inventory is read, and admission commits an attempt before pinning it, so
/// a listed pin whose attempt is still needed is always in the inventory.
/// Deletion is compare-and-delete at the listed commit, and the routine sweep
/// lock keeps this root's own evaluations out meanwhile. Any inventory
/// failure deletes nothing.
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
    let owner = Owner::of(runtime, &Source::new(root)).map_err(automation_error_to_orbit)?;
    let namespace = owner.namespace().map_err(automation_error_to_orbit)?;
    let mut cleanup = AttemptPinCleanup {
        namespace: namespace.clone(),
        ..AttemptPinCleanup::default()
    };

    for (refname, _) in top_level_refs(root, LEGACY_NAMESPACE)? {
        match refname.strip_prefix(LEGACY_NAMESPACE) {
            Some(id) if is_attempt_id(id) => cleanup.retained_legacy += 1,
            _ => cleanup.unrecognized.push(refname),
        }
    }
    let own_record = owner.record_ref().map_err(automation_error_to_orbit)?;
    cleanup.foreign_owners = Source::new(root)
        .git(&["for-each-ref", "--format=%(refname)", OWNER_RECORDS])
        .map_err(automation_error_to_orbit)?
        .lines()
        .filter(|refname| *refname != own_record)
        .count();

    let mut candidates = Vec::new();
    for (refname, object) in top_level_refs(root, &namespace)? {
        match refname.strip_prefix(namespace.as_str()) {
            Some(id) if is_attempt_id(id) => candidates.push((id.to_string(), refname, object)),
            _ => cleanup.unrecognized.push(refname),
        }
    }

    if let Err(error) = proved(&Source::new(root), &owner) {
        cleanup.refused = Some(error.to_string());
        cleanup.retained_unproven = candidates.len();
        return Ok(cleanup);
    }

    let inventory = Inventory::read(runtime, &owner)?;
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

/// Every ref directly under `namespace` with the object it names. Listed one
/// leading hex digit at a time so each listing stays far inside the source
/// budget; `*` does not cross `/`, so deeper refs such as delivery batch pins
/// are never listed.
fn top_level_refs(root: &Path, namespace: &str) -> Result<Vec<(String, String)>, OrbitError> {
    let mut refs = Vec::new();
    for digit in HEX_DIGITS.chars() {
        let listing = Source::new(root)
            .git(&[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                &format!("{namespace}{digit}*"),
            ])
            .map_err(automation_error_to_orbit)?;
        refs.extend(
            listing
                .lines()
                .filter_map(|line| line.split_once(' '))
                .map(|(name, object)| (name.to_string(), object.to_string())),
        );
    }
    Ok(refs)
}

/// Attempt ids this owner still names, by why they are needed.
#[derive(Default)]
struct Inventory {
    active: BTreeSet<String>,
    assessed: BTreeSet<String>,
    live_runs: BTreeSet<String>,
}

impl Inventory {
    /// Every consumer this owner's evaluations write, read in bounded pages to
    /// exhaustion, and every pending or running pilot run of its workspace.
    fn read(runtime: &OrbitRuntime, owner: &Owner) -> Result<Self, OrbitError> {
        let mut inventory = Self::default();
        let store = runtime.automation_store()?;
        let prefix = owner.consumers();
        let mut after: Option<String> = None;
        loop {
            let page =
                store.automation_states_page(&prefix, after.as_deref(), CONSUMER_PAGE_LIMIT)?;
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
