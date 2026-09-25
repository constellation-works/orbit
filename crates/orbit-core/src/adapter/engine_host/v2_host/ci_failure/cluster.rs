//! One root cause and the task description rendered from its runs.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::fields::{truncate_bytes, value_string};
use super::filing::{
    CI_FAILURE_KEY_TAG_PREFIX, CI_FAILURE_SWEEP_TITLE_PREFIX, DESCRIPTION_LOG_BYTES,
    MAX_LISTED_RUNS,
};
use super::grouping::{
    failure_test_names, legacy_source_matches, source_identity_fingerprint, tested_commit,
};
use super::log_signature::{
    FailedStepExcerpt, compiler_cause, relevant_log_query_errors, render_failed_step_excerpt,
    specific_command_from_log, specific_error_anchors,
};
use crate::adapter::engine_host::v2_host::admission::duplicate_tasks::{
    CoverageAnchor, CoverageFingerprint, DuplicateCandidate, DuplicateTaskLookup,
    DuplicateTaskMatch, find_covering_task,
};
use crate::adapter::engine_host::v2_host::admission::sweep_filing::{
    digest, display, truncate_chars,
};

/// One root cause, with every current run that exhibited it.
pub(super) struct FailureCluster {
    /// Dedupe identity across sweeps. Compiler proof replaces the ordinary
    /// workflow/job/step/signature key only at the same observed checkout.
    pub(super) failure_key: String,
    /// Grouping identity within one snapshot: `failure_key` plus the commit the
    /// runner actually tested.
    pub(super) cluster_key: String,
    pub(super) workflow: String,
    pub(super) job: String,
    pub(super) jobs: BTreeSet<String>,
    pub(super) step: String,
    pub(super) tested_commit: String,
    pub(super) signature: String,
    pub(super) compiler_cause: Option<String>,
    pub(super) legacy_keys: BTreeSet<String>,
    /// True when `signature` is the failing step name because no error line
    /// survived in the excerpt. The description must label that as a fallback
    /// rather than a captured diagnostic; collapsing every distinct failure of
    /// the step into one `failure_key` is the weaker identity, not a quote.
    pub(super) signature_is_step_fallback: bool,
    pub(super) log_excerpt: String,
    pub(super) failure_region_note: Option<String>,
    pub(super) log_truncated: bool,
    /// The job whose own log supplied the excerpt, when the run-scoped read
    /// returned nothing and collection recovered it per job. Such an excerpt is
    /// the whole job's log, not just its failed steps, and the description says
    /// so rather than presenting it as a failed-step quote.
    pub(super) log_source_job: Option<String>,
    pub(super) runs: Vec<Value>,
}

