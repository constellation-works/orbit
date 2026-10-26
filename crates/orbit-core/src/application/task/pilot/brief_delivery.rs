//! Delivering a stored explore brief to an implement dispatch.
//!
//! The brief is checked in two steps, each failing closed to an undelivered
//! outcome. Material currency comes first: the brief must belong to the
//! task's newest pilot receipt, and the task's material, recomputed now at
//! the brief's `inspection_revision` under the pilot's policy, must still be
//! the material that receipt sealed. Code drift comes second: the paths the
//! brief covers are compared between `inspection_revision` and the run's
//! base. Every dispatch gets an outcome, `absent` included, for the audit
//! trail.

use std::collections::BTreeSet;
use std::path::Path;
use std::str::FromStr;

use orbit_common::fs::selector::Selector;
use orbit_common::process::shell::quote_posix_arg;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::EXPLORE_BRIEF_ARTIFACT;
use orbit_types::workflow::automation::members::{MemberAttempt, PreparationPolicy};
use orbit_types::workflow::{
    ExploreBriefAdmission, ExploreBriefDropReason, ExploreBriefOutcome, ExploreBriefRecord,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskAutomationPort;

use super::explore_brief::EXPLORE_BRIEF_SCHEMA;

/// Most covered files that may change before a brief is too stale to give.
const MAX_DRIFTED_FILES: usize = 3;
/// Most changed lines, added plus deleted, across those files.
const MAX_DRIFTED_LINES: usize = 200;

/// The stored document's fields admission reads.
#[derive(Deserialize)]
struct StoredBrief {
    schema: String,
    inspection_revision: String,
    material_fingerprint: String,
    receipt_operation_id: String,
    #[serde(default)]
    pilot_run_id: Option<String>,
    brief: Value,
}

impl OrbitRuntime {
    /// What an implement dispatch of `run_id` does with `task_id`'s explore
    /// brief. Never an error: anything unreadable is an undelivered outcome.
    pub(crate) fn explore_brief_for_dispatch(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> ExploreBriefAdmission {
        let settings = self.explore_brief_settings();
        let deliver = settings.enabled && settings.deliver;
        let (record, brief) = self.classify_explore_brief(run_id, task_id);
        ExploreBriefAdmission {
            brief: brief.filter(|_| deliver && record.deliverable()),
            record,
        }
    }

    fn classify_explore_brief(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> (ExploreBriefRecord, Option<Value>) {
        // A claimed leaf reads the owner's records only through its claim;
        // it gets no brief until the claim carries one.
        if self.worker_invocation().is_some() {
            return (
                ExploreBriefRecord::absent("a claimed run does not receive a brief"),
                None,
            );
        }
        let artifact = match self.get_task_artifact(task_id, EXPLORE_BRIEF_ARTIFACT) {
            Ok(Some(artifact)) => artifact,
            Ok(None) => return (ExploreBriefRecord::absent("no brief is stored"), None),
            Err(error) => {
                return (
                    ExploreBriefRecord::absent(format!("the brief cannot be read: {error}")),
                    None,
                );
            }
        };
        let sha256 = sha256_hex(&artifact.content);
        let stored = match serde_json::from_slice::<StoredBrief>(&artifact.content) {
            Ok(stored) if stored.schema == EXPLORE_BRIEF_SCHEMA => stored,
            _ => {
                let mut record = ExploreBriefRecord::absent("the stored brief is not readable");
                record.sha256 = Some(sha256);
                return (record, None);
            }
        };
        // Absent until the receipt shows the brief is current.
        let mut record = ExploreBriefRecord {
            outcome: ExploreBriefOutcome::Absent,
            reason: None,
            detail: None,
            sha256: Some(sha256),
            inspection_revision: Some(stored.inspection_revision.clone()),
            base_sha: None,
            drifted_paths: Vec::new(),
            delivered: false,
        };
        let dropped = |mut record: ExploreBriefRecord, reason, detail: String| {
            record.outcome = ExploreBriefOutcome::Dropped;
            record.reason = Some(reason);
            record.detail = Some(detail);
            (record, None)
        };

        let sealed = match self.sealed_brief_material(task_id, &stored) {
            Ok(sealed) => sealed,
            Err(detail) => {
                record.detail = Some(detail);
                return (record, None);
            }
        };
        if let Err(detail) = self.brief_material_current(task_id, &stored, sealed.as_deref()) {
            return dropped(record, ExploreBriefDropReason::MaterialChanged, detail);
        }

        let base = match self.run_base_revision(run_id) {
            Ok(base) => base,
            Err(error) => {
                return dropped(
                    record,
                    ExploreBriefDropReason::RevisionUnrelated,
                    format!("the run's base cannot be resolved: {error}"),
                );
            }
        };
        record.base_sha = Some(base.clone());
        let covered = Covered::of(&stored.brief);
        let drift = match code_drift(
            &self.paths().repo_root,
            &stored.inspection_revision,
            &base,
            &covered,
        ) {
            Ok(drift) => drift,
            Err(detail) => {
                return dropped(record, ExploreBriefDropReason::RevisionUnrelated, detail);
            }
        };
        record.drifted_paths = drift.files.iter().map(|file| file.path.clone()).collect();
        let lines = drift
            .files
            .iter()
            .map(|file| file.added + file.deleted)
            .sum::<usize>();
        if drift.files.is_empty() {
            record.outcome = ExploreBriefOutcome::Fresh;
        } else if drift.files.len() <= MAX_DRIFTED_FILES
            && lines <= MAX_DRIFTED_LINES
            && drift.removed_change_target.is_none()
        {
            record.outcome = ExploreBriefOutcome::Drifted;
        } else {
            let detail = match &drift.removed_change_target {
                Some(path) => format!("change target {path} was deleted or renamed"),
                None => format!(
                    "{} covered files and {lines} lines changed; the limits are \
                     {MAX_DRIFTED_FILES} files and {MAX_DRIFTED_LINES} lines",
                    drift.files.len()
                ),
            };
            return dropped(record, ExploreBriefDropReason::Stale, detail);
        }

        let mut delivered = json!({
            "status": record.label(),
            "inspection_revision": stored.inspection_revision,
            "base_sha": base,
            "brief": stored.brief,
        });
        if record.outcome == ExploreBriefOutcome::Drifted {
            delivered["drift"] = json!({
                "range": format!("{}..{base}", stored.inspection_revision),
                "diff_command": format!(
                    "git diff {} {base} -- {}",
                    stored.inspection_revision,
                    record
                        .drifted_paths
                        .iter()
                        .map(|path| quote_posix_arg(&format!(":(literal){path}")))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                "files": drift.files.iter().map(|file| json!({
                    "path": file.path,
                    "added": file.added,
                    "deleted": file.deleted,
                })).collect::<Vec<_>>(),
            });
        }
        (record, Some(delivered))
    }

    /// The status-neutral material the newest pilot receipt sealed, when the
    /// brief is that receipt's. A brief another receipt superseded, or one
    /// whose write outlived a failed receipt, is not current: `Err` says why.
    fn sealed_brief_material(
        &self,
        task_id: &str,
        stored: &StoredBrief,
    ) -> Result<Option<String>, String> {
        let comments = self
            .get_task_comments(task_id)
            .map_err(|error| format!("the task's comments cannot be read: {error}"))?;
        let Some((_, receipt)) = super::pilot_receipts(&comments)
            .find(|(_, receipt)| super::is_operation_id(receipt.operation_id))
        else {
            return Err("no pilot receipt names the brief".to_string());
        };
        if receipt.operation_id != stored.receipt_operation_id {
            return Err("a later pilot receipt superseded the brief".to_string());
        }
        let audit = serde_json::from_str::<Value>(receipt.body)
            .map_err(|_| "the pilot receipt is not readable".to_string())?;
        let material = &audit["applied_material"];
        if material["material"].get(0).and_then(Value::as_str)
            != Some(stored.material_fingerprint.as_str())
        {
            return Err("the brief's material is not the material its receipt sealed".to_string());
        }
        Ok(material["status_neutral_fingerprint"]
            .as_str()
            .map(str::to_string))
    }

    /// Whether the task's material, recomputed now at the brief's revision
    /// under the pilot's policy, is still what the receipt sealed.
    ///
    /// The comparison is status-neutral: the sealed fingerprint was taken
    /// while the task awaited admission, and admission itself moves its
    /// status (and may settle its dependencies), which is not a material
    /// edit. Every other material field must match. A fingerprint that cannot
    /// be read or recomputed fails closed.
    fn brief_material_current(
        &self,
        task_id: &str,
        stored: &StoredBrief,
        sealed: Option<&str>,
    ) -> Result<(), String> {
        let sealed = sealed.ok_or("the pilot receipt sealed no status-neutral material")?;
        let task = self
            .get_task(task_id)
            .map_err(|error| format!("the task cannot be read: {error}"))?;
        let policy = self.brief_pilot_policy(stored.pilot_run_id.as_deref());
        let current = self
            .task_pilot_fingerprints(&task, &stored.inspection_revision, &policy)
            .map_err(|error| format!("the task's material cannot be recomputed: {error}"))?;
        if current.status_neutral != sealed {
            return Err("the task's material changed after its pilot".to_string());
        }
        Ok(())
    }

    /// The freshness policy the pilot run that wrote the brief evaluated: its
    /// state claim's consumer policy, or the unrouted one. A policy that is
    /// not the pilot's yields a fingerprint that cannot match the sealed one,
    /// so an unreadable run fails closed rather than passing.
    fn brief_pilot_policy(&self, pilot_run_id: Option<&str>) -> PreparationPolicy {
        let claim = pilot_run_id
            .and_then(|run_id| self.show_job_run(run_id).ok())
            .and_then(|run| run.input?.get("state_automation").cloned())
            .and_then(|claim| serde_json::from_value::<MemberAttempt>(claim).ok());
        self.claim_preparation_policy(claim.as_ref())
            .or_else(|_| self.claim_preparation_policy(None))
            .unwrap_or_default()
    }
}

/// The paths a brief covers: every target's anchor file, and `dir:` targets
/// by prefix, the same semantics `source_sensitivity = "context_files"` uses.
#[derive(Default)]
struct Covered {
    files: BTreeSet<String>,
    dirs: BTreeSet<String>,
    /// Anchors of `change` targets, whose deletion drops the brief.
    change_files: BTreeSet<String>,
    change_dirs: BTreeSet<String>,
}

impl Covered {
    fn of(brief: &Value) -> Self {
        let mut covered = Self::default();
        for (field, key) in [
            ("change", "target"),
            ("call_path", "target"),
            ("pattern", "target"),
            ("tests", "target"),
            ("gotchas", "cite"),
        ] {
            for entry in brief[field].as_array().into_iter().flatten() {
                if let Some(target) = entry[key].as_str() {
                    covered.add(target, field == "change");
                }
            }
        }
        for map in brief["maps"].as_array().into_iter().flatten() {
            if let Some(target) = map.as_str() {
                covered.add(target, false);
            }
        }
        covered
    }

    fn add(&mut self, target: &str, change: bool) {
        let Ok(selector) = Selector::from_str(target) else {
            return;
        };
        let Some(path) = selector.anchor_path() else {
            return;
        };
        let path = path.trim_end_matches('/').to_string();
        let (all, changed) = match selector {
            Selector::Dir { .. } => (&mut self.dirs, &mut self.change_dirs),
            _ => (&mut self.files, &mut self.change_files),
        };
        if change {
            changed.insert(path.clone());
        }
        all.insert(path);
    }

    fn covers(&self, path: &str) -> bool {
        self.files.contains(path)
            || self
                .dirs
                .iter()
                .any(|dir| dir == "." || path == dir || path.starts_with(&format!("{dir}/")))
    }
}

/// A covered file that changed between the brief's revision and the base.
struct DriftedFile {
    path: String,
    added: usize,
    deleted: usize,
}

struct CodeDrift {
    files: Vec<DriftedFile>,
    /// A `change` target the base deleted or renamed away.
    removed_change_target: Option<String>,
}

/// Covered changes in `inspection..base`, or why that range cannot be read
/// (`inspection` is not an ancestor of `base`, or a Git read failed).
fn code_drift(
    root: &Path,
    inspection: &str,
    base: &str,
    covered: &Covered,
) -> Result<CodeDrift, String> {
    let repo = git2::Repository::open(root)
        .map_err(|error| format!("the repository cannot be opened: {}", error.message()))?;
    let commit = |revision: &str| {
        repo.revparse_single(revision)
            .and_then(|object| object.peel_to_commit())
            .map_err(|error| {
                format!(
                    "revision {revision} cannot be resolved: {}",
                    error.message()
                )
            })
    };
    let inspected = commit(inspection)?;
    let based = commit(base)?;
    if inspected.id() != based.id()
        && !repo
            .graph_descendant_of(based.id(), inspected.id())
            .map_err(|error| format!("ancestry cannot be read: {}", error.message()))?
    {
        return Err(format!(
            "the brief's revision {inspection} is not an ancestor of the base {base}"
        ));
    }
    let unreadable = |error: git2::Error| format!("the diff cannot be read: {}", error.message());
    let inspected_tree = inspected.tree().map_err(unreadable)?;
    let base_tree = based.tree().map_err(unreadable)?;
    let diff = repo
        .diff_tree_to_tree(Some(&inspected_tree), Some(&base_tree), None)
        .map_err(unreadable)?;
    let mut files = Vec::new();
    let mut removed_change_target = None;
    for (index, delta) in diff.deltas().enumerate() {
        let Some(path) = delta
            .old_file()
            .path()
            .or_else(|| delta.new_file().path())
            .and_then(Path::to_str)
        else {
            continue;
        };
        if !covered.covers(path) {
            continue;
        }
        if delta.status() == git2::Delta::Deleted && covered.change_files.contains(path) {
            removed_change_target.get_or_insert_with(|| path.to_string());
        }
        let (added, deleted) = match git2::Patch::from_diff(&diff, index).map_err(unreadable)? {
            Some(patch) => {
                let (_, added, deleted) = patch.line_stats().map_err(unreadable)?;
                (added, deleted)
            }
            None => (0, 0),
        };
        files.push(DriftedFile {
            path: path.to_string(),
            added,
            deleted,
        });
    }
    // A `dir:` change target counts as removed when it was a directory at
    // the brief's revision and is not one at the base. One the task creates
    // (absent at both) is not.
    let is_dir = |tree: &git2::Tree<'_>, dir: &str| {
        tree.get_path(Path::new(dir))
            .is_ok_and(|entry| entry.kind() == Some(git2::ObjectType::Tree))
    };
    if removed_change_target.is_none() {
        removed_change_target = covered
            .change_dirs
            .iter()
            .filter(|dir| dir.as_str() != ".")
            .find(|dir| is_dir(&inspected_tree, dir) && !is_dir(&base_tree, dir))
            .cloned();
    }
    Ok(CodeDrift {
        files,
        removed_change_target,
    })
}
