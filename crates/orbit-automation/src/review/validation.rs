//! What a reviewer's validation records establish about a candidate
//! [ORB-11528].
//!
//! An honest reviewer records more than the checks that had to pass: the
//! negative control that proves a regression, the deployment it was never
//! authorized to perform, the diagnostic attempt a corrected rerun replaced.
//! Reading every record as a requirement turns that honesty into a refusal,
//! and reading a bare pass verdict as sufficient throws the evidence away.
//! This module sits between the two: the reviewer classifies each record and
//! the rules here decide whether the outcomes support the claim.
//!
//! Both the gate that issues a certificate and the coverage rules that spend
//! one read these rules, so a certificate never means one thing when it is
//! written and another when it is used.
//!
//! A classification is bound to evidence beyond its label and note
//! [ORB-14192]: a negative control names its kind and the in-scope sources it
//! exercises, a failed diagnostic names out-of-scope sources for its
//! failures, and a required check an earlier report revision recorded must
//! still be accounted for. Relabeling a failed required check therefore
//! contradicts its own sources or its retained history instead of clearing it.
//!
//! A counterfactual control names the files it temporarily mutated apart
//! from its sources [ORB-14616]: the mutated file is usually the production
//! code a test-only change guards, outside the candidate's scope. Its sources
//! (the checks that rejected the mutation) stay bound to the scope, and
//! settlement confirms each mutation target came back byte-identical.
//!
//! A failed check the owner trusts is never a diagnostic [ORB-14684]: a
//! `workflow.required_validation_commands` or `review.baseline_commands`
//! entry passes, or carries a baseline claim settlement reproduces on the
//! pinned base. The reviewer's own sources cannot excuse it, so choosing the
//! `diagnostic` role does not avoid the base rerun.
//!
//! An earlier record that carries an id is accounted for by that id alone
//! [ORB-14370]: the final report carries it forward, whatever its command now
//! reads, or retires it with a reason. Records written without ids keep the
//! command-identity rules.

use orbit_common::fs::selector::overlaps;
use orbit_types::workflow::{
    RecordGap, RetainedObligation, RetiredValidation, ReviewValidation, ValidationOutcome,
    ValidationRole, record_gap,
};

/// Why a validation set does not establish a validated candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationDefect {
    /// Nothing in the set is a required check that passed, so the set says
    /// nothing about the candidate.
    NoRequiredCheck,
    /// A required check did not pass on the final candidate.
    RequiredNotPassed {
        command: String,
        outcome: ValidationOutcome,
    },
    /// The outcome contradicts the classification the record was filed
    /// under: a negative control that passed, an excluded action that ran.
    RoleContradicted {
        command: String,
        role: ValidationRole,
        outcome: ValidationOutcome,
    },
    /// A superseded attempt that no same-identity required pass replaced, so the
    /// diagnostic never reached a final-candidate outcome.
    SupersededWithoutReplacement { command: String },
    /// A classification other than `required` with nothing explaining it.
    ClassificationUnexplained {
        command: String,
        role: ValidationRole,
    },
    /// A negative control or failed diagnostic without the structured
    /// evidence its role needs: `missing` names the absent field.
    ClassificationUnevidenced {
        command: String,
        role: ValidationRole,
        missing: &'static str,
    },
    /// A negative control whose source lies outside the candidate's scope:
    /// an unrelated failure is a diagnostic, not a deliberate control.
    ControlOutOfScope { command: String, source: String },
    /// [ORB-14616] A file a control says it temporarily mutated that the
    /// final candidate does not carry byte-identical to the reviewed
    /// candidate: the mutation was left in place.
    MutationTargetChanged { command: String, target: String },
    /// [ORB-14632] A file a control says it temporarily mutated, named so
    /// that it is not a repository-relative path settlement could compare.
    MutationTargetInvalid { command: String, target: String },
    /// A failed diagnostic whose source lies inside the candidate's scope:
    /// that failure is the task's own and blocks like a required check.
    DiagnosticInScope { command: String, source: String },
    /// A failed diagnostic judged with no recorded scope, so nothing shows
    /// its failures are unrelated.
    ScopeUnknown { command: String },
    /// [ORB-14684] A failed diagnostic of a command the owner trusts (a
    /// required command or a `review.baseline_commands` entry): such a
    /// check needs a passing record or a baseline claim settlement
    /// reproduced, never the reviewer's own sources.
    TrustedCheckDiagnostic { command: String },
    /// A record failing on the final candidate shares its check with a
    /// required record that passed there: one check cannot do both.
    CheckContradicted {
        command: String,
        role: ValidationRole,
    },
    /// A required check an earlier report revision of the attempt recorded
    /// that the final records neither rerun nor legitimately resolve.
    ObligationDropped {
        command: String,
        outcome: ValidationOutcome,
        role: Option<ValidationRole>,
    },
    /// A required record an earlier report revision filed under `id` that
    /// the final report neither carries forward nor legitimately retires.
    RecordDropped {
        id: String,
        command: String,
        outcome: ValidationOutcome,
        gap: RecordGap,
    },
    /// The certificate has no captured owner validation policy. It must be
    /// re-established under a fresh delivery admission.
    HostContractMissing,
    /// A captured host-required command is absent, reclassified, or has no
    /// valid required replacement on the final candidate.
    HostCheckNotEstablished { command: String },
}

