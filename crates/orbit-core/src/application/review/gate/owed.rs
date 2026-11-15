//! Evidence a workspace rule owes for a claimed leaf's candidate, derived by
//! Orbit rather than read from the reviewer's report.
//!
//! A `[[review.host_evidence]]` rule names a check, the paths (and,
//! optionally, the task tags) that owe it and the OS it runs on. When a
//! claimed leaf's candidate changes a matching path, or its task carries a
//! listed tag, on a host that cannot run the check in its agent lane — a
//! `codeql` rule on a host of another OS, a `host_sandbox_test` rule on a
//! host of its own OS — the requirement is owed:
//!
//! - admission hands it to the reviewer in the manifest, as owner-fulfilled
//!   evidence it must record `not_run` and never attempt;
//! - settlement adds it to the reviewer's named evidence whatever the report
//!   says, so a reviewer that omitted it, or claimed a pass for a check its
//!   host cannot run, never ships the candidate unverified;
//! - a verdict whose only gaps are owed checks settles into the evidence
//!   hold, which the host or the owner fulfils; any other gap, or an open
//!   finding, blocks as before.
//!
//! Once every owed check has arrived for the held candidate, the task's next
//! run that rebuilds the same tree on the same base settles the held review
//! without a reviewer ([`owed_hold_received`]): its findings, records and
//! verdict are the held certificate's, and the arrived evidence passes it.
//!
//! [ORB-15630] A claimed before-landing review on a host of a known OS other
//! than Linux, such as macOS, need not hold for Linux CodeQL the pull
//! request's hosted CI runs. [ORB-15677] Only a rule that names that hosted
//! check (`hosted_check`) is the owner's: its candidate is already
//! published, the pull request's hosted CI runs the rule's exact command on
//! it, and the owner's landing waits for that check to pass. Admission and
//! settlement record such a requirement as owner-owed instead
//! ([`split_owner_owed`], [`Judgement::defer_to_owner`]): the reviewer
//! neither attempts nor names it, a CodeQL record it left short of a pass is
//! excluded in its favour, and the certificate names it in
//! `owner_owed_evidence`. A Linux CodeQL rule without a hosted check — a
//! custom query, or a workspace with no hosted CodeQL — and any Linux CodeQL
//! the reviewer names that no such rule covers keep the hold, as does a
//! before-PR review, since nothing hosted runs before the pull request
//! opens. A failed CodeQL record still blocks.
//!
//! A claimed before-landing reviewer on a known non-Linux host also owes
//! the Linux sandbox tests it names, without a workspace rule. These keep
//! the evidence hold and the owner's fulfilment path; a failed required
//! record still blocks.

use orbit_common::OrbitError;
use std::path::Path;

use orbit_engine::review_gate::{CandidateIdentity, committed_paths, path_text_at};
use orbit_types::task::HostOs;
use orbit_types::workflow::{
    BUBBLEWRAP_DEFERRAL_PREFIX, CommitIdentity, EvidenceHostOs, HostSandboxCommand,
    REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewEvidenceHold, ReviewEvidenceKind,
    ReviewEvidenceRequirement, ReviewValidation, ReviewVerdict, ValidationOutcome, ValidationRole,
    owed_requirements,
};

use super::super::evidence::{RecordedCargoScope, deferred_test, recorded_cargo_scope};
use super::super::fulfilment::fulfilment_artifact_paths;

use crate::OrbitRuntime;

use super::context::GateContext;
use super::judgement::Judgement;

