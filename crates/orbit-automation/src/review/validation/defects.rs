//! Validation defects and the corrections their classifications need.

use orbit_types::workflow::{RecordGap, ReviewValidation, ValidationOutcome, ValidationRole};

use super::command_identity::same_host_command;
use super::evidence::ValidationContext;

/// [ORB-15083] What a command the reviewer skipped is to the owner, which
/// decides the correction that keeps the skip honest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkippedCheck {
    /// A captured `workflow.required_validation_commands` entry: it needs a
    /// passing `required` record, so only running it settles.
    HostRequired,
    /// A `review.baseline_commands` entry: the reviewer may leave it unrun,
    /// recorded `excluded` or omitted.
    Baseline,
    /// Neither list names it. If repository policy requires it (local Rust
    /// CodeQL), it is never `excluded`: it is run, or owed as external
    /// evidence for the owner to fulfil.
    Unlisted,
}

impl SkippedCheck {
    /// The correction this class of skipped command takes.
    fn remedy(self) -> &'static str {
        match self {
            SkippedCheck::HostRequired => {
                "it is a host-required check, so run it and record it `required`; \
                 `excluded` never establishes one"
            }
            SkippedCheck::Baseline => {
                "it is a baseline command, so record it `excluded` or omit it"
            }
            SkippedCheck::Unlisted => {
                "if repository policy requires this check (for example local Rust CodeQL), \
                 run it, or name it in `external_evidence` so the owner fulfils it, keeping \
                 its record `required` and `not_run`; never record it `excluded`, which drops \
                 the coverage. Otherwise omit it"
            }
        }
    }
}

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
    /// [ORB-14334] A required check whose run passed only by deferring a
    /// sandbox-confined path, so the path never executed.
    RequiredDeferred { command: String, notice: String },
    /// The outcome contradicts the classification the record was filed
    /// under: a negative control that passed, an excluded action that ran.
    RoleContradicted {
        command: String,
        role: ValidationRole,
        outcome: ValidationOutcome,
    },
    /// [ORB-15083] A diagnostic that never ran. A diagnostic is an observation,
    /// so `not_run` contradicts the role; the record is really a skipped check,
    /// and where a skipped check goes depends on `class`.
    DiagnosticNotRun {
        command: String,
        class: SkippedCheck,
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
    /// [ORB-16447] A file a control mutated that the review's own repair
    /// changed, or for which the record declares a repaired blob, whose
    /// final blob is not the one the record declares (`declared`; `None`
    /// when it declares none). `found` is the final candidate's blob, `None`
    /// when the file is gone there.
    RepairedTargetChanged {
        command: String,
        target: String,
        declared: Option<String>,
        found: Option<String>,
    },
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
    /// repository-relative path. A diagnostic recorded `not_run` is likewise a
    /// skipped check under the wrong role [ORB-15083]; its correction is a
    /// relabel, an omission or an owed-evidence entry. The reviewer can correct
    /// such a report without rerunning anything, so it is returned to the
    /// reviewer once before the verdict settles.
    pub fn correctable(&self) -> bool {
        matches!(
            self,
            ValidationDefect::ClassificationUnexplained { .. }
                | ValidationDefect::ClassificationUnevidenced { .. }
                | ValidationDefect::ControlOutOfScope { .. }
                | ValidationDefect::MutationTargetInvalid { .. }
                | ValidationDefect::DiagnosticNotRun { .. }
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
            ValidationDefect::RequiredDeferred { command, notice } => format!(
                "validation_incomplete: required check `{command}` passed only by deferring a \
                 sandbox-confined path (`{notice}`), so that path never executed; name it as \
                 `host_sandbox_test` evidence for a host that can run it"
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
            ValidationDefect::DiagnosticNotRun { command, class } => format!(
                "validation_contradicted: `{command}` was recorded as diagnostic but is not_run; \
                 a diagnostic is an observation that ran, so {}",
                class.remedy()
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
            ValidationDefect::RepairedTargetChanged {
                command,
                target,
                declared: None,
                ..
            } => format!(
                "validation_contradicted: control `{command}` mutated `{target}`, which this \
                 review's own repair changed, but declares no `repaired_blob` for it, so its \
                 restoration to the repaired state cannot be checked; record the target's \
                 `git hash-object` blob before a control mutates a repaired file"
            ),
            ValidationDefect::RepairedTargetChanged {
                command,
                target,
                declared: Some(declared),
                found,
            } => format!(
                "validation_contradicted: control `{command}` mutated `{target}`, which the \
                 final candidate carries as {}, not as the repaired blob `{declared}` the \
                 record declares; the mutation was not restored to the repaired state",
                found
                    .as_deref()
                    .map_or_else(|| "no file".to_string(), |blob| format!("blob `{blob}`"))
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

/// [ORB-15083] What the owner makes of a skipped command: a captured required
/// command wins over a baseline entry, and anything else is unlisted.
pub(super) fn skipped_check(
    record: &ReviewValidation,
    context: &ValidationContext<'_>,
) -> SkippedCheck {
    let host_required = context
        .required_validation_commands
        .unwrap_or_default()
        .iter()
        .any(|command| same_host_command(record, command));
    if host_required {
        SkippedCheck::HostRequired
    } else if context
        .baseline_commands
        .iter()
        .any(|command| same_host_command(record, command))
    {
        SkippedCheck::Baseline
    } else {
        SkippedCheck::Unlisted
    }
}

pub(super) fn unevidenced(record: &ReviewValidation, missing: &'static str) -> ValidationDefect {
    ValidationDefect::ClassificationUnevidenced {
        command: record.command.clone(),
        role: record.role,
        missing,
    }
}

pub(super) fn contradiction(record: &ReviewValidation) -> ValidationDefect {
    ValidationDefect::RoleContradicted {
        command: record.command.clone(),
        role: record.role,
        outcome: record.outcome,
    }
}