impl FailureCluster {
    pub(super) fn find_covering_task<L: DuplicateTaskLookup + ?Sized>(
        &self,
        lookup: &L,
    ) -> Result<Option<DuplicateTaskMatch>, OrbitError> {
        if let Some(found) = find_covering_task(lookup, &self.duplicate_candidate())? {
            return Ok(Some(found));
        }
        // Shipped per-job keys (including the old first-marker signature) are
        // durable references. Keep their exact/rejected-owner continuity, but
        // never use their weak command or source-only fingerprints as proof.
        for key in &self.legacy_keys {
            let tag = format!("{CI_FAILURE_KEY_TAG_PREFIX}{key}");
            let candidate = DuplicateCandidate::new(
                tag.clone(),
                vec![CoverageFingerprint::new(
                    "legacy_compiler_key",
                    vec![CoverageAnchor::new("exact_failure_key", tag)],
                )],
            );
            if let Some(found) = find_covering_task(lookup, &candidate)? {
                let source_id = found.evidence["matched_fields"]
                    .as_array()
                    .and_then(|fields| {
                        fields
                            .iter()
                            .find(|field| field["field"] == "rejected_task_id")
                    })
                    .and_then(|field| field["value"].as_str())
                    .unwrap_or(&found.task_id);
                let source = lookup.get_task(source_id)?;
                let owner = lookup.get_task(&found.task_id)?;
                let prior_cause = compiler_cause(&source.description)
                    .or_else(|| compiler_cause(&owner.description));
                if prior_cause.is_some() && prior_cause != self.compiler_cause {
                    continue;
                }
                // An old chatter-based key can recur for a different compiler
                // cause. Only immutable supplying evidence justifies migration.
                if self
                    .runs
                    .iter()
                    .any(|run| legacy_source_matches(&source.description, run))
                {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }

    fn duplicate_candidate(&self) -> DuplicateCandidate {
        let exact_tag = format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", self.failure_key);
        if let Some(cause) = &self.compiler_cause {
            let mut fingerprints = vec![CoverageFingerprint::new(
                "ci_compiler_cause",
                vec![
                    CoverageAnchor::new("compiler_cause", digest(&[cause])),
                    CoverageAnchor::new("tested_commit", &self.tested_commit),
                ],
            )];
            fingerprints.extend(self.provenance_fingerprints());
            return DuplicateCandidate::new(exact_tag, fingerprints)
                .with_completed_fingerprints(self.completed_provenance_fingerprints());
        }
        let mut fingerprints = if self.signature_is_step_fallback {
            // A step-name fallback contains no diagnostic. It is sufficient
            // for exact-key idempotency but too weak for broader free-text
            // coverage, where it could suppress an unrelated failure of the
            // same generic CI step.
            vec![CoverageFingerprint::new(
                "ci_failure_unmatchable_fallback",
                vec![CoverageAnchor::new("exact_failure_key", &exact_tag)],
            )]
        } else {
            vec![CoverageFingerprint::new(
                "ci_failure_root_cause",
                vec![
                    CoverageAnchor::new("workflow", format!("workflow {}", self.workflow)),
                    CoverageAnchor::new("job", format!("failing job {}", self.job)),
                    CoverageAnchor::new("step", format!("failing step {}", self.step)),
                    CoverageAnchor::new("normalized_error_signature", &self.signature),
                ],
            )]
        };
        if !self.signature_is_step_fallback {
            if let Some(command) = specific_command_from_log(&self.log_excerpt) {
                for diagnostic in specific_error_anchors(&self.log_excerpt, &self.signature)
                    .into_iter()
                    .take(3)
                {
                    fingerprints.push(CoverageFingerprint::new(
                        "ci_failure_error_and_command",
                        vec![
                            CoverageAnchor::new("specific_error", diagnostic),
                            CoverageAnchor::new("command", command.clone()),
                        ],
                    ));
                }
            }
            if let Some(fingerprint) = source_identity_fingerprint(&self.runs) {
                fingerprints.push(fingerprint);
            }
        }
        fingerprints.extend(self.provenance_fingerprints());
        DuplicateCandidate::new(exact_tag, fingerprints)
            .with_completed_fingerprints(self.completed_provenance_fingerprints())
    }

    fn provenance_fingerprints(&self) -> Vec<CoverageFingerprint> {
        self.provenance_fingerprints_with_test_names(true)
    }

    fn completed_provenance_fingerprints(&self) -> Vec<CoverageFingerprint> {
        self.provenance_fingerprints_with_test_names(false)
    }

    fn provenance_fingerprints_with_test_names(
        &self,
        include_test_names: bool,
    ) -> Vec<CoverageFingerprint> {
        let mut fingerprints = Vec::new();
        let mut seen = BTreeSet::new();
        for run in &self.runs {
            let run_id = value_string(run, "run_id");
            if !run_id.is_empty() && seen.insert(("run_id", run_id.clone())) {
                fingerprints.push(CoverageFingerprint::new(
                    "ci_failure_run_id",
                    vec![CoverageAnchor::new("run_id", run_id)],
                ));
            }
            // A bare SHA identifies a commit, not a failure: any open task
            // that quotes the current branch head (a code-scanning sweep, a
            // dependabot sweep, a review brief) would otherwise suppress every
            // CI failure on that head until it closes. Pairing the SHA with
            // the generated workflow / failing-job labels keeps the match to
            // tasks that actually describe this failure on this commit.
            for sha in [
                value_string(run, "event_reported_head_sha"),
                value_string(run, "current_ref_head_sha"),
                tested_commit(run),
            ] {
                if !sha.is_empty() && seen.insert(("head_sha", sha.clone())) {
                    fingerprints.push(CoverageFingerprint::new(
                        "ci_failure_head_sha",
                        vec![
                            CoverageAnchor::new("head_sha", sha),
                            CoverageAnchor::new("workflow", format!("workflow {}", self.workflow)),
                            CoverageAnchor::new("job", format!("failing job {}", self.job)),
                        ],
                    ));
                }
            }
            if include_test_names {
                for name in failure_test_names(run) {
                    if seen.insert(("test_name", name.clone())) {
                        fingerprints.push(CoverageFingerprint::new(
                            "ci_failure_test_name",
                            vec![CoverageAnchor::new("test_name", name)],
                        ));
                    }
                }
            }
        }
        fingerprints
    }

    pub(super) fn run_urls(&self) -> Vec<String> {
        self.runs
            .iter()
            .filter_map(|run| run.get("url").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect()
    }

    pub(super) fn run_ids(&self) -> Vec<Value> {
        self.runs
            .iter()
            .filter_map(|run| run.get("run_id").cloned())
            .collect()
    }

    pub(super) fn filing_entry(&self, task_id: &str) -> Value {
        json!({
            "task_id": task_id,
            "failure_key": self.failure_key,
            "cluster_key": self.cluster_key,
            "workflow": self.workflow,
            "job": self.job,
            "step": self.step,
            "jobs": self.jobs,
            "tested_commit": self.tested_commit,
            "sources": self.runs.iter().map(|run| json!({
                "run_id": run["run_id"], "job_id": run["job_id"],
                "workflow": run["workflow"], "failed_jobs": run["failed_jobs"],
                "actual_checkout_shas": run["actual_checkout_shas"],
            })).collect::<Vec<_>>(),
            "run_ids": self.run_ids(),
            "run_urls": self.run_urls(),
            "ref_kinds": self.distinct_run_strings("ref_kind"),
            "head_branches": self.distinct_run_strings("head_branch"),
        })
    }

    fn distinct_run_strings(&self, field: &str) -> Vec<String> {
        self.runs
            .iter()
            .filter_map(|run| run.get(field).and_then(Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(super) fn title(&self) -> String {
        let where_ = match (self.job.as_str(), self.step.as_str()) {
            ("", "") => self.workflow.clone(),
            (job, "") => format!("{} / {job}", self.workflow),
            ("", step) => format!("{} / {step}", self.workflow),
            (job, step) => format!("{} / {job} / {step}", self.workflow),
        };
        // Cap the body, not the whole string, so a long workflow/job/step
        // never eats into the prefix and the final title still respects the
        // existing 120-character bound.
        let body_budget = 120usize.saturating_sub(CI_FAILURE_SWEEP_TITLE_PREFIX.chars().count());
        let body = truncate_chars(&format!("Fix red CI: {where_}"), body_budget);
        format!("{CI_FAILURE_SWEEP_TITLE_PREFIX}{body}")
    }

    pub(super) fn acceptance_criteria(&self) -> Vec<String> {
        vec![
            format!(
                "The root cause of the `{}` failure recorded below is fixed in this repository; \
                 the workflow, assertion, or lint level is not disabled, weakened, or made \
                 non-blocking to obtain green.",
                self.workflow
            ),
            "The exact command or narrowest faithful local equivalent that failed on the runner \
             is reproduced and then passes locally."
                .to_string(),
            "The repository's documented pre-handoff gate passes.".to_string(),
            "This task carries a `regression_from` relation targeting the task whose landed \
             commit introduced the failure, or its execution summary states why no task can \
             be held responsible (no task ID on the culprit commit, or the failure is \
             infrastructure rather than repository-owned)."
                .to_string(),
            "If the failure turns out to be infrastructure rather than repository-owned, the \
             execution summary cites concrete evidence for that (a same-commit retry that \
             succeeded, or runner/service fault output) rather than a single non-reproduction."
                .to_string(),
        ]
    }

    /// Render the evidence a remediation agent needs, inline.
    ///
    /// This is the whole point of the sweep: the agent that picks this task up
    /// cannot reach GitHub, so anything absent here is unavailable to it.
    pub(super) fn description(&self, evidence: &Value) -> String {
        let mut out = String::new();
        out.push_str(
            "This task was filed automatically from a host-side sweep of this repository's \
             GitHub Actions runs. Every CI query ran on the host before this task existed, so \
             the evidence below is all of it — the execution lane for this task cannot reach \
             GitHub, and it is not expected to.\n\n",
        );

        out.push_str("## Failure\n\n");
        out.push_str(&format!("- Workflow: `{}`\n", display(&self.workflow)));
        out.push_str(&format!("- Failing job: `{}`\n", display(&self.job)));
        if self.jobs.len() > 1 {
            let additional = self
                .jobs
                .iter()
                .filter(|job| *job != &self.job)
                .map(|job| format!("`{}`", display(job)))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "- Other failing jobs in this test cluster: {additional}\n"
            ));
        }
        out.push_str(&format!("- Failing step: `{}`\n", display(&self.step)));
        out.push_str(&format!(
            "- Commit the runner actually checked out: `{}`\n",
            display(&self.tested_commit)
        ));
        if let Some(cause) = &self.compiler_cause {
            out.push_str(&format!(
                "- Compiler cause identity: `{}`\n",
                digest(&[cause])
            ));
        }
        if self.signature_is_step_fallback {
            out.push_str(&format!(
                "- Normalized error signature (step-name fallback — no error line was captured; \
                 the dedupe identity, not a quote): `{}`\n",
                display(&self.signature)
            ));
        } else if self.compiler_cause.is_some() {
            out.push_str(&format!(
                "- Normalized error signature (display only; the compiler cause identity controls dedupe): `{}`\n",
                display(&self.signature)
            ));
        } else {
            out.push_str(&format!(
                "- Normalized error signature (the dedupe identity, not a quote): `{}`\n",
                display(&self.signature)
            ));
        }
        if let Some(repository) = evidence.get("repository").and_then(Value::as_object) {
            if let Some(full_name) = repository.get("full_name").and_then(Value::as_str) {
                out.push_str(&format!("- Repository: `{full_name}`\n"));
            }
            if let Some(default_branch) = repository.get("default_branch").and_then(Value::as_str) {
                out.push_str(&format!(
                    "- Release branch as GitHub reports it: `{default_branch}`\n"
                ));
            }
        }
        if let Some(collected_at) = evidence.get("collected_at").and_then(Value::as_str) {
            out.push_str(&format!("- Evidence collected at: {collected_at}\n"));
        }

        out.push_str("\n## Runs exhibiting this failure\n\n");
        out.push_str(
            "Three commits are routinely conflated and are kept apart here: the SHA the \
             workflow event reported, the SHA the ref points at now, and the commit the runner \
             actually checked out. A pull-request merge SHA is not a pull-request head SHA.\n\n",
        );
        for run in self.runs.iter().take(MAX_LISTED_RUNS) {
            out.push_str(&render_run(run));
        }
        if self.runs.len() > MAX_LISTED_RUNS {
            out.push_str(&format!(
                "\n_{} further run(s) in this cluster are not listed._\n",
                self.runs.len() - MAX_LISTED_RUNS
            ));
        }

        out.push_str("\n## Failed-step log excerpt\n\n");
        if self.log_excerpt.trim().is_empty() {
            out.push_str(
                "_No log excerpt was captured for this cluster. Reproduce the failing job's \
                 command locally instead of guessing from the step name._\n",
            );
            let relevant = relevant_log_query_errors(evidence, &self.runs);
            if !relevant.is_empty() {
                out.push('\n');
                for error in relevant {
                    out.push_str(&format!(
                        "- `{}` for run `{}`: {}\n",
                        display(&value_string(error, "query")),
                        display(&value_string(error, "run_id")),
                        truncate_chars(&value_string(error, "error"), 300),
                    ));
                }
            }
        } else {
            let excerpt = if let Some(note) = &self.failure_region_note {
                out.push_str(note);
                // Already capped at 64 KiB by collection and checked again at
                // filing. Keep every selected failure for the offline worker.
                FailedStepExcerpt {
                    body: self.log_excerpt.clone(),
                    has_anchor: true,
                }
            } else {
                render_failed_step_excerpt(&self.log_excerpt, DESCRIPTION_LOG_BYTES)
            };
            if !excerpt.body.trim().is_empty() {
                out.push_str("```\n");
                out.push_str(&excerpt.body);
                out.push_str("\n```\n");
            }
            if !excerpt.has_anchor {
                out.push_str(
                    "\n_No error anchor was present in the retained excerpt; the env/with dump \
                     is omitted rather than shown as evidence._\n",
                );
            }
            if self.log_truncated {
                out.push_str(
                    "\n_The collection display was truncated. Selected evidence is used for diagnosis; \
                     its retention limits are independent of that display._\n",
                );
            }
            if let Some(job) = &self.log_source_job {
                out.push_str(&format!(
                    "\n_The run-scoped failed-step log came back empty, so this excerpt is the \
                     evidence from job {job}, read from the job log API._\n"
                ));
            }
        }

        let stale = self.stale_evidence(evidence);
        if !stale.is_empty() {
            out.push_str("\n## Stale or superseded runs of this workflow\n\n");
            out.push_str(
                "These are already excluded from the failure above. They are listed so the \
                 repair is not attributed to a run that no longer reflects the current head.\n\n",
            );
            for entry in &stale {
                out.push_str(&format!(
                    "- `{}` run {} — {}: {}\n",
                    display(&value_string(entry, "workflow")),
                    display(&value_string(entry, "url")),
                    display(&value_string(entry, "reason")),
                    display(&value_string(entry, "evidence")),
                ));
            }
        }

        if let Some(truncation) = evidence.get("truncation") {
            out.push_str("\n## Collection bounds\n\n");
            out.push_str(
                "Reported so \"no more failures\" is never mistaken for \"we stopped \
                 looking\".\n\n",
            );
            out.push_str("```json\n");
            out.push_str(&truncate_bytes(
                &serde_json::to_string_pretty(truncation).unwrap_or_default(),
                2_000,
            ));
            out.push_str("\n```\n");
        }

        let query_errors = evidence
            .get("query_errors")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if !query_errors.is_empty() {
            out.push_str("\n## Queries that failed during collection\n\n");
            for error in query_errors {
                out.push_str(&format!("- {}\n", truncate_chars(&error.to_string(), 300)));
            }
        }

        out.push_str(
            "\n## How to finish\n\n\
             Reproduce the failure at the current repository head using the exact command or \
             the narrowest faithful local equivalent, fix the repository-owned cause, and rerun \
             that command. Do not disable a workflow, weaken an assertion or lint level, add a \
             broad allow rule, or mark a failing gate non-blocking. Then run the repository's \
             documented pre-handoff gate. Verification happens on this task's own pull request: \
             CI runs there normally, and if the failure is still current the next sweep will see \
             it again.\n\
             \n\
             ## Attribute the regression\n\
             \n\
             Identify the task whose landed change introduced this failure and record it on \
             this task. Start from the commit the runner checked out and walk back to the \
             newest commit at which the failing command last passed; the commit that broke it \
             is the culprit. Read the task ID from that commit — squash-merge subjects carry \
             it as `[<task-id>]` — and confirm with `orbit tool run orbit.task.show --input \
             '{\"id\":\"<task-id>\"}'` that its change is the one at fault, not a later commit \
             that merely touched the same file. Then set the relation with `orbit tool run \
             orbit.task.update --input '{\"id\":\"<this task ID>\",\"relations\":[<existing \
             relations from orbit.task.show>,{\"type\":\"regression_from\",\"target\":\"<culprit \
             task ID>\"}]}'` — `relations` replaces the whole list, so carry the existing \
             entries forward. When the culprit commit carries no task ID, or the failure is \
             infrastructure rather than repository-owned, record that conclusion and its \
             evidence in the execution summary instead of blaming a bystander task.\n",
        );
        out
    }

    /// Stale/superseded runs of the same workflow, so the description can say
    /// why they were excluded rather than leaving them unexplained.
    fn stale_evidence(&self, evidence: &Value) -> Vec<Value> {
        evidence
            .get("stale_or_superseded")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| value_string(entry, "workflow") == self.workflow)
                    .take(MAX_LISTED_RUNS)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The job whose own log supplied this failure's excerpt, if the run-scoped
/// read produced nothing and collection fell back per job.
pub(super) fn job_log_source(failure: &Value) -> Option<String> {
    if value_string(failure, "log_source") != "job_api_log" {
        return None;
    }
    let job = failure.get("log_source_jobs")?.as_array()?.first()?;
    Some(format!(
        "`{}` (id `{}`)",
        display(&value_string(job, "name")),
        display(&value_string(job, "job_id")),
    ))
}

fn render_run(run: &Value) -> String {
    let mut out = format!(
        "- {} run `{}` ({} on `{}`) — status `{}`, conclusion `{}`\n",
        display(&value_string(run, "url")),
        display(&value_string(run, "run_id")),
        display(&value_string(run, "event")),
        display(&value_string(run, "head_branch")),
        display(&value_string(run, "status")),
        display(&value_string(run, "conclusion")),
    );
    out.push_str(&format!(
        "  - event-reported head SHA: `{}`\n",
        display(&value_string(run, "event_reported_head_sha"))
    ));
    out.push_str(&format!(
        "  - current head of that ref: `{}`\n",
        display(&value_string(run, "current_ref_head_sha"))
    ));
    let checkout = run
        .get("actual_checkout_shas")
        .and_then(Value::as_array)
        .map(|shas| {
            shas.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    out.push_str(&format!(
        "  - commit actually checked out: `{}`\n",
        display(&checkout)
    ));
    if let Some(pr) = run.get("pr_number").and_then(Value::as_u64) {
        out.push_str(&format!("  - pull request: #{pr}\n"));
    }
    for line in run
        .get("checkout_evidence")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .take(3)
    {
        out.push_str(&format!(
            "  - checkout evidence: `{}`\n",
            truncate_chars(line, 200)
        ));
    }
    for job in run
        .get("failed_jobs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .take(3)
    {
        out.push_str(&format!(
            "  - failed job `{}` (id `{}`): {}\n",
            display(&value_string(job, "name")),
            display(&value_string(job, "job_id")),
            display(&value_string(job, "url")),
        ));
        let steps = job
            .get("failed_steps")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|step| display(&value_string(step, "name")).to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        if !steps.is_empty() {
            out.push_str(&format!("  - failing step(s): `{steps}`\n"));
        }
    }
    out
}