impl ValidationDefect {
    /// [ORB-14616] Whether the defect is in the shape of a record rather
    /// than in what the checks observed: a missing note, control kind or
    /// sources, or a source outside the scope (such as a counterfactual
    /// naming the file it mutated in `sources` instead of
    /// `mutation_target`), or a mutation target that is not a
    /// repository-relative path. The reviewer can correct such a report without
    /// rerunning anything, so it is returned to the reviewer once before the
    /// verdict settles.
    pub fn correctable(&self) -> bool {
        matches!(
            self,
            ValidationDefect::ClassificationUnexplained { .. }
                | ValidationDefect::ClassificationUnevidenced { .. }
                | ValidationDefect::ControlOutOfScope { .. }
                | ValidationDefect::MutationTargetInvalid { .. }
        )
    }

    /// The escalation reason recorded on the certificate.
    ///
    /// A denied required check keeps its own `validation_unavailable` label:
    /// the runner refused the command, which is neither a defect in the
    /// candidate nor evidence about it.
    pub fn reason(&self) -> String {
        match self {
            ValidationDefect::NoRequiredCheck => "validation_incomplete: a pass needs at least \
                 one required candidate check that passed on the final candidate"
                .to_string(),
            ValidationDefect::RequiredNotPassed { command, outcome }
                if *outcome == ValidationOutcome::Denied =>
            {
                format!(
                    "validation_unavailable: the runner denied required check `{command}`; the \
                     candidate is kept for unrestricted validation"
                )
            }
            ValidationDefect::RequiredNotPassed { command, outcome } => format!(
                "validation_incomplete: required check `{command}` is {} on the final candidate",
                outcome.as_str()
            ),
            ValidationDefect::RoleContradicted {
                command,
                role,
                outcome,
            } => format!(
                "validation_contradicted: `{command}` was recorded as {} but is {}",
                role.as_str(),
                outcome.as_str()
            ),
            ValidationDefect::SupersededWithoutReplacement { command } => format!(
                "validation_incomplete: superseded attempt `{command}` has no same-identity \
                 required check that passed on the final candidate"
            ),
            ValidationDefect::ClassificationUnexplained { command, role } => format!(
                "validation_unexplained: `{command}` is recorded as {} with no note explaining it",
                role.as_str()
            ),
            ValidationDefect::ClassificationUnevidenced {
                command,
                role,
                missing,
            } => format!(
                "validation_unevidenced: `{command}` is recorded as {} without `{missing}`; a \
                 certificate issued before this evidence was required is re-established by a \
                 fresh review",
                role.as_str()
            ),
            ValidationDefect::ControlOutOfScope { command, source } => format!(
                "validation_contradicted: negative control `{command}` names `{source}`, outside \
                 the candidate's scope; record an unrelated failure as diagnostic, and list a \
                 file a counterfactual temporarily mutated in `mutation_target`, not `sources`"
            ),
            ValidationDefect::MutationTargetChanged { command, target } => format!(
                "validation_contradicted: control `{command}` mutated `{target}`, which the \
                 final candidate does not carry byte-identical to the reviewed candidate; the \
                 mutation was not restored"
            ),
            ValidationDefect::MutationTargetInvalid { command, target } => format!(
                "validation_unevidenced: control `{command}` names mutation_target `{target}`, \
                 which is not a repository-relative path, so its restoration cannot be checked; \
                 list each file the control temporarily mutated relative to the repository root"
            ),
            ValidationDefect::DiagnosticInScope { command, source } => format!(
                "validation_incomplete: diagnostic `{command}` failed in `{source}`, inside the \
                 candidate's scope, so it is a required failure"
            ),
            ValidationDefect::ScopeUnknown { command } => format!(
                "validation_incomplete: failed diagnostic `{command}` has no recorded candidate \
                 scope to show its failures are unrelated"
            ),
            ValidationDefect::TrustedCheckDiagnostic { command } => format!(
                "validation_incomplete: `{command}` is a trusted gate (a \
                 `workflow.required_validation_commands` or `review.baseline_commands` entry) \
                 and failed, so it cannot be recorded as diagnostic; a trusted gate needs a \
                 passing record or a baseline claim settlement reproduced on the pinned base"
            ),
            ValidationDefect::CheckContradicted { command, role } => format!(
                "validation_contradicted: `{command}` is recorded as a failing {} and as a \
                 required check that passed on the same final candidate",
                role.as_str()
            ),
            ValidationDefect::ObligationDropped {
                command,
                outcome,
                role,
            } => format!(
                "validation_incomplete: required check `{command}` was recorded {} by an earlier \
                 report revision of this attempt and the final report {}",
                outcome.as_str(),
                match role {
                    Some(role) => format!("reclassifies it as {}", role.as_str()),
                    None => "omits it".to_string(),
                }
            ),
            ValidationDefect::RecordDropped {
                id,
                command,
                outcome,
                gap,
            } => format!(
                "validation_incomplete: required validation record `{id}` (`{command}`) was \
                 recorded {} by an earlier report revision of this attempt and the final report {}",
                outcome.as_str(),
                gap.describe()
            ),
            ValidationDefect::HostContractMissing => "validation_contract_missing: this review has no captured host required-check list; dispatch a fresh delivery run after upgrading the workspace".to_string(),
            ValidationDefect::HostCheckNotEstablished { command } => format!(
                "validation_incomplete: host-required check `{command}` is not established as a required pass or a valid same-check replacement on the final candidate"
            ),
        }
    }
}

