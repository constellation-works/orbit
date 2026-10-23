//! Readable disclosure of validation roles and limitations.

use orbit_types::workflow::{ReviewValidation, ValidationOutcome, ValidationRole};

use super::evidence::sources;

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
