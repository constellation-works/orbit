//! Invocation trace records: inserts, list/accounting queries, and row hydration.

mod hydrate;
mod insert;
mod query;

#[cfg(test)]
mod tests;