/// What the records are judged against beyond themselves.
#[derive(Debug, Clone, Copy)]
pub struct ValidationContext<'a> {
    /// Task selectors plus a `file:` selector for every path the candidate
    /// changed from its base.
    pub scope: &'a [String],
    /// Required-check records earlier report revisions of the attempt made.
    pub obligations: &'a [RetainedObligation],
    /// Retained record ids the final report retired, with their reasons.
    pub retired: &'a [RetiredValidation],
    /// Required commands captured by the candidate owner at delivery
    /// admission. `None` is a legacy/ambiguous contract; `Some([])` is an
    /// explicit empty host contract.
    pub required_validation_commands: Option<&'a [String]>,
    /// The owner's `review.baseline_commands` captured with the run. With
    /// the required commands, these are the checks a failed diagnostic may
    /// not name [ORB-14684]. Empty for a snapshot written before the field.
    pub baseline_commands: &'a [String],
}

impl Default for ValidationContext<'_> {
    fn default() -> Self {
        Self {
            scope: &[],
            obligations: &[],
            retired: &[],
            required_validation_commands: Some(&[]),
            baseline_commands: &[],
        }
    }
}

/// Read a reviewer's validation records as evidence about the final
/// candidate.
///
/// Every required check must have passed and at least one must exist; a
/// declared negative control must have failed, name its kind and sources in
/// the candidate's scope, and, when it runs on the candidate, not share its
/// check with a required pass; an excluded action must have stayed
/// unperformed; a superseded attempt must have a required pass anywhere in
/// the report that replaced it — the same effective identity: a non-empty `check`,
/// otherwise the command with whitespace and leading `NAME=value`
/// assignments normalized; a diagnostic must be an
/// observation that ran, and a failed one must not be a check the owner
/// trusts, must name sources all outside the scope and must not share its
/// check with a required pass. Every classification
/// other than `required` must explain itself, so an unexplained
/// reclassification is refused rather than trusted. Every retained
/// obligation must still be accounted for by a record of the same check that
/// is `required`, `superseded`, or — when the retained record never ran —
/// `excluded`. An obligation with a record id is the same check only as a
/// record carrying that id, and may instead be retired with a reason unless
/// it failed; one without an id is matched by effective identity. Records
/// carrying no classification are required checks, which keeps evidence
/// written before this contract conservative.
pub fn validation_evidence(
    records: &[ReviewValidation],
    context: &ValidationContext<'_>,
) -> Result<(), ValidationDefect> {
    let mut required_passed = false;

    for record in records {
        if record.role != ValidationRole::Required && !explained(record) {
            return Err(ValidationDefect::ClassificationUnexplained {
                command: record.command.clone(),
                role: record.role,
            });
        }
        match record.role {
            ValidationRole::Required => {
                if record.outcome != ValidationOutcome::Passed {
                    return Err(ValidationDefect::RequiredNotPassed {
                        command: record.command.clone(),
                        outcome: record.outcome,
                    });
                }
                required_passed = true;
            }
            ValidationRole::ExpectedFailure => negative_control(record, records, context)?,
            ValidationRole::Excluded
                if !matches!(
                    record.outcome,
                    ValidationOutcome::NotRun | ValidationOutcome::Denied
                ) =>
            {
                return Err(contradiction(record));
            }
            ValidationRole::Superseded if !passes_as_required(record, records) => {
                return Err(ValidationDefect::SupersededWithoutReplacement {
                    command: record.command.clone(),
                });
            }
            ValidationRole::Diagnostic => diagnostic(record, records, context)?,
            _ => {}
        }
    }

    for obligation in context.obligations {
        let obligation = &obligation.validation;
        if let Some(id) = obligation.record_id() {
            if let Some(gap) = record_gap(id, obligation.outcome, records, context.retired) {
                return Err(ValidationDefect::RecordDropped {
                    id: id.to_string(),
                    command: obligation.command.clone(),
                    outcome: obligation.outcome,
                    gap,
                });
            }
            continue;
        }
        if !obligation_resolved(obligation, records) {
            return Err(ValidationDefect::ObligationDropped {
                command: obligation.command.clone(),
                outcome: obligation.outcome,
                role: records
                    .iter()
                    .find(|record| same_check(obligation, record))
                    .map(|record| record.role),
            });
        }
    }

    let Some(host_required) = context.required_validation_commands else {
        return Err(ValidationDefect::HostContractMissing);
    };
    for command in host_required {
        let established = records.iter().any(|record| {
            same_host_command(record, command)
                && match record.role {
                    ValidationRole::Required => record.outcome == ValidationOutcome::Passed,
                    ValidationRole::Superseded => passes_as_required(record, records),
                    ValidationRole::ExpectedFailure
                    | ValidationRole::Excluded
                    | ValidationRole::Diagnostic => false,
                }
        });
        if !established {
            return Err(ValidationDefect::HostCheckNotEstablished {
                command: command.clone(),
            });
        }
    }

    if required_passed {
        Ok(())
    } else {
        Err(ValidationDefect::NoRequiredCheck)
    }
}

