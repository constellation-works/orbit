//! Dependency-only aggregate failures: a job whose only failure evidence is
//! an `orbit-dependency-evidence` record naming a sibling job that failed.
//!
//! The producer (`scripts/ci-await-jobs.py`) prints exactly such a record
//! when a waiter job fails only because an awaited dependency failed. Filing
//! a separate task for the waiter duplicates the sibling's task, so a
//! failure is dependency-only here only under a strict proof: at least one
//! well-formed record, every record well-formed, at least one dependency
//! named with `conclusion: "failure"`, and no other diagnostic in the
//! remaining log. Anything else (own error anchor, timeout, script error,
//! malformed or empty record) is not dependency-only and files normally.

use std::collections::{BTreeMap, BTreeSet};

use orbit_engine::ci_log_signature::{LineKind, classify_log_lines, log_payload};
use orbit_tools::github_cli::strip_ansi_sequences;
use serde_json::Value;

use super::cluster::FailureCluster;
use super::evidence::selected_diagnostic;
use super::fields::value_string;
use super::log_signature::error_signature;

/// One failed dependency named by a dependency-only failure in one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DependencyRef {
    pub(super) run_id: String,
    pub(super) job_name: String,
    pub(super) job_id: Option<u64>,
}

/// A dependency identity as named in a record: name plus numeric id when present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DependencyIdentity {
    pub(super) job_name: String,
    pub(super) job_id: Option<u64>,
}

pub(super) fn normalized_job_name(name: &str) -> String {
    let mut out = String::new();
    let mut in_digits = false;
    for ch in name.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push_str("<n>");
                in_digits = true;
            }
        } else {
            in_digits = false;
            out.push(ch);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Failed dependencies for one failure, if it is dependency-only.
pub(super) fn dependency_only_failed_jobs(failure: &Value) -> Option<Vec<DependencyIdentity>> {
    let raw = selected_diagnostic(failure)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value_string(failure, "log_excerpt"));
    let text = strip_ansi_sequences(&raw);
    if text.trim().is_empty() {
        return None;
    }
    let mut failed: Vec<DependencyIdentity> = Vec::new();
    let mut saw_record = false;
    let mut remaining: Vec<String> = Vec::new();
    for (kind, line) in classify_log_lines(&text) {
        if matches!(
            kind,
            LineKind::RunCommand | LineKind::CommandBody | LineKind::ParamDump | LineKind::EndGroup
        ) {
            continue;
        }
        let payload = strip_ansi_sequences(log_payload(line));
        let Some((_, record_text)) = payload.split_once("orbit-dependency-evidence:") else {
            remaining.push(payload);
            continue;
        };
        // A line carrying the marker is the record line, wherever runner
        // columns placed it; a record embedded in other output is not the
        // waiter's emitted evidence.
        if !payload
            .trim_start()
            .starts_with("orbit-dependency-evidence:")
        {
            return None;
        }
        saw_record = true;
        let record: Value = serde_json::from_str(record_text.trim()).ok()?;
        let dependencies = record.get("dependencies").and_then(Value::as_array)?;
        if dependencies.is_empty() {
            return None;
        }
        for dependency in dependencies {
            let job_name = dependency.get("job_name").and_then(Value::as_str)?.trim();
            let conclusion = dependency.get("conclusion").and_then(Value::as_str)?;
            if job_name.is_empty() {
                return None;
            }
            if conclusion == "failure" {
                let identity = DependencyIdentity {
                    job_name: job_name.to_string(),
                    job_id: dependency.get("job_id").and_then(Value::as_u64),
                };
                if !failed.contains(&identity) {
                    failed.push(identity);
                }
            }
        }
    }
    if !saw_record || failed.is_empty() {
        return None;
    }
    let failed_names: Vec<String> = failed.iter().map(|dep| dep.job_name.clone()).collect();
    // The waiter also prints a `ci-await:` line reporting the named
    // dependency's conclusion. That line is part of the dependency
    // evidence, not an independent failure; every other `ci-await:` line
    // (timeout, listing failure) remains and blocks folding.
    let remaining: Vec<String> = remaining
        .into_iter()
        .filter(|line| !is_dependency_conclusion_line(line, &failed_names))
        .collect();
    let step = failing_step(failure);
    let signature = error_signature(&remaining.join("\n"), &step);
    if !signature.step_fallback {
        return None;
    }
    Some(failed)
}

fn is_dependency_conclusion_line(line: &str, failed_names: &[String]) -> bool {
    let lowered = line.trim().to_ascii_lowercase();
    let Some(rest) = lowered.strip_prefix("ci-await:") else {
        return false;
    };
    let rest = rest.trim();
    failed_names.iter().any(|name| {
        rest.strip_prefix(&name.to_ascii_lowercase())
            .is_some_and(|tail| tail.contains(" concluded "))
    })
}

