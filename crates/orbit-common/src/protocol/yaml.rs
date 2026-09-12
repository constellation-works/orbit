use orbit_types::resource::{
    POLICY_RESOURCE_SCHEMA_VERSION, PolicyResource, ResourceHeader, ResourceKind,
};
use orbit_types::task::TaskPlan;
use orbit_types::workflow::{
    AUTO_TASK_SCHEMA_VERSION, AutoTaskDefinition, ROUTINE_SCHEMA_VERSION, RoutineDefinition,
    SchemaHeader,
};

use crate::error::OrbitError;

pub fn parse_task_plan(raw: &str, label: &str) -> Result<TaskPlan, OrbitError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !looks_like_structured_task_plan(trimmed) {
        return Ok(TaskPlan::default());
    }
    serde_yaml::from_str::<TaskPlan>(trimmed)
        .map_err(|error| OrbitError::InvalidInput(format!("failed to parse {label}: {error}")))
}

fn looks_like_structured_task_plan(raw: &str) -> bool {
    raw.contains("checkpoints:") || raw.contains("success_criteria:")
}

pub fn parse_auto_task_yaml(yaml: &str) -> Result<AutoTaskDefinition, OrbitError> {
    let header: SchemaHeader = serde_yaml::from_str(yaml)
        .map_err(|error| OrbitError::InvalidInput(format!("auto-task header: {error}")))?;
    if header.schema_version != AUTO_TASK_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "unsupported auto-task schemaVersion {} (this binary supports {})",
            header.schema_version, AUTO_TASK_SCHEMA_VERSION
        )));
    }
    let definition: AutoTaskDefinition = serde_yaml::from_str(yaml)
        .map_err(|error| OrbitError::InvalidInput(format!("auto-task: {error}")))?;
    definition.validate()?;
    Ok(definition)
}

pub fn parse_routine_yaml(yaml: &str) -> Result<RoutineDefinition, OrbitError> {
    let header: SchemaHeader = serde_yaml::from_str(yaml)
        .map_err(|error| OrbitError::InvalidInput(format!("routine header: {error}")))?;
    if header.schema_version != ROUTINE_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "unsupported routine schemaVersion {} (this binary supports {})",
            header.schema_version, ROUTINE_SCHEMA_VERSION
        )));
    }
    let definition: RoutineDefinition = serde_yaml::from_str(yaml)
        .map_err(|error| OrbitError::InvalidInput(format!("routine: {error}")))?;
    definition.validate()?;
    Ok(definition)
}

pub fn parse_policy_resource(yaml: &str, label: &str) -> Result<PolicyResource, OrbitError> {
    let header: ResourceHeader = serde_yaml::from_str(yaml)
        .map_err(|error| OrbitError::InvalidInput(format!("failed to parse {label}: {error}")))?;
    if header.kind != ResourceKind::Policy {
        return Err(OrbitError::InvalidInput(format!(
            "failed to parse {label}: expected kind Policy, found {}",
            header.kind
        )));
    }
    if header.schema_version == 1 {
        return Err(OrbitError::InvalidInput(format!(
            "failed to parse {label}: policy schemaVersion 1 is no longer supported; migrate to schemaVersion 2 with `spec.denyRead`, `spec.denyModify`, and `spec.fsProfiles`"
        )));
    }
    if header.schema_version != POLICY_RESOURCE_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "failed to parse {label}: unsupported policy schemaVersion {}",
            header.schema_version
        )));
    }
    header.metadata.validate_name()?;
    serde_yaml::from_str(yaml)
        .map_err(|error| OrbitError::InvalidInput(format!("failed to parse {label}: {error}")))
}
