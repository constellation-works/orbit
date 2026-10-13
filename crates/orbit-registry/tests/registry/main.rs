//! Identity, publication and registry persistence through the public registry API.
//!
//! One integration-test binary for these surfaces. The two admitted migration
//! writer-fault and lock-interleaving harnesses stay beside their private
//! callbacks in `src/tests/workspace_registry.rs`.

#![allow(missing_docs, clippy::expect_used)]

mod machine_identity;
mod workspace_publication;
mod workspace_registry;