/// What the run's captured host-evidence rules owe for `commits` and the
/// run's task tags on this host. Only a claimed leaf owes evidence: a local
/// run's host is the owner.
pub(super) fn owed_evidence<'a>(
    runtime: &OrbitRuntime,
    context: &GateContext,
    commits: impl IntoIterator<Item = &'a CommitIdentity>,
) -> Result<Vec<ReviewEvidenceRequirement>, OrbitError> {
    let rules = match &context.admission {
        Some(admission) if context.claimed && !admission.host_evidence.is_empty() => {
            &admission.host_evidence
        }
        _ => return Ok(Vec::new()),
    };
    let mut changed = Vec::new();
    for commit in commits {
        changed.extend(committed_paths(&context.workspace_path, &commit.commit)?);
    }
    let tags: Vec<String> = context
        .tasks
        .iter()
        .flat_map(|task| task.tags.iter().cloned())
        .collect();
    Ok(owed_requirements(
        rules,
        &changed,
        &tags,
        EvidenceHostOs::of_host(runtime.host_os()),
    ))
}

/// Whether this review's host leaves Linux CodeQL to the owner and the pull
/// request's hosted CI: a claimed before-landing review on a host of a
/// known OS other than Linux.
pub(super) fn defers_linux_codeql(runtime: &OrbitRuntime, context: &GateContext) -> bool {
    context.claimed
        && context
            .admission
            .as_ref()
            .is_some_and(|admission| admission.gates_landing())
        && runtime.host_os().is_some_and(|os| os != HostOs::Linux)
}

/// Split `owed` into what holds the review and what the owner owes instead:
/// with `defers`, every Linux CodeQL requirement whose rule names the pull
/// request's hosted check that runs it is the owner's.
pub(super) fn split_owner_owed(
    owed: Vec<ReviewEvidenceRequirement>,
    defers: bool,
) -> (
    Vec<ReviewEvidenceRequirement>,
    Vec<ReviewEvidenceRequirement>,
) {
    if !defers {
        return (owed, Vec::new());
    }
    owed.into_iter()
        .partition(|required| !(linux_codeql(required) && required.hosted_check.is_some()))
}

/// A CodeQL requirement that runs on Linux, or on no named OS.
fn linux_codeql(required: &ReviewEvidenceRequirement) -> bool {
    required.kind == ReviewEvidenceKind::CodeQl
        && required.os.is_none_or(|os| os == EvidenceHostOs::Linux)
}

/// The held review `candidate` settles without a reviewer: the task's hold
/// names only checks its certificate recorded as owed, every one of them has
/// arrived for the held tree, and `candidate` is that tree on the held base
/// under the same task meaning. `None` sends the candidate to a reviewer.
pub(super) fn owed_hold_received(
    runtime: &OrbitRuntime,
    context: &GateContext,
    candidate: &CandidateIdentity,
) -> Result<Option<(ReviewEvidenceHold, ReviewCertificate)>, OrbitError> {
    let [task_id] = context.task_ids.as_slice() else {
        return Ok(None);
    };
    let Some(hold) = super::super::evidence::evidence_hold(runtime, task_id)? else {
        return Ok(None);
    };
    let Some(certificate) = runtime
        .get_task_artifact(task_id, REVIEW_GATE_ARTIFACT)?
        .and_then(|artifact| serde_json::from_slice::<ReviewCertificate>(&artifact.content).ok())
    else {
        return Ok(None);
    };
    let owed = |required: &ReviewEvidenceRequirement| {
        certificate.owed_evidence.iter().any(|owed| {
            owed.kind == required.kind
                && owed.command == required.command
                && owed.artifact == required.artifact
        })
    };
    let received = certificate.attempt_id == hold.attempt_id
        && certificate.final_candidate == hold.candidate
        && certificate.task_meaning_digest == hold.task_meaning_digest
        && hold.task_meaning_digest == context.task_digests.1
        && !hold.requirements.is_empty()
        && hold.requirements.iter().all(owed)
        && certificate.repair_commits.is_empty()
        && candidate.head.tree == hold.candidate.tree
        && candidate.base.tree == certificate.base.tree
        && super::super::evidence::evidence_only(&certificate, &hold.requirements)
        && super::super::evidence::evidence_ready(runtime, task_id, &hold)?;
    Ok(received.then_some((hold, certificate)))
}

/// Words that mark a changed test source, or its path, as touching the
/// sandbox: a sandbox test, its fixture or its support code.
const SANDBOX_MARKERS: [&str; 3] = ["sandbox", "bwrap", "bubblewrap"];

