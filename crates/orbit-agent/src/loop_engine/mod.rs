//! Audit blob sinks for runtime persistence.

pub mod audit;

pub use audit::{AuditSink, BlobStore, InMemorySink, NullSink};
