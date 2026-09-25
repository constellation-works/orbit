//! Repository security sweep: file Dependabot, code scanning and secret
//! scanning alerts as backlog tasks, grouping code scanning alerts that share
//! one repair.

pub(super) mod code_groups;
pub(super) mod consolidate;
mod duplicates;
pub(super) mod filing;

#[cfg(test)]
pub(super) mod tests;
