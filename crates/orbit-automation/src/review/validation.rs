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

use orbit_common::fs::selector::overlaps;
use orbit_types::workflow::{
    RetainedObligation, ReviewValidation, ValidationOutcome, ValidationRole,
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
    /// A superseded attempt that no later required check replaced, so the
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
    /// A failed diagnostic whose source lies inside the candidate's scope:
    /// that failure is the task's own and blocks like a required check.
    DiagnosticInScope { command: String, source: String },
    /// A failed diagnostic judged with no recorded scope, so nothing shows
    /// its failures are unrelated.
    ScopeUnknown { command: String },
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
    /// The certificate has no captured owner validation policy. It must be
    /// re-established under a fresh delivery admission.
    HostContractMissing,
    /// A captured host-required command is absent, reclassified, or has no
    /// valid required replacement on the final candidate.
    HostCheckNotEstablished { command: String },
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
                 the candidate's scope; record an unrelated failure as diagnostic"
            ),
            ValidationDefect::DiagnosticInScope { command, source } => format!(
                "validation_incomplete: diagnostic `{command}` failed in `{source}`, inside the \
                 candidate's scope, so it is a required failure"
            ),
            ValidationDefect::ScopeUnknown { command } => format!(
                "validation_incomplete: failed diagnostic `{command}` has no recorded candidate \
                 scope to show its failures are unrelated"
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
    /// Required commands captured by the candidate owner at delivery
    /// admission. `None` is a legacy/ambiguous contract; `Some([])` is an
    /// explicit empty host contract.
    pub required_validation_commands: Option<&'a [String]>,
}

impl Default for ValidationContext<'_> {
    fn default() -> Self {
        Self {
            scope: &[],
            obligations: &[],
            required_validation_commands: Some(&[]),
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
/// unperformed; a superseded attempt must be followed by the required check
/// that replaced it — the same effective identity: a non-empty `check`,
/// otherwise the command with whitespace and leading `NAME=value`
/// assignments normalized; a diagnostic must be an
/// observation that ran, and a failed one must name sources all outside the
/// scope and not share its check with a required pass. Every classification
/// other than `required` must explain itself, so an unexplained
/// reclassification is refused rather than trusted. Every retained
/// obligation must still be accounted for by a record of the same check that
/// is `required`, `superseded`, or — when the retained record never ran —
/// `excluded`. Records carrying no classification are required checks, which
/// keeps evidence written before this contract conservative.
pub fn validation_evidence(
    records: &[ReviewValidation],
    context: &ValidationContext<'_>,
) -> Result<(), ValidationDefect> {
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
            ValidationRole::ExpectedFailure => negative_control(record, records, context)?,
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
            ValidationRole::Diagnostic => diagnostic(record, records, context)?,
            _ => {}
        }
    }

    for obligation in context.obligations {
        let obligation = &obligation.validation;
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
        let established = records.iter().enumerate().any(|(index, record)| {
            same_host_command(record, command)
                && match record.role {
                    ValidationRole::Required => record.outcome == ValidationOutcome::Passed,
                    ValidationRole::Superseded => {
                        replaced_by_required_check(record, &records[index + 1..])
                    }
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

/// A negative control: it failed, names its kind and the code it exercises,
/// that code is the candidate's own, and a control run on the candidate does
/// not share its check with a required pass there.
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

/// A diagnostic is an observation that ran. A failed one names where its
/// failures lie, every place outside the candidate's scope, and does not
/// share its check with a required pass on the same candidate.
fn diagnostic(
    record: &ReviewValidation,
    records: &[ReviewValidation],
    context: &ValidationContext<'_>,
) -> Result<(), ValidationDefect> {
    match record.outcome {
        ValidationOutcome::Passed => Ok(()),
        ValidationOutcome::Failed => {
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
fn in_scope(source: &str, scope: &[String]) -> bool {
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

/// Whether a later record is the required check the superseded attempt was
/// replaced by. Order carries the meaning: a supersession must be resolved
/// after it, never by a check recorded before it. The later record must be
/// the same check: its effective identity is a non-empty `check`, otherwise
/// its command (whitespace-normalized, with leading POSIX environment
/// assignments removed). An explicit identity can match another record's
/// normalized command, including when a wrapper changed the command text.
/// Different effective identities never match, even with identical commands.
/// Any later required pass is not enough.
fn replaced_by_required_check(superseded: &ReviewValidation, later: &[ReviewValidation]) -> bool {
    later.iter().any(|record| {
        record.role == ValidationRole::Required
            && record.outcome == ValidationOutcome::Passed
            && same_check(superseded, record)
    })
}

fn same_check(left: &ReviewValidation, right: &ReviewValidation) -> bool {
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
fn same_host_command(record: &ReviewValidation, host_command: &str) -> bool {
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
