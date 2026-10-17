//! Pure domain contracts exercised through the exported types API.
//!
//! Plugin, task, tool and workflow cases share one binary to pay one link cost;
//! workspace publication, digest and deferred-sandbox cases are included below.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod plugin;
mod task;
mod tool;
mod workflow;

// Workspace publication, digest and deferred-sandbox cases join this
// binary. Their test paths are unchanged; only the Cargo target name is.
use orbit_types::identity::lower_hex;
use orbit_types::workflow::BUBBLEWRAP_DEFERRAL_PREFIX;
use orbit_types::workflow::BUBBLEWRAP_NAMESPACE_PROBE;
use orbit_types::workflow::BUBBLEWRAP_NAMESPACE_REFUSAL;
use orbit_types::workflow::DeferralLane;
use orbit_types::workflow::DeferredSandboxValidation;
use orbit_types::workflow::EvidenceHostOs;
use orbit_types::workflow::HostValidationGates;
use orbit_types::workflow::NamespaceProbe;
use orbit_types::workflow::PS_DENIED_DEFERRAL_PREFIX;
use orbit_types::workflow::PS_EXEC_PROBE;
use orbit_types::workflow::SEATBELT_APPLY_PROBE;
use orbit_types::workflow::SEATBELT_DEFERRAL_PREFIX;
use orbit_types::workflow::ValidationGateSource;
use orbit_types::workflow::ValidationGates;
use orbit_types::workflow::deferral_handoff;
use orbit_types::workspace::WorkspacePublicationBinding;
use orbit_types::workspace::git_remote_identity;
use orbit_types::workspace::git_remote_network_host;
use orbit_types::workspace::redact_git_remote;
use orbit_types::workspace::validate_publication_remote;
use orbit_types::workspace::validate_source_repository_fingerprint;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;

include!("deferred_sandbox.rs");
include!("digest.rs");
include!("workspace.rs");