/// [ORB-14616] Every file a record says it temporarily mutated, as the
/// repository-relative path settlement compares between the reviewed and the
/// final candidate, paired with the record's command. Only the repository can
/// tell whether a target came back byte-identical, so settlement makes that
/// comparison itself; this only reads the targets. [ORB-14632] A target that
/// is not a repository-relative file path (absolute, home-relative, climbing
/// out of the repository or another selector kind) could never be compared,
/// so it is refused as [`ValidationDefect::MutationTargetInvalid`] rather than
/// passed. A `file:` selector reads as its path; `dir:` and `symbol:` are no
/// file and are refused.
pub fn mutation_targets(
    records: &[ReviewValidation],
) -> Result<Vec<(&str, String)>, ValidationDefect> {
    let mut targets = Vec::new();
    for record in records {
        for target in record
            .mutation_target
            .iter()
            .map(|target| target.trim())
            .filter(|target| !target.is_empty())
        {
            let path =
                repository_path(target).ok_or_else(|| ValidationDefect::MutationTargetInvalid {
                    command: record.command.clone(),
                    target: target.to_string(),
                })?;
            targets.push((record.command.as_str(), path));
        }
    }
    Ok(targets)
}

/// `target` as a normalized repository-relative path, or `None` when it
/// names nothing a candidate's tree could hold.
fn repository_path(target: &str) -> Option<String> {
    let path = target.strip_prefix("file:").unwrap_or(target);
    let drive = matches!(path.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
    if drive
        || ["dir:", "symbol:"]
            .iter()
            .any(|kind| path.starts_with(kind))
        || path.starts_with(['/', '~'])
        || path.contains(['\\', '\0'])
    {
        return None;
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            // Git's own store is no file of the candidate.
            part if part.eq_ignore_ascii_case(".git") => return None,
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// A negative control: it failed, names its kind and the code it exercises,
/// that code is the candidate's own, and a control run on the candidate does
/// not share its check with a required pass there. A counterfactual's
/// `mutation_target` is not a source: it may lie anywhere in the
/// repository, and settlement judges its restoration ([`mutation_targets`]).
fn negative_control(
    record: &ReviewValidation,
    records: &[ReviewValidation],
    context: &ValidationContext<'_>,
) -> Result<(), ValidationDefect> {
    if record.outcome != ValidationOutcome::Failed {
        return Err(contradiction(record));
    }
    let Some(control) = record.control else {
        return Err(unevidenced(record, "control"));
    };
    let sources = sources(record);
    if sources.is_empty() {
        return Err(unevidenced(record, "sources"));
    }
    if let Some(source) = sources
        .iter()
        .find(|source| !in_scope(source, context.scope))
    {
        return Err(ValidationDefect::ControlOutOfScope {
            command: record.command.clone(),
            source: (*source).to_string(),
        });
    }
    if control.runs_on_candidate() && passes_as_required(record, records) {
        return Err(ValidationDefect::CheckContradicted {
            command: record.command.clone(),
            role: record.role,
        });
    }
    Ok(())
}

/// A diagnostic is an observation that ran. A failed one is no check the
/// owner trusts, names where its failures lie, every place outside the
/// candidate's scope, and does not share its check with a required pass on
/// the same candidate.
fn diagnostic(
    record: &ReviewValidation,
    records: &[ReviewValidation],
    context: &ValidationContext<'_>,
) -> Result<(), ValidationDefect> {
    match record.outcome {
        ValidationOutcome::Passed => Ok(()),
        ValidationOutcome::Failed => {
            if trusted(record, context) {
                return Err(ValidationDefect::TrustedCheckDiagnostic {
                    command: record.command.clone(),
                });
            }
            let sources = sources(record);
            if sources.is_empty() {
                return Err(unevidenced(record, "sources"));
            }
            if context.scope.is_empty() {
                return Err(ValidationDefect::ScopeUnknown {
                    command: record.command.clone(),
                });
            }
            if let Some(source) = sources
                .iter()
                .find(|source| in_scope(source, context.scope))
            {
                return Err(ValidationDefect::DiagnosticInScope {
                    command: record.command.clone(),
                    source: (*source).to_string(),
                });
            }
            if passes_as_required(record, records) {
                return Err(ValidationDefect::CheckContradicted {
                    command: record.command.clone(),
                    role: record.role,
                });
            }
            Ok(())
        }
        ValidationOutcome::Denied | ValidationOutcome::NotRun => Err(contradiction(record)),
    }
}

/// Whether `record` is a check the owner trusts: a captured required command
/// or `review.baseline_commands` entry, by the host-command identity rule.
fn trusted(record: &ReviewValidation, context: &ValidationContext<'_>) -> bool {
    context
        .required_validation_commands
        .unwrap_or_default()
        .iter()
        .chain(context.baseline_commands)
        .any(|command| same_host_command(record, command))
}

/// Whether the final records still account for a retained required check:
/// the same check is recorded `required` (it must then pass) or `superseded`
/// (it must then be replaced by a required pass), or it is `excluded` and the
/// retained record never ran either. A diagnostic, a negative control, or no
/// record at all leaves the obligation dropped.
fn obligation_resolved(obligation: &ReviewValidation, records: &[ReviewValidation]) -> bool {
    records
        .iter()
        .filter(|record| same_check(obligation, record))
        .any(|record| match record.role {
            ValidationRole::Required | ValidationRole::Superseded => true,
            ValidationRole::Excluded => matches!(
                obligation.outcome,
                ValidationOutcome::NotRun | ValidationOutcome::Denied
            ),
            ValidationRole::ExpectedFailure | ValidationRole::Diagnostic => false,
        })
}

/// How many records carry each classification, for readable disclosure.
pub fn validation_role_counts(records: &[ReviewValidation]) -> Vec<(ValidationRole, usize)> {
    [
        ValidationRole::Required,
        ValidationRole::ExpectedFailure,
        ValidationRole::Excluded,
        ValidationRole::Superseded,
        ValidationRole::Diagnostic,
    ]
    .into_iter()
    .filter_map(|role| {
        let count = records.iter().filter(|record| record.role == role).count();
        (count > 0).then_some((role, count))
    })
    .collect()
}

/// What a set of records does not establish about the candidate, for
/// disclosure beside `validation_complete`: each failed diagnostic, with
/// where its failures lie.
pub fn validation_limitations(records: &[ReviewValidation]) -> Vec<String> {
    records
        .iter()
        .filter(|record| {
            record.role == ValidationRole::Diagnostic && record.outcome != ValidationOutcome::Passed
        })
        .map(|record| {
            let sources = sources(record);
            if sources.is_empty() {
                format!(
                    "diagnostic `{}` {}",
                    record.command,
                    record.outcome.as_str()
                )
            } else {
                format!(
                    "diagnostic `{}` {} in {}",
                    record.command,
                    record.outcome.as_str(),
                    sources.join(", ")
                )
            }
        })
        .collect()
}

fn unevidenced(record: &ReviewValidation, missing: &'static str) -> ValidationDefect {
    ValidationDefect::ClassificationUnevidenced {
        command: record.command.clone(),
        role: record.role,
        missing,
    }
}

/// The record's non-empty sources, trimmed.
fn sources(record: &ReviewValidation) -> Vec<&str> {
    record
        .sources
        .iter()
        .map(|source| source.trim())
        .filter(|source| !source.is_empty())
        .collect()
}

/// Whether `source` overlaps any scope selector. A bare path reads as the
/// `file:` selector of that path.
pub fn in_scope(source: &str, scope: &[String]) -> bool {
    let source = if ["file:", "dir:", "symbol:"]
        .iter()
        .any(|kind| source.starts_with(kind))
    {
        source.to_string()
    } else {
        format!("file:{}", source.trim_start_matches("./"))
    };
    scope.iter().any(|selector| overlaps(selector, &source))
}

/// Whether a required record of the same check passed.
///
/// Required passes describe the final candidate, so their position in the
/// report does not affect replacement. Effective identities remain strict:
/// a non-empty `check`, otherwise the normalized command. Different identities
/// never match, even when their commands match or one check claims broader coverage.
fn passes_as_required(record: &ReviewValidation, records: &[ReviewValidation]) -> bool {
    records.iter().any(|other| {
        other.role == ValidationRole::Required
            && other.outcome == ValidationOutcome::Passed
            && same_check(record, other)
    })
}

fn contradiction(record: &ReviewValidation) -> ValidationDefect {
    ValidationDefect::RoleContradicted {
        command: record.command.clone(),
        role: record.role,
        outcome: record.outcome,
    }
}

fn explained(record: &ReviewValidation) -> bool {
    record
        .note
        .as_deref()
        .is_some_and(|note| !note.trim().is_empty())
}

/// One check: both records carry the same record id, or they share an
/// effective identity.
///
/// Without a shared record id, identical commands are not enough when either
/// record has a non-empty `check`: that identity takes precedence. For example,
/// a superseded `cargo test -p x` with `check: "unit"` is not replaced by a
/// required pass of `cargo test -p x` that omits `check`. The replacement must
/// carry `check: "unit"` or normalize to the command `unit`. Conversely, an
/// explicit `check: "cargo test -p x"` matches that command without a check.
/// This rule also governs retained obligations and diagnostic contradictions.
fn same_check(left: &ReviewValidation, right: &ReviewValidation) -> bool {
    if matches!(
        (left.record_id(), right.record_id()),
        (Some(left), Some(right)) if left == right
    ) {
        return true;
    }
    matches!(
        (effective_check_identity(left), effective_check_identity(right)),
        (Some(left), Some(right)) if left == right
    )
}

/// The explicit identity takes precedence over the normalized command.
fn effective_check_identity(record: &ReviewValidation) -> Option<String> {
    check_identity(record)
        .map(str::to_owned)
        .or_else(|| normalized_command(record))
}

/// A present, non-empty `check` identity.
fn check_identity(record: &ReviewValidation) -> Option<&str> {
    record
        .check
        .as_deref()
        .map(str::trim)
        .filter(|check| !check.is_empty())
}

/// Whether `record` is the host-required command.
///
/// Whitespace and leading POSIX environment assignments do not make a
/// different check. The command that remains is compared in full. A
/// reviewer may also set `check` to the host command string when the run
/// is wrapped in any other way (`env`, a shell prefix); that identity is
/// the host command.
pub fn same_host_command(record: &ReviewValidation, host_command: &str) -> bool {
    let same_command = matches!(
        (
            normalized_command(record),
            normalize_command_text(host_command),
        ),
        (Some(left), Some(right)) if left == right
    );
    let same_identity = check_identity(record).is_some_and(|check| check == host_command.trim());
    same_command || same_identity
}

/// The command with whitespace runs collapsed and leading POSIX environment
/// assignments removed, so `TMPDIR="$PWD/.orbit/tmp" make ci-fast` and
/// `make  ci-fast` are one check. `make ci-fast-extra` and `FOO=1 make other`
/// stay different checks.
fn normalized_command(record: &ReviewValidation) -> Option<String> {
    normalize_command_text(&record.command)
}

fn normalize_command_text(command: &str) -> Option<String> {
    let command = strip_leading_env_assignments(command)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!command.is_empty()).then_some(command)
}

/// Drop leading `NAME=value` words. A quoted value, including one with
/// spaces, stays part of the assignment word. The command text that remains
/// is returned unchanged so later whitespace collapsing matches the previous
/// comparison.
fn strip_leading_env_assignments(command: &str) -> &str {
    let mut rest = command;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return trimmed;
        }
        let (word, after) = first_shell_word(trimmed);
        if word.is_empty() || !is_env_assignment(word) {
            return trimmed;
        }
        rest = after;
    }
}

