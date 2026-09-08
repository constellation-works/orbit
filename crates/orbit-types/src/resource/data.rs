use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::policy::FsProfile;
use crate::resource::ResourceError;
use crate::workflow::{ExecutorSandboxKind, ExecutorType, ModelPairOverride, StdoutFormat};

pub const EXECUTOR_RESOURCE_SCHEMA_VERSION: u32 = 2;
pub const POLICY_RESOURCE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResourceKind {
    Job,
    Activity,
    Policy,
    Executor,
}

impl std::fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResourceKind::Job => write!(f, "Job"),
            ResourceKind::Activity => write!(f, "Activity"),
            ResourceKind::Policy => write!(f, "Policy"),
            ResourceKind::Executor => write!(f, "Executor"),
        }
    }
}

impl std::str::FromStr for ResourceKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "job" | "jobs" => Ok(ResourceKind::Job),
            "activity" | "activities" => Ok(ResourceKind::Activity),
            "policy" | "policies" => Ok(ResourceKind::Policy),
            "executor" | "executors" => Ok(ResourceKind::Executor),
            _ => Err(format!("unknown resource kind: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub labels: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub annotations: HashMap<String, String>,
}

impl ResourceMetadata {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            labels: HashMap::new(),
            annotations: HashMap::new(),
        }
    }

    pub fn validate_name(&self) -> Result<(), ResourceError> {
        validate_resource_name(&self.name)
    }
}

pub fn validate_resource_name(name: &str) -> Result<(), ResourceError> {
    if name.is_empty() {
        return invalid_resource_name(name, "must not be empty");
    }

    if name.trim() != name {
        return invalid_resource_name(name, "must not have leading or trailing whitespace");
    }

    if name.starts_with('.') {
        return invalid_resource_name(name, "must not start with `.`");
    }

    if name.contains("..") {
        return invalid_resource_name(name, "must not contain `..`");
    }

    if name
        .chars()
        .any(|ch| matches!(ch, '/' | '\\' | ':' | '\0') || ch.is_control())
    {
        return invalid_resource_name(
            name,
            "must not contain separators, drive prefixes, or control characters",
        );
    }

    if std::path::Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        != Some(name)
    {
        return invalid_resource_name(name, "must be a single file stem");
    }

    Ok(())
}

fn invalid_resource_name(name: &str, reason: &str) -> Result<(), ResourceError> {
    Err(ResourceError::Invalid(format!(
        "invalid resource name {name:?}: {reason}"
    )))
}

/// Header-only parse for routing `orbit apply` to the right store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceHeader {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub kind: ResourceKind,
    pub metadata: ResourceMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceEnvelope<T> {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub kind: ResourceKind,
    pub metadata: ResourceMetadata,
    pub spec: T,
}

impl<T> ResourceEnvelope<T> {
    pub fn new(schema_version: u32, kind: ResourceKind, name: impl Into<String>, spec: T) -> Self {
        Self {
            schema_version,
            kind,
            metadata: ResourceMetadata::named(name),
            spec,
        }
    }

    pub fn header(&self) -> ResourceHeader {
        ResourceHeader {
            schema_version: self.schema_version,
            kind: self.kind.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PolicyResourceSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "denyRead", default, skip_serializing_if = "Vec::is_empty")]
    pub deny_read: Vec<String>,
    #[serde(rename = "denyModify", default, skip_serializing_if = "Vec::is_empty")]
    pub deny_modify: Vec<String>,
    #[serde(
        rename = "fsProfiles",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub fs_profiles: HashMap<String, FsProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutorResourceSpec {
    pub executor_type: ExecutorType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_format: Option<StdoutFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_pair_override: Option<ModelPairOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_flag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<ExecutorSandboxKind>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_fallback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub type PolicyResource = ResourceEnvelope<PolicyResourceSpec>;
pub type ExecutorResource = ResourceEnvelope<ExecutorResourceSpec>;
