//! Revision-based review size accounting; no attribute/generated exclusions.

use super::Source;
use orbit_automation::AutomationError;
use orbit_types::workflow::automation::{ReviewWorkload, SourceRevision};
use std::collections::BTreeSet;

impl Source<'_> {
    pub(in crate::application::automation) fn review_workload(
        &self,
        from: &SourceRevision,
        through: &SourceRevision,
    ) -> Result<ReviewWorkload, AutomationError> {
        // Verify trees as well as commits before measuring immutable objects.
        if self.revision(&from.commit)? != *from || self.revision(&through.commit)? != *through {
            return Err(AutomationError::Refused(
                "review_workload_revision_mismatch".into(),
            ));
        }
        let range = format!("{}..{}", from.commit, through.commit);
        let commits = self.git(&[
            "rev-list",
            "--first-parent",
            "--parents",
            "--reverse",
            &range,
        ])?;
        // Feed explicit (commit, first parent) pairs to one diff-tree process.
        // This includes merge changes exactly once and avoids one process per
        // commit exhausting the source deadline on an otherwise small range.
        let mut input = String::new();
        for row in commits.lines() {
            let mut fields = row.split_whitespace();
            let commit = fields
                .next()
                .ok_or_else(|| AutomationError::Refused("review_workload_missing_commit".into()))?;
            let parent = fields.next().ok_or_else(|| {
                AutomationError::Refused("review_workload_missing_first_parent".into())
            })?;
            input.push_str(commit);
            input.push(' ');
            input.push_str(parent);
            input.push('\n');
        }
        let stats = self.git_with_input(
            &[
                "diff-tree",
                "--stdin",
                "-r",
                "--numstat",
                "--no-commit-id",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "-z",
            ],
            input.as_bytes(),
        )?;
        let mut paths = BTreeSet::new();
        let mut binary = BTreeSet::new();
        let mut lines = 0u64;
        for record in stats.split('\0').filter(|record| !record.is_empty()) {
            let mut fields = record.splitn(3, '\t');
            let added = fields.next().unwrap_or_default();
            let deleted = fields.next().unwrap_or_default();
            let path = fields.next().ok_or_else(|| {
                AutomationError::Refused("review_workload_invalid_numstat".into())
            })?;
            paths.insert(path.to_string());
            if added == "-" || deleted == "-" {
                binary.insert(path.to_string());
            } else {
                for value in [added, deleted] {
                    let count = value.parse::<u64>().map_err(|_| {
                        AutomationError::Refused("review_workload_invalid_numstat".into())
                    })?;
                    lines = lines.checked_add(count).ok_or_else(|| {
                        AutomationError::Refused("review_workload_overflow".into())
                    })?;
                }
            }
        }
        Ok(ReviewWorkload {
            files: u32::try_from(paths.len())
                .map_err(|_| AutomationError::Refused("review_workload_overflow".into()))?,
            lines,
            binary_files: u32::try_from(binary.len())
                .map_err(|_| AutomationError::Refused("review_workload_overflow".into()))?,
        })
    }
}