fn failing_step(failure: &Value) -> String {
    failure
        .get("failed_jobs")
        .and_then(Value::as_array)
        .and_then(|jobs| jobs.first())
        .and_then(|job| job.get("failed_steps"))
        .and_then(Value::as_array)
        .and_then(|steps| steps.first())
        .map(|step| value_string(step, "name"))
        .unwrap_or_default()
}

/// Dependency refs for a whole cluster, if every run in it is
/// dependency-only. A cluster mixing a dependency-only run with an own
/// failure is not folded.
pub(super) fn cluster_dependency_refs(cluster: &FailureCluster) -> Option<Vec<DependencyRef>> {
    if cluster.runs.is_empty() {
        return None;
    }
    let mut refs = Vec::new();
    for run in &cluster.runs {
        let deps = dependency_only_failed_jobs(run)?;
        let run_id = value_string(run, "run_id");
        if run_id.is_empty() {
            return None;
        }
        for dep in deps {
            let entry = DependencyRef {
                run_id: run_id.clone(),
                job_name: dep.job_name,
                job_id: dep.job_id,
            };
            if !refs.contains(&entry) {
                refs.push(entry);
            }
        }
    }
    (!refs.is_empty()).then_some(refs)
}

/// Map `(run_id, kind:key)` to the cluster exhibiting that failure, where
/// kind is `name`, `norm` (lowercased, digits collapsed) or `id`.
pub(super) fn sibling_cluster_index(
    clusters: &[FailureCluster],
) -> BTreeMap<(String, String), usize> {
    let mut index = BTreeMap::new();
    for (position, cluster) in clusters.iter().enumerate() {
        for run in &cluster.runs {
            let run_id = value_string(run, "run_id");
            if run_id.is_empty() {
                continue;
            }
            let mut names: Vec<String> = Vec::new();
            let mut ids: Vec<u64> = Vec::new();
            if let Some(job_id) = run.get("job_id").and_then(Value::as_u64) {
                ids.push(job_id);
            }
            if let Some(job) = run
                .get("failed_jobs")
                .and_then(Value::as_array)
                .and_then(|jobs| jobs.first())
            {
                let name = value_string(job, "name");
                if !name.is_empty() && !names.contains(&name) {
                    names.push(name);
                }
                if let Some(job_id) = job.get("job_id").and_then(Value::as_u64)
                    && !ids.contains(&job_id)
                {
                    ids.push(job_id);
                }
            }
            for name in names {
                index
                    .entry((run_id.clone(), format!("name:{name}")))
                    .or_insert(position);
                index
                    .entry((
                        run_id.clone(),
                        format!("lower:{}", name.to_ascii_lowercase()),
                    ))
                    .or_insert(position);
                index
                    .entry((
                        run_id.clone(),
                        format!("norm:{}", normalized_job_name(&name)),
                    ))
                    .or_insert(position);
            }
            for id in ids {
                index
                    .entry((run_id.clone(), format!("id:{id}")))
                    .or_insert(position);
            }
        }
    }
    index
}

/// Resolve a dependency reference to its sibling cluster, by id, then by
/// exact name, then by normalized name (the signature form).
pub(super) fn sibling_cluster_for(
    index: &BTreeMap<(String, String), usize>,
    reference: &DependencyRef,
) -> Option<usize> {
    let by_name = index
        .get(&(
            reference.run_id.clone(),
            format!("name:{}", reference.job_name),
        ))
        .copied()
        .or_else(|| {
            index
                .get(&(
                    reference.run_id.clone(),
                    format!("lower:{}", reference.job_name.to_ascii_lowercase()),
                ))
                .copied()
        })
        .or_else(|| {
            // Only a record carrying the signature placeholder (`<n>`) may
            // match across differing partition numbers; a concrete number
            // must match exactly.
            reference.job_name.contains("<n>").then(|| {
                index
                    .get(&(
                        reference.run_id.clone(),
                        format!("norm:{}", normalized_job_name(&reference.job_name)),
                    ))
                    .copied()
            })?
        });
    if let Some(job_id) = reference.job_id {
        let by_id = index
            .get(&(reference.run_id.clone(), format!("id:{job_id}")))
            .copied();
        // A record naming an id and a name must name the same sibling.
        return match (by_name, by_id) {
            (Some(name_target), Some(id_target)) if name_target == id_target => Some(name_target),
            (Some(_), Some(_)) => None,
            _ => None,
        };
    }
    by_name
}

