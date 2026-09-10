use thiserror::Error;

use orbit_types::resource::ResourceKind;

use orbit_types::workflow::JobV2;
use orbit_types::workflow::SchemaHeader;
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, TrustedHostActivityError, validate_trusted_host_activity,
};

fn parse_schema_header(yaml: &str) -> Result<SchemaHeader, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}
use orbit_types::workflow::{ToolAllowlistError, validate_activity_tool_allowlist};

/// Loaded schemaVersion 2 activity asset plus its envelope metadata.
#[derive(Debug, Clone)]
pub struct ActivityAsset {
    pub name: String,
    pub spec: ActivityV2,
}

/// Loaded schemaVersion 2 job asset plus its envelope metadata.
#[derive(Debug, Clone)]
pub struct JobAsset {
    pub name: String,
    pub spec: JobV2,
}

#[derive(Debug, Error)]
pub enum AssetLoadError {
    #[error("failed to parse schema header: {0}")]
    HeaderParse(serde_yaml::Error),
    #[error("schemaVersion {0} assets were retired; migrate this asset to schemaVersion 2")]
    RetiredVersion(u32),
    #[error("unsupported schemaVersion: {0}")]
    UnsupportedVersion(u32),
    #[error("schemaVersion 2 parse failed: {0}")]
    Parse(serde_yaml::Error),
    #[error(
        "{asset_kind} `{asset}` declares retired `role`; remove it and pass `crew` in the activity input to select a non-default crew (activities without `crew` use the run's resolved crew)"
    )]
    RetiredRole {
        asset_kind: &'static str,
        asset: String,
    },
    #[error("kind mismatch: expected `{expected}`, got `{actual}`")]
    KindMismatch { expected: String, actual: String },
    // Both asset-validation refusals below name the offending activity, and
    // they stay in step. An activity's `metadata.name` identifies a workspace
    // file the operator can open and fix, so naming it is what makes the
    // refusal actionable; redaction here is reserved for credentials and
    // user-identifying paths. Redacting one arm alone only costs
    // diagnosability, because the other still reports the same value.
    #[error("activity `{activity}` tool allowlist invalid: {source}")]
    ToolAllowlist {
        activity: String,
        source: ToolAllowlistError,
    },
    #[error(transparent)]
    TrustedHostActivity(#[from] TrustedHostActivityError),
}

/// Two-pass activity-asset loader for schemaVersion 2 assets.
pub fn load_activity_asset(yaml: &str) -> Result<ActivityAsset, AssetLoadError> {
    let header = parse_schema_header(yaml).map_err(AssetLoadError::HeaderParse)?;
    match header.schema_version {
        1 => Err(AssetLoadError::RetiredVersion(1)),
        2 => {
            let document: serde_yaml::Value =
                serde_yaml::from_str(yaml).map_err(AssetLoadError::Parse)?;
            reject_activity_role(&document)?;
            let res: V2EnvelopeYaml<ActivityV2> =
                serde_yaml::from_str(yaml).map_err(AssetLoadError::Parse)?;
            require_kind(&res.kind, ResourceKind::Activity)?;
            validate_activity_tool_allowlist(&res.spec).map_err(|source| {
                AssetLoadError::ToolAllowlist {
                    activity: res.metadata.name.clone(),
                    source,
                }
            })?;
            // The unsandboxed execution mode is legal on exactly one built-in
            // activity name, so an edited or hand-written asset cannot claim
            // it. [ORB-11354]
            validate_trusted_host_activity(
                &res.metadata.name,
                matches!(&res.spec.spec, ActivityV2Spec::AgentLoop(spec) if spec.trusted_host_execution),
            )?;
            Ok(ActivityAsset {
                name: res.metadata.name,
                spec: res.spec,
            })
        }
        other => Err(AssetLoadError::UnsupportedVersion(other)),
    }
}

/// Two-pass job-asset loader for schemaVersion 2 assets.
pub fn load_job_asset(yaml: &str) -> Result<JobAsset, AssetLoadError> {
    let header = parse_schema_header(yaml).map_err(AssetLoadError::HeaderParse)?;
    match header.schema_version {
        1 => Err(AssetLoadError::RetiredVersion(1)),
        2 => {
            let document: serde_yaml::Value =
                serde_yaml::from_str(yaml).map_err(AssetLoadError::Parse)?;
            reject_job_roles(&document)?;
            let res: V2EnvelopeYaml<JobV2> =
                serde_yaml::from_str(yaml).map_err(AssetLoadError::Parse)?;
            require_kind(&res.kind, ResourceKind::Job)?;
            Ok(JobAsset {
                name: res.metadata.name,
                spec: res.spec,
            })
        }
        other => Err(AssetLoadError::UnsupportedVersion(other)),
    }
}

fn reject_activity_role(document: &serde_yaml::Value) -> Result<(), AssetLoadError> {
    let Some(spec) = field(document, "spec") else {
        return Ok(());
    };
    if has_field(spec, "role") {
        return Err(retired_role_error("activity", document));
    }
    Ok(())
}

fn reject_job_roles(document: &serde_yaml::Value) -> Result<(), AssetLoadError> {
    let Some(steps) = field(document, "spec")
        .and_then(|spec| field(spec, "steps"))
        .and_then(serde_yaml::Value::as_sequence)
    else {
        return Ok(());
    };
    for step in steps {
        reject_step_roles(step, document)?;
    }
    Ok(())
}

fn reject_step_roles(
    step: &serde_yaml::Value,
    document: &serde_yaml::Value,
) -> Result<(), AssetLoadError> {
    if has_field(step, "role") || field(step, "spec").is_some_and(|spec| has_field(spec, "role")) {
        return Err(retired_role_error("job", document));
    }

    if let Some(branches) = field(step, "parallel")
        .and_then(|parallel| field(parallel, "branches"))
        .and_then(serde_yaml::Value::as_sequence)
    {
        for branch in branches {
            reject_step_roles(branch, document)?;
        }
    }
    if let Some(worker) = field(step, "fan_out").and_then(|fan_out| field(fan_out, "worker")) {
        reject_step_roles(worker, document)?;
    }
    if let Some(steps) = field(step, "loop")
        .and_then(|loop_block| field(loop_block, "steps"))
        .and_then(serde_yaml::Value::as_sequence)
    {
        for nested in steps {
            reject_step_roles(nested, document)?;
        }
    }
    Ok(())
}

fn retired_role_error(asset_kind: &'static str, document: &serde_yaml::Value) -> AssetLoadError {
    let asset = field(document, "metadata")
        .and_then(|metadata| field(metadata, "name"))
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or("<unnamed>")
        .to_string();
    AssetLoadError::RetiredRole { asset_kind, asset }
}

fn has_field(value: &serde_yaml::Value, name: &str) -> bool {
    field(value, name).is_some()
}

fn field<'a>(value: &'a serde_yaml::Value, name: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()?
        .get(serde_yaml::Value::String(name.to_string()))
}

fn require_kind(actual: &ResourceKind, expected: ResourceKind) -> Result<(), AssetLoadError> {
    if actual == &expected {
        Ok(())
    } else {
        Err(AssetLoadError::KindMismatch {
            expected: expected.to_string(),
            actual: actual.to_string(),
        })
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
struct V2EnvelopeYaml<T> {
    #[serde(rename = "schemaVersion")]
    _schema_version: u32,
    kind: ResourceKind,
    metadata: orbit_types::resource::ResourceMetadata,
    spec: T,
}