/// [ORB-25096] Which deferred Bubblewrap tests a candidate's change can
/// reach. A test the candidate cannot reach is not owed as host evidence:
/// a host whose kernel refuses user namespaces would never produce it, and
/// the pull request's required checks still run it before landing.
///
/// The rule is conservative. The candidate reaches every deferred test when
/// it changes any path other than Markdown and Rust sources under a `tests`
/// directory: production code, a manifest, a build script, configuration,
/// a script or an asset. It reaches a test when a changed test source,
/// before or after the change, names the test or mentions a sandbox, or the
/// source's path does.
pub(super) struct BubblewrapReach {
    beyond_tests: bool,
    test_sources: Vec<String>,
}

impl BubblewrapReach {
    /// The reach that every deferred test is within.
    pub(super) fn everything() -> Self {
        Self {
            beyond_tests: true,
            test_sources: Vec::new(),
        }
    }

    /// The reach of `commits`, the change from `base` to `head`.
    pub(super) fn of_candidate<'a>(
        workspace_path: &Path,
        base: &str,
        head: &str,
        commits: impl IntoIterator<Item = &'a CommitIdentity>,
    ) -> Result<Self, OrbitError> {
        let mut changed = Vec::new();
        for commit in commits {
            changed.extend(committed_paths(workspace_path, &commit.commit)?);
        }
        changed.sort();
        changed.dedup();
        let mut test_sources = Vec::new();
        for path in &changed {
            if path.ends_with(".md") {
                continue;
            }
            let lower = path.to_ascii_lowercase();
            let test_source =
                path.ends_with(".rs") && path.split('/').any(|component| component == "tests");
            if !test_source || SANDBOX_MARKERS.iter().any(|marker| lower.contains(marker)) {
                return Ok(Self::everything());
            }
            for commit in [base, head] {
                if let Some(text) = path_text_at(workspace_path, commit, path)? {
                    test_sources.push(text);
                }
            }
        }
        Ok(Self {
            beyond_tests: false,
            test_sources,
        })
    }

    /// Whether the change can reach the deferred `test`.
    fn reaches(&self, test: &str) -> bool {
        let name = test.rsplit("::").next().unwrap_or(test);
        self.beyond_tests
            || self.test_sources.iter().any(|text| {
                let lower = text.to_ascii_lowercase();
                text.contains(name) || SANDBOX_MARKERS.iter().any(|marker| lower.contains(marker))
            })
    }
}

impl Judgement {
    /// Whether a required record carries a Bubblewrap deferral notice.
    pub(super) fn has_bubblewrap_deferral(&self) -> bool {
        self.validation.iter().any(|record| {
            record
                .deferred
                .iter()
                .any(|notice| notice.trim().starts_with(BUBBLEWRAP_DEFERRAL_PREFIX))
        })
    }

    /// Add the reviewer's named Linux sandbox tests to `owed` on a claimed
    /// before-landing host of another known OS. Workspace rules take
    /// precedence for the same check, and failed required records stay
    /// failed rather than becoming unavailable evidence.
    pub(super) fn add_named_linux_sandbox_evidence(
        &self,
        owed: &mut Vec<ReviewEvidenceRequirement>,
    ) {
        for named in &self.external_evidence {
            if named.kind != ReviewEvidenceKind::HostSandboxTest
                || named.os != Some(EvidenceHostOs::Linux)
                || owed.iter().any(|required| {
                    required.artifact == named.artifact
                        || (required.kind == named.kind && required.command == named.command)
                })
                || self.validation.iter().any(|record| {
                    record.role == ValidationRole::Required
                        && record.command.trim() == named.command.trim()
                        && record.outcome == ValidationOutcome::Failed
                })
            {
                continue;
            }
            owed.push(named.clone());
        }
    }

