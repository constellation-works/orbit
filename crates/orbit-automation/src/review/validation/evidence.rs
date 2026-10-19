//! Judge validation records against the candidate and captured owner policy.

use orbit_types::workflow::{
    HostKnownRed, RetainedObligation, RetiredValidation, ReviewValidation, ValidationOutcome,
    ValidationRole, record_gap,
};

use super::command_identity::{same_check, same_host_command};
use super::defects::{ValidationDefect, contradiction, skipped_check, unevidenced};
use super::mutation_paths::in_scope;

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
    /// [ORB-15546] Failed required checks the host verified known red on
    /// the final candidate. A failed required record one of them names is
    /// established without its outcome changing.
    pub known_red: &'a [HostKnownRed],
}

impl Default for ValidationContext<'_> {
    fn default() -> Self {
        Self {
            scope: &[],
            obligations: &[],
            retired: &[],
            required_validation_commands: Some(&[]),
            baseline_commands: &[],
            known_red: &[],
        }
    }
}

/// Read a reviewer's validation records as evidence about the final
/// candidate.
///
/// Every required check must have passed, without deferring a confined path,
/// or failed only as the host verified known red ([`ValidationContext::known_red`]),
/// and at least one must exist; a
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
                if host_known_red(record, context) {
                    required_passed = true;
                    continue;
                }
                if record.outcome != ValidationOutcome::Passed {
                    return Err(ValidationDefect::RequiredNotPassed {
                        command: record.command.clone(),
                        outcome: record.outcome,
                    });
                }
                if let Some(notice) = record.deferral() {
                    return Err(ValidationDefect::RequiredDeferred {
                        command: record.command.clone(),
                        notice: notice.to_string(),
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
                    ValidationRole::Required => {
                        record.executed_outcome() == ValidationOutcome::Passed
                            || host_known_red(record, context)
                    }
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

/// [ORB-15546] Whether the host verified `record`'s failure known red: it is
/// a failed required record carrying a baseline claim, and a
/// [`HostKnownRed`] entry naming at least one test carries its id and its
/// host command. Whoever records the entry binds it to the base and final
/// candidate ([`crate::review::certificate_acceptable`]).
fn host_known_red(record: &ReviewValidation, context: &ValidationContext<'_>) -> bool {
    record.role == ValidationRole::Required
        && record.outcome == ValidationOutcome::Failed
        && record.baseline.is_some()
        && context.known_red.iter().any(|known| {
            known.record_id.as_deref() == record.record_id()
                && !known.validation.failures.identities.is_empty()
                && same_host_command(record, &known.validation.command)
        })
}

/// A negative control: it failed, names its kind and the code it exercises,
/// that code is the candidate's own, and a control run on the candidate does
/// not share its check with a required pass there. A counterfactual's
/// `mutation_target` is not a source: it may lie anywhere in the
/// repository, and settlement judges its restoration ([`super::mutation_targets`]).
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

/// A diagnostic is an observation that ran; a `not_run` one is a skipped
/// check under the wrong role, returned to the reviewer with the correction
/// its class takes [ORB-15083]. A failed one is no check the
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
        ValidationOutcome::NotRun => Err(ValidationDefect::DiagnosticNotRun {
            command: record.command.clone(),
            class: skipped_check(record, context),
        }),
        ValidationOutcome::Denied => Err(contradiction(record)),
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

/// The record's non-empty sources, trimmed.
pub(super) fn sources(record: &ReviewValidation) -> Vec<&str> {
    record
        .sources
        .iter()
        .map(|source| source.trim())
        .filter(|source| !source.is_empty())
        .collect()
}

/// Whether a required record of the same check passed, executing every
/// path it covers (a deferred-only pass does not).
///
/// Required passes describe the final candidate, so their position in the
/// report does not affect replacement. Effective identities remain strict:
/// a non-empty `check`, otherwise the normalized command. Different identities
/// never match, even when their commands match or one check claims broader coverage.
fn passes_as_required(record: &ReviewValidation, records: &[ReviewValidation]) -> bool {
    records.iter().any(|other| {
        other.role == ValidationRole::Required
            && other.executed_outcome() == ValidationOutcome::Passed
            && same_check(record, other)
    })
}

fn explained(record: &ReviewValidation) -> bool {
    record
        .note
        .as_deref()
        .is_some_and(|note| !note.trim().is_empty())
}
