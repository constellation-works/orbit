//! Generic resource envelope, header, and executor/policy resource contracts; imports `policy`, `task`, `timestamp`, and `workflow` within `orbit-types` only (I/O-free).

mod data;
mod error;
pub use error::ResourceError;

pub use data::{
    EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource, ExecutorResourceSpec,
    POLICY_RESOURCE_SCHEMA_VERSION, PolicyResource, PolicyResourceSpec, ResourceEnvelope,
    ResourceHeader, ResourceKind, ResourceMetadata, validate_resource_name,
};