    /// [ORB-24656] Detect a Bubblewrap self-deferral in the validation
    /// records themselves and return the Linux `host_sandbox_test`
    /// requirements it owes, whether or not the reviewer named them.
    ///
    /// Only a passing verdict with no open finding is rerouted: an
    /// `incomplete` report keeps the strict named-requirement rules, and a
    /// failed required record is an answer, not missing evidence. A
    /// requirement is derived from the deferred test a notice names: a
    /// reviewer-named Linux requirement that runs the record's own package
    /// and target with that test as its filter (or a test path ending in
    /// it), else the record's own cargo scope —
    /// `cargo test -p <package> --test <target> <test>` for an integration
    /// record and `cargo test -p <package> --lib <test>` for a
    /// package-wide one. A notice no such command can name stays on its
    /// record and keeps blocking. Each covered notice is cleared from its
    /// record with a note naming the owed command, so the broader pass is
    /// judged with the requirements [`Self::require_owed_evidence`] adds.
    ///
    /// [ORB-25096] A deferred test the candidate cannot `reach` is not owed,
    /// unless a workspace rule in `owed` owes the same check: its notice is
    /// cleared with a note saying so, and a reviewer-named requirement that
    /// covers only that notice leaves the named evidence.
    pub(super) fn synthesized_bubblewrap_requirements(
        &mut self,
        owed: &[ReviewEvidenceRequirement],
        reach: &BubblewrapReach,
    ) -> Vec<ReviewEvidenceRequirement> {
        if !self.verdict.passed() || self.has_open_findings() {
            return Vec::new();
        }
        let named = self.external_evidence.clone();
        let mut requirements: Vec<ReviewEvidenceRequirement> = Vec::new();
        let mut covered: Vec<(usize, String, String, String)> = Vec::new();
        let mut released: Vec<(usize, String, String)> = Vec::new();
        let mut released_named: Vec<ReviewEvidenceRequirement> = Vec::new();
        for (index, record) in self.validation.iter().enumerate() {
            if record.role != ValidationRole::Required
                || record.outcome != ValidationOutcome::Passed
            {
                continue;
            }
            for notice in &record.deferred {
                if !notice.trim().starts_with(BUBBLEWRAP_DEFERRAL_PREFIX) {
                    continue;
                }
                let Some(test) = deferred_test(notice) else {
                    continue;
                };
                let filter_matches =
                    |filter: &str| filter == test || filter.ends_with(&format!("::{test}"));
                // As `covers_deferral` requires, a named run covers the notice
                // only in the record's own scope: the same package and target,
                // or for a record outside the two shapes, the same command. A
                // same-named test in another crate runs a different path.
                let scope = recorded_cargo_scope(&record.command);
                let mut requirement = named.iter().find_map(|candidate| {
                    if candidate.kind != ReviewEvidenceKind::HostSandboxTest
                        || candidate.os != Some(EvidenceHostOs::Linux)
                    {
                        return None;
                    }
                    let admitted = HostSandboxCommand::admit(&candidate.command, &[]);
                    let covers = match (&scope, admitted) {
                        (
                            Some(RecordedCargoScope::Integration { package, target }),
                            Ok(HostSandboxCommand::CargoTest {
                                package: named_package,
                                target: named_target,
                                filter,
                            }),
                        ) => {
                            named_package == *package
                                && named_target == *target
                                && filter.as_deref().is_some_and(filter_matches)
                        }
                        (
                            Some(RecordedCargoScope::Package { package }),
                            Ok(HostSandboxCommand::CargoLibTest {
                                package: named_package,
                                filter,
                            }),
                        ) => {
                            named_package == *package
                                && filter.as_deref().is_some_and(filter_matches)
                        }
                        (None, _) => candidate.command.trim() == record.command.trim(),
                        _ => false,
                    };
                    covers.then(|| candidate.clone())
                });
                let reviewer_named = requirement.is_some();
                if requirement.is_none() {
                    let command = match recorded_cargo_scope(&record.command) {
                        Some(RecordedCargoScope::Integration { package, target }) => {
                            format!("cargo test -p {package} --test {target} {test}")
                        }
                        Some(RecordedCargoScope::Package { package }) => {
                            format!("cargo test -p {package} --lib {test}")
                        }
                        None => match HostSandboxCommand::admit(&record.command, &[]) {
                            Ok(HostSandboxCommand::CargoTest { filter, .. })
                            | Ok(HostSandboxCommand::CargoLibTest { filter, .. })
                                if filter.as_deref().is_some_and(filter_matches) =>
                            {
                                record.command.trim().to_string()
                            }
                            _ => continue,
                        },
                    };
                    if HostSandboxCommand::admit(&command, &[]).is_err() {
                        continue;
                    }
                    let slug = test
                        .chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                                c
                            } else {
                                '-'
                            }
                        })
                        .collect::<String>();
                    let mut artifact = format!("evidence/host-sandbox/bubblewrap-{slug}.json");
                    let mut suffix = 2;
                    while {
                        let (cand_res, cand_log) = match fulfilment_artifact_paths(&artifact) {
                            Ok(paths) => paths,
                            Err(_) => (artifact.clone(), format!("{artifact}.log.json")),
                        };
                        named.iter().any(|known| {
                            review_evidence_paths_collide(
                                &cand_res, &cand_log, &command, known, false,
                            )
                        }) || owed.iter().any(|known| {
                            review_evidence_paths_collide(
                                &cand_res, &cand_log, &command, known, true,
                            )
                        }) || requirements.iter().any(|known| {
                            review_evidence_paths_collide(
                                &cand_res, &cand_log, &command, known, true,
                            )
                        })
                    } {
                        artifact = format!("evidence/host-sandbox/bubblewrap-{slug}-{suffix}.json");
                        suffix += 1;
                    }
                    requirement = Some(ReviewEvidenceRequirement {
                        kind: ReviewEvidenceKind::HostSandboxTest,
                        name: test.to_string(),
                        command,
                        artifact,
                        os: Some(EvidenceHostOs::Linux),
                        hosted_check: None,
                    });
                }
                let Some(requirement) = requirement else {
                    continue;
                };
                if !reach.reaches(test)
                    && !owed.iter().any(|known| {
                        known.artifact == requirement.artifact
                            || (known.kind == requirement.kind
                                && known.command == requirement.command)
                    })
                {
                    released.push((index, notice.clone(), test.to_string()));
                    if reviewer_named {
                        released_named.push(requirement);
                    }
                    continue;
                }
                // Settlement drops a requirement whose artifact a rule owes for
                // another command. Keep the notice blocking rather than clear it
                // for a requirement that is never owed.
                if owed.iter().any(|known| {
                    let alias_match = match (
                        fulfilment_artifact_paths(&known.artifact),
                        fulfilment_artifact_paths(&requirement.artifact),
                    ) {
                        (Ok((known_res, _)), Ok((req_res, _))) => known_res == req_res,
                        _ => known.artifact == requirement.artifact,
                    };
                    alias_match
                        && (known.kind != requirement.kind || known.command != requirement.command)
                }) {
                    continue;
                }
                covered.push((
                    index,
                    notice.clone(),
                    test.to_string(),
                    requirement.command.clone(),
                ));
                if !requirements.iter().any(|known| {
                    known.kind == requirement.kind && known.command == requirement.command
                }) {
                    requirements.push(requirement);
                }
            }
        }
        for (index, notice, test, command) in covered {
            let record = &mut self.validation[index];
            record.deferred.retain(|kept| kept != &notice);
            append_note(
                record,
                &format!(
                    "deferred Bubblewrap test `{test}` is owed as host_sandbox_test \
                     evidence: `{command}`"
                ),
            );
        }
        for (index, notice, test) in released {
            let record = &mut self.validation[index];
            record.deferred.retain(|kept| kept != &notice);
            append_note(
                record,
                &format!(
                    "deferred Bubblewrap test `{test}` is not owed as host evidence: the \
                     candidate changes only tests and docs that neither name it nor touch a \
                     sandbox, and the pull request's required checks still run it"
                ),
            );
        }
        // A named requirement stays when another notice is still owed by it.
        released_named.retain(|named| {
            !requirements
                .iter()
                .any(|kept| kept.kind == named.kind && kept.command == named.command)
        });
        self.external_evidence.retain(|named| {
            !released_named
                .iter()
                .any(|released| released.kind == named.kind && released.command == named.command)
        });
        for record in &mut self.validation {
            if record.role == ValidationRole::Required
                && record.outcome == ValidationOutcome::NotRun
                && released_named
                    .iter()
                    .any(|released| released.command.trim() == record.command.trim())
            {
                record.role = ValidationRole::Excluded;
                append_note(
                    record,
                    "not owed: it covers only a deferred Bubblewrap test the candidate \
                     cannot reach",
                );
            }
        }
        requirements
    }

    /// Hold the verdict to the evidence `owed` names. Each owed requirement
    /// replaces any the reviewer named for the same check, and its required
    /// record is `not_run` until the evidence arrives: a reviewer record of
    /// the check is reset, one naming the same program with another command
    /// is excluded in its favour, and a missing one is added. A passing
    /// verdict that has not seen the evidence becomes `incomplete`.
    ///
    /// A required record that already failed is an answer, not missing
    /// evidence: it stays required and failed (an exact match, or another
    /// invocation of the same CodeQL program), is not replaced in
    /// `external_evidence`, and does not turn a passing verdict into an
    /// evidence hold, so settlement blocks on it as on any other failure.
    pub(super) fn require_owed_evidence(&mut self, owed: &[ReviewEvidenceRequirement]) {
        if owed.is_empty() {
            return;
        }
        let host_note = |required: &ReviewEvidenceRequirement| {
            format!(
                "owed host evidence `{}`: Orbit runs it on {} for this \
                 candidate",
                required.name,
                required.os.map_or("its host", EvidenceHostOs::as_str)
            )
        };
        let mut pending = Vec::new();
        for required in owed {
            let failed = self.validation.iter().any(|record| {
                record.role == ValidationRole::Required
                    && record.outcome == ValidationOutcome::Failed
                    && (record.command.trim() == required.command.trim()
                        || (required.kind == ReviewEvidenceKind::CodeQl
                            && program(&record.command) == program(&required.command)))
            });
            if failed {
                continue;
            }
            pending.push(required);
            self.external_evidence.retain(|named| {
                !(named.artifact == required.artifact
                    || (named.kind == required.kind
                        && (named.command == required.command
                            || named.kind == ReviewEvidenceKind::CodeQl)))
            });
            self.external_evidence.push(required.clone());
            let mut recorded = false;
            for record in &mut self.validation {
                if record.role != ValidationRole::Required {
                    continue;
                }
                if record.command.trim() == required.command {
                    recorded = true;
                    record.outcome = ValidationOutcome::NotRun;
                    append_note(record, &host_note(required));
                } else if required.kind == ReviewEvidenceKind::CodeQl
                    && program(&record.command) == program(&required.command)
                {
                    record.role = ValidationRole::Excluded;
                    record.outcome = ValidationOutcome::NotRun;
                    append_note(
                        record,
                        &format!(
                            "superseded by `{}`, {}",
                            required.command,
                            host_note(required)
                        ),
                    );
                }
            }
            if !recorded {
                self.validation.push(ReviewValidation {
                    id: None,
                    command: required.command.clone(),
                    outcome: ValidationOutcome::NotRun,
                    role: ValidationRole::Required,
                    note: Some(host_note(required)),
                    check: None,
                    control: None,
                    sources: Vec::new(),
                    mutation_target: Vec::new(),
                    repaired_blob: Default::default(),
                    deferred: Vec::new(),
                    baseline: None,
                });
            }
        }
        if self.verdict.passed() && !pending.is_empty() {
            self.verdict = ReviewVerdict::Incomplete;
            self.validation_complete = false;
            self.escalate(&format!(
                "owed_evidence: {} not yet run on the host that owes it",
                pending
                    .iter()
                    .map(|required| format!("`{}`", required.name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
}

impl Judgement {
    /// [ORB-15630] Record `owner_owed` as owed to the owner and the pull
    /// request's hosted CI when `defers`. Linux CodeQL the reviewer named for
    /// the same command leaves the named evidence; any other it named stays
    /// and holds [ORB-15677]. A record of a deferred check that did not fail
    /// is excluded in its favour, and a missing one is added excluded, so the
    /// certificate says why it never ran. A failed record stays and keeps
    /// blocking, and a record of a check still pending is never excluded.
    pub(super) fn defer_to_owner(
        &mut self,
        owner_owed: &[ReviewEvidenceRequirement],
        defers: bool,
    ) {
        if !defers || owner_owed.is_empty() {
            return;
        }
        self.external_evidence.retain(|named| {
            !(linux_codeql(named)
                && owner_owed
                    .iter()
                    .any(|owed| owed.command.trim() == named.command.trim()))
        });
        for required in owner_owed {
            let note = format!(
                "owed to the owner and the pull request's hosted check `{}`: `{}` runs on \
                 Linux, not on this review's host, and does not hold a before-landing review",
                required.hosted_check.as_deref().unwrap_or_default(),
                required.name
            );
            let mut recorded = false;
            for record in &mut self.validation {
                // Another invocation of the same CodeQL program is the same
                // check; a shared runner such as `make` is not, and neither is
                // the command of a check that still holds the review.
                let same = record.command.trim() == required.command.trim()
                    || (program(&required.command).contains("codeql")
                        && program(&record.command) == program(&required.command)
                        && !self
                            .external_evidence
                            .iter()
                            .any(|pending| pending.command.trim() == record.command.trim()));
                if !same || record.role == ValidationRole::ExpectedFailure {
                    continue;
                }
                recorded = true;
                if record.outcome == ValidationOutcome::Failed {
                    continue;
                }
                record.role = ValidationRole::Excluded;
                record.outcome = ValidationOutcome::NotRun;
                record.deferred.clear();
                append_note(record, &note);
            }
            if !recorded {
                self.validation.push(ReviewValidation {
                    id: None,
                    command: required.command.clone(),
                    outcome: ValidationOutcome::NotRun,
                    role: ValidationRole::Excluded,
                    note: Some(note),
                    check: None,
                    control: None,
                    sources: Vec::new(),
                    mutation_target: Vec::new(),
                    repaired_blob: Default::default(),
                    deferred: Vec::new(),
                    baseline: None,
                });
            }
        }
        self.host_decided = true;
        self.owner_owed = owner_owed.to_vec();
    }
}

/// The program a command runs: its first word.
fn program(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or_default()
}

fn append_note(record: &mut ReviewValidation, note: &str) {
    record.note = Some(match record.note.take() {
        Some(previous) if !previous.trim().is_empty() => format!("{previous}; {note}"),
        _ => note.to_string(),
    });
}

/// [ORB-24665] Whether a candidate requirement's canonical result and derived log
/// paths collide with an existing requirement's reserved canonical keys.
fn review_evidence_paths_collide(
    cand_res: &str,
    cand_log: &str,
    command: &str,
    known: &ReviewEvidenceRequirement,
    allow_same_command: bool,
) -> bool {
    let (known_res, known_log) = match fulfilment_artifact_paths(&known.artifact) {
        Ok((res, log)) => (res, log),
        Err(_) => {
            let overlap = known.artifact == cand_res || known.artifact == cand_log;
            if !overlap {
                return false;
            }
            if allow_same_command
                && known.kind == ReviewEvidenceKind::HostSandboxTest
                && known.command == command
            {
                return false;
            }
            return true;
        }
    };
    let overlap = cand_res == known_res
        || cand_res == known_log
        || cand_log == known_res
        || cand_log == known_log;
    if !overlap {
        return false;
    }
    if allow_same_command
        && known.kind == ReviewEvidenceKind::HostSandboxTest
        && known.command == command
        && cand_res == known_res
        && cand_log == known_log
    {
        return false;
    }
    true
}
