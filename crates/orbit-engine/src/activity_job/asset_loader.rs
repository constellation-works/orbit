use serde::Deserialize as _;
use thiserror::Error;

use orbit_types::resource::ResourceKind;

use orbit_types::workflow::JobV2;
use orbit_types::workflow::SchemaHeader;
use orbit_types::workflow::activity_job::job_v2::AUTO_TASK_WAIT_KEYS;
use orbit_types::workflow::activity_job::{
    ASSET_SCHEMA_VERSION, AUTO_TASK_JOB_SCHEMA_VERSION, AUTO_TASK_REF_PREFIX, ActivityV2,
    ActivityV2Spec, TrustedHostActivityError, validate_trusted_host_activity,
};

fn parse_schema_header(yaml: &str) -> Result<SchemaHeader, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}
use orbit_types::workflow::{
    ToolAllowlistError, activity_tool_policy_deprecation, validate_activity_tool_allowlist,
};

/// Loaded schemaVersion 2 activity asset plus its envelope metadata.
#[derive(Debug, Clone)]
pub struct ActivityAsset {
    pub name: String,
    pub spec: ActivityV2,
}

/// Loaded job asset plus its envelope metadata: schemaVersion 2, or 3 for a
/// job using the auto-task step extension.
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
    #[error("schemaVersion {version} parse failed: {source}")]
    Parse {
        version: u32,
        source: serde_yaml::Error,
    },
    /// A `schemaVersion: 2` job names a key of the auto-task step extension,
    /// even with a null value or in a nested step, where a binary that
    /// predates the extension would drop it.
    #[error(
        "job `{job}` sets `{key}`, which needs `schemaVersion: 3` so that an older Orbit refuses the file instead of dropping it"
    )]
    AutoTaskNeedsSchemaVersion { job: String, key: &'static str },
    #[error(
        "{asset_kind} `{asset}` declares retired `role`; remove it and pass `crew` or `crew_config_key` in the activity input to select a non-default crew (activities without either use the run's resolved crew)"
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

/// Activity-asset loader for schemaVersion 2 assets.
pub fn load_activity_asset(yaml: &str) -> Result<ActivityAsset, AssetLoadError> {
    let res: V2EnvelopeYaml<ActivityV2> =
        load_envelope(yaml, &[ASSET_SCHEMA_VERSION], reject_activity_role)?;
    require_kind(&res.kind, ResourceKind::Activity)?;
    validate_activity_tool_allowlist(&res.spec).map_err(|source| {
        AssetLoadError::ToolAllowlist {
            activity: res.metadata.name.clone(),
            source,
        }
    })?;
    if let Some(deprecation) = activity_tool_policy_deprecation(&res.spec) {
        tracing::warn!(activity = %res.metadata.name, "activity {deprecation}");
    }
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

/// Job-asset loader. A job using the auto-task step extension must declare
/// schemaVersion 3; a schemaVersion 2 job naming any of its keys, null or
/// nested, is refused ([ORB-20237]). The declared version is carried on
/// [`JobV2::schema_version`], never inferred from the remaining fields.
pub fn load_job_asset(yaml: &str) -> Result<JobAsset, AssetLoadError> {
    let mut res: V2EnvelopeYaml<JobV2> = load_envelope(
        yaml,
        &[ASSET_SCHEMA_VERSION, AUTO_TASK_JOB_SCHEMA_VERSION],
        check_job_document,
    )?;
    require_kind(&res.kind, ResourceKind::Job)?;
    res.spec.schema_version = res.schema_version;
    Ok(JobAsset {
        name: res.metadata.name,
        spec: res.spec,
    })
}

/// Checks a parsed document that declares the given `schemaVersion` for keys
/// its typed read would not refuse: the retired `role`, and for jobs the
/// auto-task keys a schemaVersion 2 file may not name.
type CheckDocument = fn(&serde_yaml::Value, u32) -> Result<(), AssetLoadError>;

/// Read the envelope of an asset whose header names one of `versions`: its
/// header, the retired-role check, and the typed body.
///
/// Every catalog walk loads every asset, so the text is parsed into one
/// untyped document that serves all three steps instead of three times.
/// Typed reads out of an untyped document are stricter than typed reads out of
/// text (a number-like scalar is already a number by then), so a document the
/// single pass cannot read is handed to [`load_envelope_in_passes`], which owns
/// every error message and source position.
fn load_envelope<T: serde::de::DeserializeOwned>(
    yaml: &str,
    versions: &[u32],
    check_document: CheckDocument,
) -> Result<V2EnvelopeYaml<T>, AssetLoadError> {
    load_envelope_single_pass(yaml, versions, check_document)
        .unwrap_or_else(|| load_envelope_in_passes(yaml, versions, check_document))
}

fn admit_schema_version(version: u32, versions: &[u32]) -> Result<(), AssetLoadError> {
    match version {
        1 => Err(AssetLoadError::RetiredVersion(1)),
        version if versions.contains(&version) => Ok(()),
        other => Err(AssetLoadError::UnsupportedVersion(other)),
    }
}

/// `None` when the document needs [`load_envelope_in_passes`] to be judged.
fn load_envelope_single_pass<T: serde::de::DeserializeOwned>(
    yaml: &str,
    versions: &[u32],
    check_document: CheckDocument,
) -> Option<Result<V2EnvelopeYaml<T>, AssetLoadError>> {
    let document: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
    let header = SchemaHeader::deserialize(&document).ok()?;
    if let Err(error) = admit_schema_version(header.schema_version, versions) {
        return Some(Err(error));
    }
    if let Err(error) = check_document(&document, header.schema_version) {
        return Some(Err(error));
    }
    V2EnvelopeYaml::<T>::deserialize(&document).ok().map(Ok)
}

/// One typed parse per step, straight from the text.
fn load_envelope_in_passes<T: serde::de::DeserializeOwned>(
    yaml: &str,
    versions: &[u32],
    check_document: CheckDocument,
) -> Result<V2EnvelopeYaml<T>, AssetLoadError> {
    let version = parse_schema_header(yaml)
        .map_err(AssetLoadError::HeaderParse)?
        .schema_version;
    admit_schema_version(version, versions)?;
    let parse = |source| AssetLoadError::Parse { version, source };
    let document: serde_yaml::Value = serde_yaml::from_str(yaml).map_err(parse)?;
    check_document(&document, version)?;
    serde_yaml::from_str(yaml).map_err(parse)
}

fn reject_activity_role(document: &serde_yaml::Value, _version: u32) -> Result<(), AssetLoadError> {
    let Some(spec) = field(document, "spec") else {
        return Ok(());
    };
    if has_field(spec, "role") {
        return Err(retired_role_error("activity", document));
    }
    Ok(())
}

fn check_job_document(document: &serde_yaml::Value, version: u32) -> Result<(), AssetLoadError> {
    let spec = field(document, "spec");
    let steps = spec
        .and_then(|spec| field(spec, "steps"))
        .and_then(serde_yaml::Value::as_sequence)
        .map(Vec::as_slice)
        .unwrap_or_default();
    for step in steps {
        reject_step_roles(step, document)?;
    }
    if version == ASSET_SCHEMA_VERSION {
        let job_key = spec
            .filter(|spec| has_field(spec, "expire_when"))
            .map(|_| "expire_when");
        if let Some(key) = job_key.or_else(|| steps.iter().find_map(auto_task_key)) {
            return Err(AssetLoadError::AutoTaskNeedsSchemaVersion {
                job: asset_name(document),
                key,
            });
        }
    }
    Ok(())
}

/// The first auto-task step key `step` or a step nested in it names, by key
/// presence alone: a null value counts, because a binary that predates the
/// extension cannot tell it from a set one.
fn auto_task_key(step: &serde_yaml::Value) -> Option<&'static str> {
    let is_auto_task_target = field(step, "target")
        .and_then(serde_yaml::Value::as_str)
        .is_some_and(|target| target.starts_with(AUTO_TASK_REF_PREFIX));
    if is_auto_task_target {
        return Some("target: auto-task:<name>");
    }
    if let Some(key) = AUTO_TASK_WAIT_KEYS
        .into_iter()
        .find(|key| has_field(step, key))
    {
        return Some(key);
    }
    nested_steps(step).find_map(auto_task_key)
}

/// The steps a `parallel`, `fan_out` or `loop` body nests directly.
fn nested_steps(step: &serde_yaml::Value) -> impl Iterator<Item = &serde_yaml::Value> {
    let sequence = |block: &str, key: &str| {
        field(step, block)
            .and_then(|body| field(body, key))
            .and_then(serde_yaml::Value::as_sequence)
            .map(Vec::as_slice)
            .unwrap_or_default()
    };
    let worker = field(step, "fan_out").and_then(|fan_out| field(fan_out, "worker"));
    sequence("parallel", "branches")
        .iter()
        .chain(worker)
        .chain(sequence("loop", "steps"))
}

fn reject_step_roles(
    step: &serde_yaml::Value,
    document: &serde_yaml::Value,
) -> Result<(), AssetLoadError> {
    if has_field(step, "role") || field(step, "spec").is_some_and(|spec| has_field(spec, "role")) {
        return Err(retired_role_error("job", document));
    }

    for nested in nested_steps(step) {
        reject_step_roles(nested, document)?;
    }
    Ok(())
}

fn retired_role_error(asset_kind: &'static str, document: &serde_yaml::Value) -> AssetLoadError {
    AssetLoadError::RetiredRole {
        asset_kind,
        asset: asset_name(document),
    }
}

fn asset_name(document: &serde_yaml::Value) -> String {
    field(document, "metadata")
        .and_then(|metadata| field(metadata, "name"))
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or("<unnamed>")
        .to_string()
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
    schema_version: u32,
    kind: ResourceKind,
    metadata: orbit_types::resource::ResourceMetadata,
    spec: T,
}