/// The first shell word and the text after it.
///
/// Quotes keep their contents, spaces included, in the word. A backslash
/// escapes the next character outside quotes and inside double quotes. The
/// raw word is returned, quotes included, because only assignment words are
/// discarded; the surviving command is collapsed separately.
fn first_shell_word(command: &str) -> (&str, &str) {
    let mut quote: Option<char> = None;
    let mut chars = command.char_indices();
    while let Some((idx, ch)) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if ch == '\\' {
                    chars.next();
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => {}
            None if ch.is_whitespace() => {
                return (&command[..idx], &command[idx..]);
            }
            None => match ch {
                '\'' => quote = Some('\''),
                '"' => quote = Some('"'),
                '\\' => {
                    chars.next();
                }
                _ => {}
            },
        }
    }
    (command, "")
}

/// A POSIX assignment word: an unquoted `NAME` immediately followed by `=`.
///
/// `NAME` is `[A-Za-z_][A-Za-z0-9_]*`. The value may be quoted. A word whose
/// name is quoted, or that has no `=`, is a command word.
fn is_env_assignment(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_posix_name_start(first) {
        return false;
    }
    for ch in chars {
        if is_posix_name_continue(ch) {
            continue;
        }
        return ch == '=';
    }
    false
}

fn is_posix_name_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_posix_name_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}