/// Decide which clusters fold into a sibling. A candidate folds only when
/// every failed dependency it names maps to a different cluster in the same
/// run, and that sibling is covered by an existing task or will be filed
/// within the cap (folded clusters consume no cap slot). Siblings that are
/// withheld, pending supersession, or over the cap are not effective, so
/// their aggregates stay filed. Chains of dependency-only jobs resolve
/// transitively through other folded clusters.
pub(super) fn folded_cluster_indices(
    clusters: &[FailureCluster],
    refs_by_cluster: &[Option<Vec<DependencyRef>>],
    covered: &[bool],
    blocked: &[bool],
    max_tasks: usize,
) -> BTreeSet<usize> {
    let siblings = sibling_cluster_index(clusters);
    let mut candidates: BTreeSet<usize> = BTreeSet::new();
    for (position, refs) in refs_by_cluster.iter().enumerate() {
        if covered[position] || blocked[position] {
            continue;
        }
        let Some(refs) = refs else { continue };
        let mut targets = Vec::new();
        let mut all_mapped = true;
        for reference in refs {
            match sibling_cluster_for(&siblings, reference) {
                Some(target) if target != position => targets.push(target),
                _ => {
                    all_mapped = false;
                    break;
                }
            }
        }
        if all_mapped && !targets.is_empty() {
            candidates.insert(position);
        }
    }
    loop {
        // Cap allocation over the clusters that would actually file,
        // in cluster order, skipping currently assumed folds.
        let mut filed_count = 0usize;
        let mut filed_keys: BTreeSet<String> = BTreeSet::new();
        let mut effective: BTreeSet<usize> = BTreeSet::new();
        for (position, cluster) in clusters.iter().enumerate() {
            if candidates.contains(&position) {
                continue;
            }
            if covered[position] {
                effective.insert(position);
                continue;
            }
            if blocked[position] {
                continue;
            }
            if filed_keys.contains(&cluster.failure_key) {
                effective.insert(position);
                continue;
            }
            if filed_count >= max_tasks {
                continue;
            }
            filed_count += 1;
            filed_keys.insert(cluster.failure_key.clone());
            effective.insert(position);
        }
        // Folded clusters are effective for their dependents only when
        // their own dependencies are effective (transitively).
        let mut folded_effective: BTreeSet<usize> = BTreeSet::new();
        loop {
            let mut progressed = false;
            for &position in &candidates {
                if folded_effective.contains(&position) {
                    continue;
                }
                let Some(refs) = &refs_by_cluster[position] else {
                    continue;
                };
                let ok = refs.iter().all(|reference| {
                    sibling_cluster_for(&siblings, reference).is_some_and(|target| {
                        effective.contains(&target) || folded_effective.contains(&target)
                    })
                });
                if ok {
                    folded_effective.insert(position);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        if folded_effective == candidates {
            return candidates;
        }
        // Drop candidates whose siblings are not effective and retry:
        // they now consume cap slots, which may change the rest.
        let before = candidates.len();
        candidates.retain(|position| folded_effective.contains(position));
        if candidates.len() == before {
            return candidates;
        }
    }
}

/// The non-folded clusters a folded cluster's dependency chain ends in,
/// following edges through other folded clusters. For A -> B -> C with B
/// and A folded, this is `[C]`: A's record names only B, but B folds into C,
/// so C's task is the one that covers A. Empty for a cluster that is not
/// folded.
pub(super) fn fold_owner_indices(
    siblings: &BTreeMap<(String, String), usize>,
    refs_by_cluster: &[Option<Vec<DependencyRef>>],
    folded: &BTreeSet<usize>,
    position: usize,
) -> Vec<usize> {
    let mut owners = Vec::new();
    let mut visited = BTreeSet::from([position]);
    for reference in refs_by_cluster[position].iter().flatten() {
        if let Some(target) = sibling_cluster_for(siblings, reference) {
            collect_fold_owners(
                siblings,
                refs_by_cluster,
                folded,
                target,
                &mut visited,
                &mut owners,
            );
        }
    }
    owners
}

fn collect_fold_owners(
    siblings: &BTreeMap<(String, String), usize>,
    refs_by_cluster: &[Option<Vec<DependencyRef>>],
    folded: &BTreeSet<usize>,
    target: usize,
    visited: &mut BTreeSet<usize>,
    owners: &mut Vec<usize>,
) {
    if !folded.contains(&target) {
        if !owners.contains(&target) {
            owners.push(target);
        }
        return;
    }
    if !visited.insert(target) {
        return;
    }
    for reference in refs_by_cluster[target].iter().flatten() {
        if let Some(next) = sibling_cluster_for(siblings, reference) {
            collect_fold_owners(siblings, refs_by_cluster, folded, next, visited, owners);
        }
    }
}
