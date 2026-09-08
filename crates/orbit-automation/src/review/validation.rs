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

use orbit_types::workflow::{ReviewValidation, ValidationOutcome, ValidationRole};

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
    /// A superseded attempt that no later required check replaced, so the
    /// diagnostic never reached a final-candidate outcome.
    SupersededWithoutReplacement { command: String },
    /// A classification other than `required` with nothing explaining it.
    ClassificationUnexplained {
        command: String,
        role: ValidationRole,
    },
}

impl ValidationDefect {
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
                "validation_incomplete: superseded attempt `{command}` is followed by no related \
                 required check that passed on the final candidate"
            ),
            ValidationDefect::ClassificationUnexplained { command, role } => format!(
                "validation_unexplained: `{command}` is recorded as {} with no note explaining it",
                role.as_str()
            ),
        }
    }
}

/// Read a reviewer's validation records as evidence about the final
/// candidate.
///
/// Every required check must have passed and at least one must exist; a
/// declared negative control must have failed; an excluded action must have
/// stayed unperformed; a superseded attempt must be followed by the required
/// check that replaced it — the same command, or the same non-empty `check`
/// identity. Every classification other than `required` must explain itself,
/// so an unexplained reclassification is refused rather than trusted. Records
/// carrying no classification are required checks, which keeps evidence
/// written before this contract conservative.
pub fn validation_evidence(records: &[ReviewValidation]) -> Result<(), ValidationDefect> {
    let mut required_passed = false;

    for (index, record) in records.iter().enumerate() {
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
            ValidationRole::ExpectedFailure if record.outcome != ValidationOutcome::Failed => {
                return Err(contradiction(record));
            }
            ValidationRole::Excluded
                if !matches!(
                    record.outcome,
                    ValidationOutcome::NotRun | ValidationOutcome::Denied
                ) =>
            {
                return Err(contradiction(record));
            }
            ValidationRole::Superseded
                if !replaced_by_required_check(record, &records[index + 1..]) =>
            {
                return Err(ValidationDefect::SupersededWithoutReplacement {
                    command: record.command.clone(),
                });
            }
            _ => {}
        }
    }

    if required_passed {
        Ok(())
    } else {
        Err(ValidationDefect::NoRequiredCheck)
    }
}

/// How many records carry each classification, for readable disclosure.
pub fn validation_role_counts(records: &[ReviewValidation]) -> Vec<(ValidationRole, usize)> {
    [
        ValidationRole::Required,
        ValidationRole::ExpectedFailure,
        ValidationRole::Excluded,
        ValidationRole::Superseded,
    ]
    .into_iter()
    .filter_map(|role| {
        let count = records.iter().filter(|record| record.role == role).count();
        (count > 0).then_some((role, count))
    })
    .collect()
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

/// Whether a later record is the required check the superseded attempt was
/// replaced by. Order carries the meaning: a supersession must be resolved
/// after it, never by a check recorded before it. The later record must
/// name the same check: the same command when both records have no `check`,
/// or the same non-empty `check` identity when both records provide one and
/// the command or environment was corrected. Check identities and commands
/// are separate namespaces, so one cannot impersonate the other. Any later
/// required pass is not enough.
fn replaced_by_required_check(superseded: &ReviewValidation, later: &[ReviewValidation]) -> bool {
    let Some(identity) = replacement_identity(superseded) else {
        return false;
    };
    later.iter().any(|record| {
        record.role == ValidationRole::Required
            && record.outcome == ValidationOutcome::Passed
            && replacement_identity(record) == Some(identity)
    })
}

/// The namespaced identity a superseded attempt and its replacement share.
///
/// A present `check` is the identity when it is non-empty after trim;
/// otherwise the command string is the identity, so a same-command rerun
/// still binds. These forms deliberately remain distinct even when their
/// strings are equal. An empty or whitespace-only `check` is invalid and
/// matches nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacementIdentity<'a> {
    Check(&'a str),
    Command(&'a str),
}

fn replacement_identity(record: &ReviewValidation) -> Option<ReplacementIdentity<'_>> {
    match record.check.as_deref() {
        Some(value) => {
            let trimmed = value.trim();
            (!trimmed.is_empty()).then_some(ReplacementIdentity::Check(trimmed))
        }
        None => {
            let command = record.command.as_str();
            (!command.trim().is_empty()).then_some(ReplacementIdentity::Command(command))
        }
    }
}
