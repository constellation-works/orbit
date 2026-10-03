//! Invocation trace records: inserts, list/accounting queries, and row hydration.

mod hydrate;
mod insert;
mod query;

#[cfg(test)]
pub(crate) use insert::INVOCATION_INSERT_COLUMNS;
