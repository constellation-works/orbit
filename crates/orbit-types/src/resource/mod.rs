//! Domain contracts for this Orbit types module.

mod data;
mod error;
pub use error::ResourceError;

pub use data::{
    EXECUTOR_RESOURCE_SCHEMA_VERSION, ExecutorResource, ExecutorResourceSpec,
    POLICY_RESOURCE_SCHEMA_VERSION, PolicyResource, PolicyResourceSpec, ResourceEnvelope,
    ResourceHeader, ResourceKind, ResourceMetadata, validate_resource_name,
};
